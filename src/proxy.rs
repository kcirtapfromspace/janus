//! The local LiteLLM gateway that brokers ic's LLM requests (see litellm/).
//!
//! `ic proxy setup` writes the compose file, config, and a generated `.env` into
//! ~/InterviewCoach/litellm/, starts the containers, and creates a LiteLLM virtual key for ic (a
//! local credential between ic and the proxy, generated automatically). Claude needs no key here:
//! ic sends your browser-login token, which LiteLLM forwards. Only OpenAI, which has no browser
//! login for its API, needs a key in that `.env`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Provider, Settings};

const COMPOSE: &str = include_str!("../litellm/docker-compose.yml");
const CONFIG: &str = include_str!("../litellm/config.yaml");
const DEFAULT_PORT: &str = "4000";

/// Where ic sends LLM requests, and the LiteLLM virtual key it authenticates with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmEndpoint {
    pub base_url: String,
    pub api_key: String,
}

impl LlmEndpoint {
    /// `IC_LLM_URL` + `IC_LLM_KEY` override the saved endpoint (e.g. to use a shared proxy).
    pub fn load(settings: &Settings) -> Option<Self> {
        if let (Ok(base_url), Ok(api_key)) = (std::env::var("IC_LLM_URL"), std::env::var("IC_LLM_KEY")) {
            return Some(LlmEndpoint { base_url, api_key });
        }
        serde_json::from_str(&std::fs::read_to_string(endpoint_path(settings)).ok()?).ok()
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        write_private(&endpoint_path(settings), &serde_json::to_string_pretty(self)?)
    }
}

fn endpoint_path(settings: &Settings) -> PathBuf {
    settings.data_dir.join("llm.json")
}

pub fn dir(settings: &Settings) -> PathBuf {
    settings.data_dir.join("litellm")
}

fn project() -> String {
    std::env::var("IC_PROXY_PROJECT").unwrap_or_else(|_| "interview-coach-llm".into())
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

// --- .env ------------------------------------------------------------------------------------

pub fn env_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string()).filter(|v| !v.is_empty())
    })
}

/// Set `key` to `value`, replacing an existing line or appending a new one.
pub fn set_env_value(contents: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut lines: Vec<String> = contents
        .lines()
        .map(|line| match line.split_once('=') {
            Some((k, _)) if k.trim() == key => {
                found = true;
                format!("{key}={value}")
            }
            _ => line.to_string(),
        })
        .collect();
    if !found {
        lines.push(format!("{key}={value}"));
    }
    lines.join("\n") + "\n"
}

/// Add any missing secrets. Existing values are never changed: LITELLM_SALT_KEY encrypts stored
/// credentials, and Postgres only reads its password when the volume is first created.
pub fn fill_env(contents: &str, port: &str) -> Result<String> {
    let mut out = contents.to_string();
    let defaults = [
        ("LITELLM_MASTER_KEY", format!("sk-{}", random_hex(24)?)),
        ("LITELLM_SALT_KEY", format!("sk-{}", random_hex(24)?)),
        ("POSTGRES_PASSWORD", random_hex(24)?),
        ("LITELLM_PORT", port.to_string()),
    ];
    for (key, value) in defaults {
        if env_value(&out, key).is_none() {
            out = set_env_value(&out, key, &value);
        }
    }
    Ok(out)
}

fn read_env(settings: &Settings) -> String {
    std::fs::read_to_string(dir(settings).join(".env")).unwrap_or_default()
}

pub fn has_provider_key(settings: &Settings, provider: Provider) -> bool {
    env_value(&read_env(settings), provider.key_var()).is_some()
}

/// An Anthropic key in the proxy would be injected alongside your login token, and Anthropic
/// rejects requests that carry both. ic never writes one; `ic doctor` warns if one appears.
pub fn has_stray_anthropic_key(settings: &Settings) -> bool {
    has_provider_key(settings, Provider::Anthropic)
}

/// Whether ic is pointed at a proxy other than the local one `ic proxy setup` manages.
pub fn using_external_proxy() -> bool {
    std::env::var_os("IC_LLM_URL").is_some()
}

pub fn base_url(settings: &Settings) -> String {
    let port = env_value(&read_env(settings), "LITELLM_PORT").unwrap_or_else(|| DEFAULT_PORT.into());
    format!("http://127.0.0.1:{port}")
}

/// Write the compose file, config, and `.env` (filling in missing secrets; setting a provider key
/// only when one is given).
pub fn write_files(settings: &Settings, provider_key: Option<(Provider, &str)>) -> Result<()> {
    if let Some((Provider::Anthropic, _)) = provider_key {
        bail!("Claude uses your browser login, not an API key — run: ic login");
    }
    let d = dir(settings);
    std::fs::create_dir_all(&d)?;
    std::fs::write(d.join("docker-compose.yml"), COMPOSE)?;
    std::fs::write(d.join("config.yaml"), CONFIG)?;
    let port = std::env::var("IC_PROXY_PORT").unwrap_or_else(|_| DEFAULT_PORT.into());
    let mut env = fill_env(&read_env(settings), &port)?;
    if let Some((provider, key)) = provider_key {
        env = set_env_value(&env, provider.key_var(), key);
    }
    write_private(&d.join(".env"), &env)
}

// --- docker ------------------------------------------------------------------------------------

