//! Claude sign-in through the browser — no API key to copy or store.
//!
//! `ic login` runs Anthropic's CLI (`ant auth login`, bundled with the app), which opens the browser
//! so you can approve access, then keeps an OAuth session (short-lived access token + refresh token)
//! in its own credential store. Before each Claude request ic asks `ant` for a fresh access token
//! and sends it through the LiteLLM proxy, which forwards it to Anthropic. The session is saved
//! under a dedicated "interview-coach" profile, so it never changes which account other tools
//! (e.g. Claude Code) use.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::tools::Tool;

pub const PROFILE: &str = "interview-coach";

fn config_dir() -> PathBuf {
    std::env::var_os("ANTHROPIC_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config/anthropic"))
}

/// Whether a Claude login session exists (it may still have expired; `access_token` will tell).
pub fn has_login() -> bool {
    config_dir().join("credentials").join(format!("{PROFILE}.json")).exists()
}

/// Who the sign-in belongs to: only these two fields are read from the credentials file.
#[derive(Debug, Deserialize, PartialEq)]
struct Identity {
    account_email: Option<String>,
    organization_name: Option<String>,
}

fn describe(credentials_json: &str) -> Option<String> {
    let id: Identity = serde_json::from_str(credentials_json).ok()?;
    match (id.account_email, id.organization_name) {
        (Some(email), Some(org)) => Some(format!("{email} ({org})")),
        (Some(email), None) => Some(email),
        (None, Some(org)) => Some(org),
        (None, None) => None,
    }
}

/// The signed-in account, e.g. "you@example.com (Your Organization)", when ant saved it.
pub fn account() -> Option<String> {
    describe(&std::fs::read_to_string(config_dir().join("credentials").join(format!("{PROFILE}.json"))).ok()?)
}

/// The profile remembers the organization (and workspace) it was first signed in to, and `ant`
/// asks the sign-in page for that same organization next time: an account outside it then gets
/// "You don't have access to this organization". Without a signed-in session there's nothing to
/// keep it for, so it's forgotten and the page lets you choose.
fn forget_organization(config_dir: &std::path::Path) -> Result<()> {
    let credentials = config_dir.join("credentials").join(format!("{PROFILE}.json"));
    let config = config_dir.join("configs").join(format!("{PROFILE}.json"));
    if !credentials.exists() && config.exists() {
        std::fs::remove_file(config)?;
    }
    Ok(())
}

/// Sign out of ic's own profile. Other profiles, and which one other tools use, are untouched.
pub fn logout() -> Result<()> {
    if !has_login() {
        return forget_organization(&config_dir());
    }
    let out = Command::new(Tool::Ant.require()?)
        .args(["auth", "logout", "--profile", PROFILE])
        .env_remove("ANTHROPIC_API_KEY")
        .output()?;
    if !out.status.success() || has_login() {
        bail!("couldn't sign out of Claude: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    forget_organization(&config_dir())
}

/// A fresh access token from the login session (`ant` refreshes it when it's near expiry).
pub fn access_token() -> Result<String> {
    let out = Command::new(Tool::Ant.require()?)
        .args(["auth", "print-credentials", "--access-token"])
        .env("ANTHROPIC_PROFILE", PROFILE)
        .env_remove("ANTHROPIC_API_KEY")
        .output()?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        bail!("your Claude login is missing or has expired — sign in again (Setup in the app, or: ic login)");
    }
    Ok(token)
}

/// What happens during a sign-in, for an app to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// The page to approve access on (the browser normally opens it by itself).
    OpenUrl(String),
    /// The page showed a code instead of finishing on its own; send it as a line on stdin.
    NeedCode,
    Log(String),
}

/// Classify one line of `ant auth login` output.
pub fn parse_login_line(line: &str) -> Option<LoginEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(url) = line.split_whitespace().find(|w| w.starts_with("https://") && w.contains("/oauth/authorize")) {
        return Some(LoginEvent::OpenUrl(url.to_string()));
    }
    if line.contains("Paste it here") || line.starts_with("Code:") {
        return Some(LoginEvent::NeedCode);
    }
    Some(LoginEvent::Log(line.to_string()))
}

