//! Model downloads: pinned and checksum-verified, resumable, and checked for disk space first.
//!
//! Files download to `<dest>.part` and are renamed into place only once their SHA-256 matches, so a
//! half-finished or tampered file is never used. An interrupted download picks up where it stopped
//! (HTTP Range); a finished file is checked by size, not re-hashed on every run.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::progress::Progress;

/// Something to download, with its checksum and size when they're pinned.
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub url: String,
    pub sha256: Option<&'static str>,
    pub size: Option<u64>,
}

impl Asset {
    pub fn pinned(url: impl Into<String>, sha256: &'static str, size: u64) -> Self {
        Asset { url: url.into(), sha256: Some(sha256), size: Some(size) }
    }

    pub fn unpinned(url: impl Into<String>) -> Self {
        Asset { url: url.into(), sha256: None, size: None }
    }

    /// Whether `path` holds this asset already (judged by size when the size is pinned).
    pub fn is_present(&self, path: &Path) -> bool {
        match (std::fs::metadata(path), self.size) {
            (Ok(meta), Some(size)) => meta.len() == size,
            (Ok(_), None) => true,
            (Err(_), _) => false,
        }
    }
}

/// Bytes free on the volume holding `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0).then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

fn sha256_of(path: &Path, hasher: &mut Sha256) -> Result<u64> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download `asset` to `dest` unless it's already there.
pub fn ensure_file(asset: &Asset, dest: &Path, label: &str, progress: &mut dyn Progress) -> Result<PathBuf> {
    if asset.is_present(dest) {
        return Ok(dest.to_path_buf());
    }
    let dir = dest.parent().context("download destination has no parent")?;
    std::fs::create_dir_all(dir)?;
    let part = part_path(dest);
    // A complete file from before sizes were pinned, or a corrupted one: start over.
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if let (Some(size), Some(free)) = (asset.size, free_space(dir)) {
        let needed = size.saturating_sub(have) + 200 * 1024 * 1024;
        if free < needed {
            bail!("Not enough disk space for the {label}: it needs {:.1} GB free, and {:.1} GB is available.",
                  needed as f64 / 1e9, free as f64 / 1e9);
        }
    }
    progress.stage(&format!("Downloading the {label}"));
    let mut attempt = 0;
    loop {
        match download_into(asset, &part, progress) {
            Ok(()) => break,
            Err(e) if attempt < 3 && e.downcast_ref::<Retryable>().is_some() => {
                attempt += 1;
                std::thread::sleep(Duration::from_secs(2 * attempt));
            }
            Err(e) => return Err(e.context(format!("downloading the {label} from {}", asset.url))),
        }
    }
    verify(asset, &part, label)?;
    std::fs::rename(&part, dest)?;
    Ok(dest.to_path_buf())
}

/// A network failure worth retrying (the next attempt resumes from what's on disk).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Retryable(String);

