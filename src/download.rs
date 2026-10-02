//! First-use model downloads (resumable only in the sense that a finished file is never re-fetched).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::progress::Progress;

/// Download `url` to `dest` unless it's already there. Writes to `.part` first so an interrupted
/// download is never mistaken for a complete one.
pub fn ensure_file(url: &str, dest: &Path, label: &str, progress: &mut dyn Progress) -> Result<PathBuf> {
    if dest.exists() {
        return Ok(dest.to_path_buf());
    }
    std::fs::create_dir_all(dest.parent().context("download destination has no parent")?)?;
    progress.stage(&format!("Downloading {label} (first run only)"));
    let client = reqwest::blocking::Client::builder().timeout(None).build()?;
    let mut resp = client.get(url).send()?.error_for_status().with_context(|| format!("downloading {url}"))?;
    let total = resp.content_length().unwrap_or(0);
    let part = dest.with_extension("part");
    let mut file = File::create(&part)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress.step(done, total);
    }
    file.sync_all()?;
    if total > 0 && done != total {
        bail!("download of {url} was incomplete ({done} of {total} bytes)");
    }
    std::fs::rename(&part, dest)?;
    Ok(dest.to_path_buf())
}

/// Download a .tar.bz2 and unpack it into `dir`, unless `marker` (a file it contains) already exists.
pub fn ensure_archive(url: &str, dir: &Path, marker: &Path, label: &str, progress: &mut dyn Progress) -> Result<()> {
    if marker.exists() {
        return Ok(());
    }
    let archive = dir.join(url.rsplit('/').next().unwrap_or("archive.tar.bz2"));
    ensure_file(url, &archive, label, progress)?;
    let status = Command::new("tar").arg("xjf").arg(&archive).arg("-C").arg(dir).status()?;
    if !status.success() {
        bail!("couldn't unpack {}", archive.display());
    }
    std::fs::remove_file(&archive)?;
    if !marker.exists() {
        bail!("{} didn't contain {}", url, marker.display());
    }
    Ok(())
}
