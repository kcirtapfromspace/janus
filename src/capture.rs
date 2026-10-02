//! Drive ICRecorder.app (mac/): launch it via LaunchServices, stop it, and read its report.
//!
//! Launching with `open -a` matters: macOS then asks permission for ICRecorder itself. Running the
//! binary directly would attribute the permission to whatever terminal started ic.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub fn app_path() -> PathBuf {
    if let Some(p) = std::env::var_os("IC_RECORDER_APP") {
        return PathBuf::from(p);
    }
    // Next to the ic binary first (a packaged install), then the source tree (a dev build).
    let beside_exe = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("ICRecorder.app")));
    match beside_exe {
        Some(p) if p.exists() => p,
        _ => Path::new(env!("CARGO_MANIFEST_DIR")).join("mac/build/ICRecorder.app"),
    }
}

fn pid(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("recorder.pid")).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // Signal 0 checks existence; EPERM still means the process exists.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

pub fn is_running(dir: &Path) -> bool {
    pid(dir).is_some_and(alive)
}

pub fn launch(dir: &Path, duration: Option<u32>, aec: bool) -> Result<()> {
    let app = app_path();
    if !app.exists() {
        bail!("Recorder app not found at {}. Build it with: mac/build.sh", app.display());
    }
    let dir = std::fs::canonicalize(dir)?;
    let mut cmd = Command::new("open");
    cmd.args(["-n", "-a"]).arg(&app).args(["--args", "--session-dir"]).arg(&dir);
    if let Some(d) = duration {
        cmd.args(["--duration", &d.to_string()]);
    }
    if aec {
        cmd.arg("--aec");
    }
    let status = cmd.status().context("running `open`")?;
    if !status.success() {
        bail!("Couldn't launch {}", app.display());
    }
    Ok(())
}

/// Wait for the pid file. The generous timeout covers a first-run microphone permission prompt.
pub fn wait_started(dir: &Path, timeout: Duration) -> Result<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(p) = pid(dir) {
            return Ok(p);
        }
        if let Some(report) = read_report(dir) {
            let errors = describe_errors(&report);
            bail!("{}", if errors.is_empty() { "The recorder exited before it started recording.".into() } else { errors });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("The recorder didn't start within {}s. See {}", timeout.as_secs(), dir.join("recorder.log").display())
}

/// The executable path of a running process, e.g. ".../ICRecorder.app/Contents/MacOS/ICRecorder".
fn process_name(pid: i32) -> Option<String> {
    let out = Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// Stop the CLI recorder (ICRecorder.app) recording into `dir`. Never signals any other process:
/// the Interview Coach app records in-process, so its sessions are stopped from the app.
pub fn stop(dir: &Path) -> Result<()> {
    let Some(p) = pid(dir).filter(|&p| alive(p)) else { return Ok(()) };
    let name = process_name(p).unwrap_or_default();
    if !name.ends_with("/ICRecorder") && name != "ICRecorder" {
        let app = name.rsplit('/').next().unwrap_or("another app");
        bail!("this session is being recorded by {app} (pid {p}) — stop it there");
    }
    unsafe { libc::kill(p, libc::SIGINT) };
    Ok(())
}

/// Wait for the recorder to finalize its WAVs; returns recorder.json.
pub fn wait_finished(dir: &Path, timeout: Duration) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_running(dir)
            && let Some(report) = read_report(dir)
        {
            return Ok(report);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("The recorder didn't finish within {}s. See {}", timeout.as_secs(), dir.join("recorder.log").display())
}

pub fn read_report(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("recorder.json")).ok()?).ok()
}

fn describe_errors(report: &Value) -> String {
    report["errors"]
        .as_array()
        .map(|errs| {
            errs.iter()
                .map(|e| e["message"].as_str().or(e["code"].as_str()).unwrap_or_default().to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// Human-readable problems from recorder.json, with fixes for the ones we know.
pub fn report_warnings(report: &Value) -> Vec<String> {
    let mut out: Vec<String> = report["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| match w["code"].as_str() {
            Some("system_silent") => "The interviewer track is silent. Allow the recording app (Interview Coach or ICRecorder) under System Settings > Privacy & \
                Security > Screen & System Audio Recording > System Audio Recording Only."
                .to_string(),
            Some("mic_silent") => "Your mic track is silent. Check the input device in System Settings > Sound, and \
                Microphone permission for the recording app."
                .to_string(),
            _ => w["message"].as_str().unwrap_or_default().to_string(),
        })
        .filter(|w| !w.is_empty())
        .collect();
    let errors = describe_errors(report);
    if !errors.is_empty() {
        out.push(format!("Recorder errors: {errors}"));
    }
    out
}