/// Open the browser so you can approve access, and save the session as ic's own profile.
///
/// Without `on_event`, `ant` talks to the terminal directly. With it, `ant`'s output is turned into
/// events instead (for the app); stdin stays connected, so a code typed or sent there reaches `ant`.
pub fn login(on_event: Option<&mut dyn FnMut(LoginEvent)>) -> Result<()> {
    // A profile left from an earlier account would send the page to that account's organization.
    forget_organization(&config_dir())?;
    let pointer = config_dir().join("active_config");
    let had_pointer = pointer.exists();
    let mut cmd = Command::new(Tool::Ant.require()?);
    cmd.args(["auth", "login", "--profile", PROFILE]).env_remove("ANTHROPIC_API_KEY");
    let status = match on_event {
        None => cmd.status()?,
        Some(on_event) => {
            let mut child = cmd.stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
            // The app cancels by terminating ic; take `ant` down too, or it keeps waiting for the
            // browser for up to 5 minutes. (Fails harmlessly if a handler is already installed.)
            let pid = child.id() as libc::pid_t;
            let _ = ctrlc::set_handler(move || {
                unsafe { libc::kill(pid, libc::SIGTERM) };
                std::process::exit(130);
            });
            let (tx, rx) = mpsc::channel::<String>();
            let readers: Vec<_> = [
                child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
                child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            ]
            .into_iter()
            .flatten()
            .map(|pipe| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                        let _ = tx.send(line);
                    }
                })
            })
            .collect();
            drop(tx);
            for line in rx {
                if let Some(event) = parse_login_line(&line) {
                    on_event(event);
                }
            }
            for reader in readers {
                let _ = reader.join();
            }
            child.wait()?
        }
    };
    if !status.success() {
        bail!("Claude sign-in didn't complete: approval wasn't given within 5 minutes, it was cancelled, or the \
               organization you chose doesn't allow it. Choose an organization you can use the Claude Console with; a \
               Team or Enterprise organization may need its admin to allow the Anthropic CLI.");
    }
    if !had_pointer && pointer.exists() {
        // Our login became the machine-wide default; undo that so it stays private to ic.
        std::fs::remove_file(&pointer)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed out, the remembered organization goes too, so the next sign-in lets you choose one.
    /// Signed in, it stays (ant uses it for that session).
    #[test]
    fn the_organization_is_forgotten_only_without_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("configs").join(format!("{PROFILE}.json"));
        let credentials = dir.path().join("credentials").join(format!("{PROFILE}.json"));
        let other = dir.path().join("configs").join("other.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::create_dir_all(credentials.parent().unwrap()).unwrap();
        for f in [&config, &credentials, &other] {
            std::fs::write(f, "{}").unwrap();
        }
        forget_organization(dir.path()).unwrap();
        assert!(config.exists(), "kept while signed in");
        std::fs::remove_file(&credentials).unwrap();
        forget_organization(dir.path()).unwrap();
        assert!(!config.exists(), "forgotten once signed out");
        assert!(other.exists(), "other profiles are untouched");
        forget_organization(dir.path()).unwrap();
    }

    /// The account shown in Setup comes from the credentials file's non-secret fields.
    #[test]
    fn the_account_is_read_without_the_tokens() {
        let file = r#"{"version":"1.0","type":"oauth_token","access_token":"sk-ant-oat01-x","refresh_token":"r",
                      "account_email":"you@example.com","organization_name":"Your Org"}"#;
        assert_eq!(describe(file).as_deref(), Some("you@example.com (Your Org)"));
        assert_eq!(describe(r#"{"account_email":"you@example.com"}"#).as_deref(), Some("you@example.com"));
        assert_eq!(describe(r#"{"access_token":"x"}"#), None);
        assert_eq!(describe("not json"), None);
    }

    /// What `ant auth login --no-browser` actually prints (from ant 1.36, URL shortened).
    #[test]
    fn ant_output_becomes_events() {
        let lines = [
            "Creating profile \"interview-coach\".",
            "Open this URL to authorize:",
            "",
            "  https://platform.claude.com/oauth/authorize?client_id=41077d10&response_type=code&state=x",
            "After authorizing, the page will display a code. Paste it here:",
        ];
        let events: Vec<_> = lines.iter().filter_map(|l| parse_login_line(l)).collect();
        assert_eq!(events, vec![
            LoginEvent::Log("Creating profile \"interview-coach\".".into()),
            LoginEvent::Log("Open this URL to authorize:".into()),
            LoginEvent::OpenUrl("https://platform.claude.com/oauth/authorize?client_id=41077d10&response_type=code&state=x".into()),
            LoginEvent::NeedCode,
        ]);
    }
}
