//! Audio plumbing: normalize and decode with ffmpeg, measure loudness.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::config::which;

pub const SR: u32 = 16_000;

/// A track this quiet is digital silence — almost always a capture/permission failure, not a quiet room.
pub const SILENCE_DBFS: f64 = -70.0;

fn ffmpeg() -> Result<std::path::PathBuf> {
    which("ffmpeg").context("ffmpeg not found. Install it with: brew install ffmpeg")
}

/// Decode any audio/video file to 16 kHz mono FLAC (small on disk, lossless for Whisper).
pub fn normalize(src: &Path, dst: &Path) -> Result<()> {
    let out = Command::new(ffmpeg()?)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "1", "-ar", &SR.to_string(), "-c:a", "flac"])
        .arg(dst)
        .output()?;
    if !out.status.success() {
        bail!("ffmpeg could not decode {}:\n{}", src.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Decode a file to 16 kHz mono f32 samples.
pub fn load(path: &Path) -> Result<Vec<f32>> {
    let out = Command::new(ffmpeg()?)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", &SR.to_string(), "-f", "f32le", "-"])
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        bail!("ffmpeg could not read {}:\n{}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

pub fn duration_s(samples: &[f32]) -> f64 {
    samples.len() as f64 / SR as f64
}

pub fn rms_dbfs(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mean_sq = samples.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / samples.len() as f64;
    if mean_sq > 0.0 { 10.0 * mean_sq.log10() } else { f64::NEG_INFINITY }
}

pub fn is_silent(samples: &[f32]) -> bool {
    rms_dbfs(samples) < SILENCE_DBFS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_detection() {
        assert!(is_silent(&vec![0.0; 16_000]));
        let tone: Vec<f32> = (0..16_000).map(|i| 0.1 * (i as f32 * 0.125).sin()).collect();
        assert!(!is_silent(&tone));
        let db = rms_dbfs(&tone);
        assert!((-24.0..-22.0).contains(&db), "{db}");
    }
}