pub fn docker_running() -> bool {
    Command::new("docker").args(["info", "--format", "{{.ServerVersion}}"]).output().is_ok_and(|o| o.status.success())
}

fn compose(settings: &Settings, args: &[&str]) -> Result<()> {
    let d = dir(settings);
    if !d.join("docker-compose.yml").exists() {
        bail!("The LLM proxy isn't set up yet. Run: ic proxy setup");
    }
    let status = Command::new("docker")
        .args(["compose", "-p", &project(), "--project-directory"])
        .arg(&d)
        .arg("-f")
        .arg(d.join("docker-compose.yml"))
        .args(args)
        .status()
        .context("running docker compose")?;
    if !status.success() {
        bail!("docker compose {} failed", args.join(" "));
    }
    Ok(())
}

pub fn start(settings: &Settings) -> Result<()> {
    compose(settings, &["up", "-d"])
}

/// Recreate the LiteLLM container so a changed `.env` (e.g. a new provider key) takes effect.
/// Postgres is left running; it only reads its settings on first start.
pub fn restart_with_new_env(settings: &Settings) -> Result<()> {
    compose(settings, &["up", "-d", "--force-recreate", "litellm"])
}

pub fn stop(settings: &Settings) -> Result<()> {
    compose(settings, &["down"])
}

// --- LiteLLM HTTP API ------------------------------------------------------------------------

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().expect("HTTP client")
}

/// `/health/readiness` once the proxy is up and connected to its database.
pub fn readiness(base_url: &str) -> Result<Value> {
    let resp = http().get(format!("{base_url}/health/readiness")).send()?;
    if !resp.status().is_success() {
        bail!("proxy not ready (HTTP {})", resp.status());
    }
    Ok(resp.json()?)
}

/// First start pulls images and runs database migrations, which can take a few minutes.
pub fn wait_ready(base_url: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match readiness(base_url) {
            Ok(v) if v["db"] == "connected" => return Ok(()),
            _ if Instant::now() > deadline => {
                bail!("The LLM proxy didn't become ready within {}s. See: docker compose -p {} logs litellm",
                      timeout.as_secs(), project())
            }
            _ => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

fn create_key(base_url: &str, master_key: &str) -> Result<String> {
    let resp = http()
        .post(format!("{base_url}/key/generate"))
        .bearer_auth(master_key)
        .json(&json!({
            "key_alias": format!("interview-coach-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S")),
            "metadata": {"app": "interview-coach"},
        }))
        .send()?;
    let status = resp.status();
    let body: Value = resp.json().unwrap_or_default();
    match body["key"].as_str() {
        Some(key) if status.is_success() => Ok(key.to_string()),
        _ => bail!("LiteLLM didn't create a key (HTTP {status}): {body}"),
    }
}

/// The virtual key's own info (includes `spend`), or an error if the proxy rejects it.
pub fn key_info(endpoint: &LlmEndpoint) -> Result<Value> {
    let resp = http().get(format!("{}/key/info", endpoint.base_url)).bearer_auth(&endpoint.api_key).send()?;
    let status = resp.status();
    let body: Value = resp.json().unwrap_or_default();
    if !status.is_success() {
        bail!("LiteLLM rejected ic's key (HTTP {status})");
    }
    Ok(body["info"].clone())
}

/// Reuse ic's saved key if the proxy still accepts it; otherwise mint and save a new one.
pub fn ensure_key(settings: &Settings) -> Result<LlmEndpoint> {
    let base_url = base_url(settings);
    if let Some(existing) = LlmEndpoint::load(settings).filter(|e| e.base_url == base_url)
        && key_info(&existing).is_ok()
    {
        return Ok(existing);
    }
    let master = env_value(&read_env(settings), "LITELLM_MASTER_KEY").context("LITELLM_MASTER_KEY missing from .env")?;
    let endpoint = LlmEndpoint { api_key: create_key(&base_url, &master)?, base_url };
    endpoint.save(settings)?;
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_env_adds_missing_secrets_but_never_changes_existing_ones() {
        let first = fill_env("", "4000").unwrap();
        let salt = env_value(&first, "LITELLM_SALT_KEY").unwrap();
        assert!(salt.starts_with("sk-") && salt.len() == 51);
        assert_eq!(env_value(&first, "LITELLM_PORT").as_deref(), Some("4000"));

        let with_key = set_env_value(&first, "ANTHROPIC_API_KEY", "sk-ant-1");
        let again = fill_env(&with_key, "4001").unwrap();
        assert_eq!(again, with_key, "re-running setup must not touch existing values");
        assert_eq!(env_value(&again, "LITELLM_SALT_KEY").unwrap(), salt);
    }

    #[test]
    fn set_env_value_replaces_in_place() {
        let env = "A=1\nANTHROPIC_API_KEY=old\nB=2\n";
        let out = set_env_value(env, "ANTHROPIC_API_KEY", "new");
        assert_eq!(out, "A=1\nANTHROPIC_API_KEY=new\nB=2\n");
        assert_eq!(env_value("X=\n", "X"), None, "empty values count as missing");
    }

    #[test]
    fn templates_bind_the_proxy_to_loopback_only() {
        assert!(COMPOSE.contains("\"127.0.0.1:${LITELLM_PORT:-4000}:4000\""));
        assert!(!COMPOSE.contains("5432:5432"), "Postgres must not be published");
        assert!(CONFIG.contains("os.environ/LITELLM_MASTER_KEY"));
    }
}