fn download_into(asset: &Asset, part: &Path, progress: &mut dyn Progress) -> Result<()> {
    let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if asset.size.is_some_and(|size| have >= size) {
        return Ok(()); // already all there; verify() decides whether it's right
    }
    let client = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(15)).timeout(None).build()?;
    let mut request = client.get(&asset.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = request.send().map_err(|e| Retryable(e.to_string()))?;
    let status = resp.status();
    let (mut file, mut done) = match status.as_u16() {
        206 => (OpenOptions::new().append(true).open(part)?, have),
        200 => (File::create(part)?, 0),
        416 => return Ok(()), // nothing past what we have; verify() checks it
        code if code >= 500 => return Err(Retryable(format!("HTTP {status}")).into()),
        _ => bail!("HTTP {status}"),
    };
    let total = asset.size.or_else(|| resp.content_length().map(|n| n + done)).unwrap_or(0);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = resp.read(&mut buf).map_err(|e| Retryable(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress.step(done, total);
    }
    file.sync_all()?;
    if total > 0 && done < total {
        return Err(Retryable(format!("the connection closed after {done} of {total} bytes")).into());
    }
    Ok(())
}

fn verify(asset: &Asset, part: &Path, label: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    let len = sha256_of(part, &mut hasher)?;
    let wrong_size = asset.size.is_some_and(|size| size != len);
    let wrong_hash = asset.sha256.is_some_and(|want| hex(&hasher.finalize()) != want);
    if wrong_size || wrong_hash {
        std::fs::remove_file(part)?;
        bail!("The downloaded {label} didn't match its pinned checksum, so it was deleted. Try again; if it keeps \
               happening, the download is being altered on its way to this Mac.");
    }
    Ok(())
}

/// Download a .tar.bz2 and unpack it into `dir`, unless `marker` (a file it contains) already exists.
pub fn ensure_archive(asset: &Asset, dir: &Path, marker: &Path, label: &str, progress: &mut dyn Progress) -> Result<()> {
    if marker.exists() {
        return Ok(());
    }
    let archive = dir.join(asset.url.rsplit('/').next().unwrap_or("archive.tar.bz2"));
    ensure_file(asset, &archive, label, progress)?;
    let status = Command::new("/usr/bin/tar").arg("xjf").arg(&archive).arg("-C").arg(dir).status()?;
    if !status.success() {
        bail!("couldn't unpack {}", archive.display());
    }
    std::fs::remove_file(&archive)?;
    if !marker.exists() {
        bail!("{} didn't contain {}", asset.url, marker.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::Quiet;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    const BODY: &[u8] = b"interview coach model bytes: 0123456789abcdefghijklmnopqrstuvwxyz";

    fn body_sha() -> &'static str {
        Box::leak(hex(&Sha256::digest(BODY)).into_boxed_str())
    }

    /// A tiny HTTP server that honours `Range: bytes=N-` and closes the first full response early.
    fn serve(cut_first_response_at: Option<usize>, requests: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut ranges = vec![];
            for (i, stream) in listener.incoming().take(requests).enumerate() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                ranges.push(range.map(|r| r.to_string()).unwrap_or_default());
                let start = range.unwrap_or(0);
                let (status, slice) = if start >= BODY.len() {
                    ("416 Range Not Satisfiable", &BODY[0..0])
                } else if start > 0 {
                    ("206 Partial Content", &BODY[start..])
                } else {
                    ("200 OK", BODY)
                };
                let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len());
                stream.write_all(header.as_bytes()).unwrap();
                let send = if i == 0 { cut_first_response_at.unwrap_or(slice.len()).min(slice.len()) } else { slice.len() };
                stream.write_all(&slice[..send]).unwrap();
            }
            ranges
        });
        (url, handle)
    }

    #[test]
    fn an_interrupted_download_resumes_and_is_verified() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve(Some(20), 2);
        let asset = Asset { url, sha256: Some(body_sha()), size: Some(BODY.len() as u64) };
        let dest = dir.path().join("model.bin");
        ensure_file(&asset, &dest, "test model", &mut Quiet).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), BODY);
        assert_eq!(server.join().unwrap(), ["", "20"], "the second request resumes at byte 20");
        assert!(!part_path(&dest).exists());
        // Already there: no request at all (the server has stopped listening).
        ensure_file(&asset, &dest, "test model", &mut Quiet).unwrap();
    }

    #[test]
    fn a_complete_part_file_is_verified_without_downloading_again() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");
        std::fs::write(part_path(&dest), BODY).unwrap();
        // Nothing listens here: a request would fail. The pinned size says the part is complete.
        let asset = Asset { url: "http://127.0.0.1:9/model.bin".into(), sha256: Some(body_sha()), size: Some(BODY.len() as u64) };
        ensure_file(&asset, &dest, "test model", &mut Quiet).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    }

    #[test]
    fn a_checksum_mismatch_is_rejected_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve(None, 1);
        let asset = Asset { url, sha256: Some("00".repeat(32).leak()), size: Some(BODY.len() as u64) };
        let dest = dir.path().join("model.bin");
        let err = ensure_file(&asset, &dest, "test model", &mut Quiet).unwrap_err();
        assert!(format!("{err:#}").contains("didn't match its pinned checksum"), "{err:#}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        server.join().unwrap();
    }

    #[test]
    fn a_server_with_nothing_past_the_part_answers_416_and_the_part_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve(None, 1);
        let dest = dir.path().join("model.bin");
        std::fs::write(part_path(&dest), BODY).unwrap();
        // Size unknown, so ic asks the server for the rest and gets 416.
        let asset = Asset { url, sha256: Some(body_sha()), size: None };
        ensure_file(&asset, &dest, "test model", &mut Quiet).unwrap();
        assert_eq!(server.join().unwrap(), [BODY.len().to_string()]);
        assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    }

    #[test]
    fn a_wrong_sized_existing_file_is_downloaded_again() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = serve(None, 1);
        let dest = dir.path().join("model.bin");
        std::fs::write(&dest, b"truncated").unwrap();
        let asset = Asset { url, sha256: Some(body_sha()), size: Some(BODY.len() as u64) };
        ensure_file(&asset, &dest, "test model", &mut Quiet).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), BODY);
        server.join().unwrap();
    }

    #[test]
    fn free_space_is_reported() {
        assert!(free_space(Path::new("/")).is_some_and(|b| b > 0));
    }
}
