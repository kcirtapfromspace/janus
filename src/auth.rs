//! Claude sign-in through the browser — no API key to copy or store.
//!
//! `ic login` runs Anthropic's CLI (`ant auth login`), which opens the browser so you can approve
//! access, then keeps an OAuth session (short-lived access token + refresh token) in its own
//! credential store. Before each Claude request ic asks `ant` for a fresh access token and sends it
//! through the LiteLLM proxy, which forwards it to Anthropic. The session is saved under a dedicated
//! "interview-coach" profile, so it never changes which account other tools (e.g. Claude Code) use.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::which;

pub const PROFILE: &str = "interview-coach";
pub const INSTALL_ANT: &str = "brew tap anthropics/tap && brew install anthropics/tap/ant";

fn config_dir() -> PathBuf {
    std::env::var_os("ANTHROPIC_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config/anthropic"))
}

fn ant() -> Result<PathBuf> {
    which("ant").with_context(|| format!("Anthropic's CLI isn't installed. Install it with:\n  {INSTALL_ANT}"))
}

/// Whether a Claude login session exists (it may still have expired; `access_token` will tell).
pub fn has_login() -> bool {
    config_dir().join("credentials").join(format!("{PROFILE}.json")).exists()
}

/// A fresh access token from the login session (`ant` refreshes it when it's near expiry).
pub fn access_token() -> Result<String> {
    let out = Command::new(ant()?)
        .args(["auth", "print-credentials", "--access-token"])
        .env("ANTHROPIC_PROFILE", PROFILE)
        .env_remove("ANTHROPIC_API_KEY")
        .output()?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        bail!("your Claude login is missing or has expired — run: ic login");
    }
    Ok(token)
}

/// Open the browser so you can approve access, and save the session as ic's own profile.
pub fn login() -> Result<()> {
    let pointer = config_dir().join("active_config");
    let had_pointer = pointer.exists();
    let status = Command::new(ant()?).args(["auth", "login", "--profile", PROFILE]).status()?;
    if !status.success() {
        bail!("Claude sign-in didn't complete (approval wasn't given within 5 minutes, or it was cancelled).");
    }
    if !had_pointer && pointer.exists() {
        // Our login became the machine-wide default; undo that so it stays private to ic.
        std::fs::remove_file(&pointer)?;
    }
    Ok(())
}
