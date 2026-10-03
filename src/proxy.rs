//! The local LiteLLM gateway that brokers ic's LLM requests (see litellm/).
//!
//! Setup (the app's Setup window, or `ic proxy setup`) writes the compose file, config, and a
//! generated `.env` into ~/InterviewCoach/litellm/, starts the containers, and creates a LiteLLM
//! virtual key for ic (a local credential between ic and the proxy, generated automatically).
//! Claude needs no key here: ic sends your browser-login token, which LiteLLM forwards. OpenAI and
//! TypeSafe (Jev), which have no browser login for their APIs, keep their keys in that `.env`.
//!
//! Docker doesn't have to be running beforehand: `ensure_running` starts Docker and the proxy
//! before any step that needs them.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Provider, Settings};
use crate::progress::Progress;
use crate::tools::Tool;

/// An API key the proxy holds for a service with no browser login. Claude isn't one: it uses your
/// login, so there's no way to put an Anthropic key here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum KeyTarget {
    #[value(name = "openai")]
    OpenAi,
    /// TypeSafe AI, for Jev.
    #[value(name = "typesafe")]
    TypeSafe,
}

impl KeyTarget {
    pub const ALL: [KeyTarget; 2] = [KeyTarget::OpenAi, KeyTarget::TypeSafe];

    pub fn as_str(self) -> &'static str {
        match self {
            KeyTarget::OpenAi => "openai",
            KeyTarget::TypeSafe => "typesafe",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            KeyTarget::OpenAi => "OpenAI",
            KeyTarget::TypeSafe => "TypeSafe (Jev)",
        }
    }

    /// The variable LiteLLM reads the key from.
    pub fn env_var(self) -> &'static str {
        match self {
            KeyTarget::OpenAi => "OPENAI_API_KEY",
            KeyTarget::TypeSafe => "TYPESAFE_API_KEY",
        }
    }

    pub fn keys_url(self) -> &'static str {
        match self {
            KeyTarget::OpenAi => "https://platform.openai.com/api-keys",
            KeyTarget::TypeSafe => "https://console.typesafe.ai/",
        }
    }

    /// A light sanity check, so a pasted sentence or a Claude key isn't stored by mistake.
    pub fn check(self, key: &str) -> Result<(), String> {
        if key.is_empty() || key.chars().any(char::is_whitespace) || key.len() < 20 {
            return Err(format!("That doesn't look like a {} API key.", self.label()));
        }
        if key.starts_with("sk-ant-") {
            return Err("That's an Anthropic key. Claude uses your browser sign-in instead, so no key is needed.".into());
        }
        if self == KeyTarget::OpenAi && !key.starts_with("sk-") {
            return Err("That doesn't look like an OpenAI API key (they start with sk-).".into());
        }
        Ok(())
    }
}

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

pub fn has_key(settings: &Settings, target: KeyTarget) -> bool {
    env_value(&read_env(settings), target.env_var()).is_some()
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

/// Write the compose file, config, and `.env` (filling in missing secrets; setting a key only when
/// one is given).
pub fn write_files(settings: &Settings, key: Option<(KeyTarget, &str)>) -> Result<()> {
    let d = dir(settings);
    std::fs::create_dir_all(&d)?;
    std::fs::write(d.join("docker-compose.yml"), COMPOSE)?;
    std::fs::write(d.join("config.yaml"), CONFIG)?;
    let port = std::env::var("IC_PROXY_PORT").unwrap_or_else(|_| DEFAULT_PORT.into());
    let mut env = fill_env(&read_env(settings), &port)?;
    if let Some((target, key)) = key {
        env = set_env_value(&env, target.env_var(), key);
    }
    write_private(&d.join(".env"), &env)
}

// --- docker ------------------------------------------------------------------------------------

/// Docker Desktop or OrbStack, whichever is installed (a CLI alone isn't enough to start one).
fn docker_app() -> Option<String> {
    let home = dirs::home_dir().unwrap_or_default();
    [PathBuf::from("/Applications/Docker.app"), home.join("Applications/Docker.app"),
     PathBuf::from("/Applications/OrbStack.app"), home.join("Applications/OrbStack.app")]
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.display().to_string())
}

pub fn docker_installed() -> bool {
    Tool::Docker.find().is_some() || docker_app().is_some()
}

pub fn docker_running() -> bool {
    Tool::Docker
        .command()
        .is_ok_and(|mut c| c.args(["info", "--format", "{{.ServerVersion}}"]).output().is_ok_and(|o| o.status.success()))
}

/// Start Docker Desktop (or OrbStack) if it isn't running, and wait until it answers.
pub fn start_docker(progress: &mut dyn Progress) -> Result<()> {
    if docker_running() {
        return Ok(());
    }
    let app = docker_app().context(
        "Docker isn't installed. Get Docker Desktop from https://www.docker.com/products/docker-desktop/ (or install OrbStack).",
    )?;
    progress.stage("Starting Docker");
    let status = Command::new("/usr/bin/open").args(["-g", "-a", &app]).status()?;
    if !status.success() {
        bail!("Couldn't open {app}.");
    }
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        if docker_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    bail!("Docker didn't start within 3 minutes. Open {app} and finish anything it asks for (accepting its terms, \
           allowing its helper), then try again.")
}

fn compose_command(settings: &Settings, args: &[&str]) -> Result<Command> {
    let d = dir(settings);
    if !d.join("docker-compose.yml").exists() {
        bail!("The AI proxy isn't set up yet. Open Setup in the app (or run: ic proxy setup).");
    }
    let mut cmd = Tool::Docker.command()?;
    cmd.args(["compose", "-p", &project(), "--project-directory"]).arg(&d).arg("-f").arg(d.join("docker-compose.yml"));
    cmd.args(args);
    Ok(cmd)
}

fn compose(settings: &Settings, args: &[&str]) -> Result<()> {
    let out = compose_command(settings, args)?.stdin(Stdio::null()).output().context("running docker compose")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<_> = stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("docker compose {} failed:\n{}", args.join(" "), tail.join("\n"));
    }
    Ok(())
}

/// Byte progress across every image layer, from `docker compose --progress json pull` lines.
#[derive(Debug, Default)]
pub struct PullProgress {
    layers: std::collections::BTreeMap<String, (u64, u64)>,
    images_done: std::collections::BTreeSet<String>,
}

impl PullProgress {
    /// Take one output line; returns (bytes done, bytes total) once any layer reports sizes.
    pub fn feed(&mut self, line: &str) -> Option<(u64, u64)> {
        let event: Value = serde_json::from_str(line).ok()?;
        let id = event["id"].as_str()?.to_string();
        if event["parent_id"].is_null() {
            if event["status"] == "Done" {
                self.images_done.insert(id);
            }
            return None;
        }
        let (current, total) = (event["current"].as_u64().unwrap_or(0), event["total"].as_u64().unwrap_or(0));
        let entry = self.layers.entry(id).or_default();
        if total > 0 {
            *entry = (current.min(total), total);
        }
        if event["status"] == "Done" && entry.1 > 0 {
            entry.0 = entry.1;
        }
        let (done, total) = self.layers.values().fold((0, 0), |(d, t), (ld, lt)| (d + ld, t + lt));
        (total > 0).then_some((done, total))
    }

    pub fn images_done(&self) -> usize {
        self.images_done.len()
    }
}

/// Download the proxy's images (about 2 GB the first time), reporting byte progress.
pub fn pull(settings: &Settings, progress: &mut dyn Progress) -> Result<()> {
    progress.stage("Downloading the AI proxy (first time only)");
    let mut child = compose_command(settings, &["--progress", "json", "pull"])?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running docker compose pull")?;
    // Compose writes its JSON progress to stderr; stdout is read too so neither pipe fills up.
    let stdout = child.stdout.take().expect("piped");
    let drain = std::thread::spawn(move || std::io::copy(&mut BufReader::new(stdout), &mut std::io::sink()));
    let mut state = PullProgress::default();
    let mut errors = vec![];
    for line in BufReader::new(child.stderr.take().expect("piped")).lines().map_while(Result::ok) {
        if let Some((done, total)) = state.feed(&line) {
            progress.step(done, total);
        } else if !line.trim_start().starts_with('{') {
            errors.push(line);
        }
    }
    let _ = drain.join();
    if !child.wait()?.success() {
        bail!("Couldn't download the AI proxy's images:\n{}", errors.join("\n"));
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

/// Whether the proxy answers and accepts ic's key.
pub fn is_ready(settings: &Settings) -> bool {
    LlmEndpoint::load(settings).is_some_and(|e| readiness(&e.base_url).is_ok() && key_info(&e).is_ok())
}

/// Set up (or repair) the local proxy from scratch: Docker, config, images, containers, ic's key.
pub fn setup(settings: &Settings, progress: &mut dyn Progress) -> Result<LlmEndpoint> {
    start_docker(progress)?;
    write_files(settings, None)?;
    pull(settings, progress)?;
    progress.stage("Starting the AI proxy (the first start sets up its database)");
    compose(settings, &["up", "-d"])?;
    wait_ready(&base_url(settings), Duration::from_secs(300))?;
    ensure_key(settings)
}

/// Make sure the proxy is up before an AI step: start Docker and the containers if they stopped
/// (e.g. after a restart). Does nothing for an external proxy (`IC_LLM_URL`), and never sets one up
/// from scratch — that's Setup's job.
pub fn ensure_running(settings: &Settings, progress: &mut dyn Progress) -> Result<()> {
    if using_external_proxy() || is_ready(settings) {
        return Ok(());
    }
    if !dir(settings).join("docker-compose.yml").exists() {
        bail!("The AI proxy isn't set up yet. Open Setup in the app (or run: ic proxy setup).");
    }
    start_docker(progress)?;
    progress.stage("Starting the AI proxy");
    write_files(settings, None)?; // refreshes the compose file after an app update; keys are kept
    compose(settings, &["up", "-d"])?;
    wait_ready(&base_url(settings), Duration::from_secs(300))?;
    ensure_key(settings)?;
    Ok(())
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
    fn pull_progress_adds_up_layers_across_images() {
        let mut p = PullProgress::default();
        let lines = [
            r#"{"id":"Image postgres:16","status":"Working","text":"Pulling"}"#,
            r#"{"id":"a1","parent_id":"Image postgres:16","status":"Working","text":"Downloading","current":100,"total":1000}"#,
            r#"{"id":"b2","parent_id":"Image litellm","status":"Working","text":"Downloading","current":50,"total":500}"#,
            r#"{"id":"a1","parent_id":"Image postgres:16","status":"Done","text":"Pull complete"}"#,
            r#"{"id":"Image postgres:16","status":"Done","text":"Pulled"}"#,
        ];
        let seen: Vec<_> = lines.iter().map(|l| p.feed(l)).collect();
        assert_eq!(seen, [None, Some((100, 1000)), Some((150, 1500)), Some((1050, 1500)), None]);
        assert_eq!(p.images_done(), 1);
        assert_eq!(p.feed("not json"), None);
    }

    #[test]
    fn keys_are_sanity_checked_and_claude_keys_refused() {
        assert!(KeyTarget::OpenAi.check("sk-proj-0123456789abcdefghij").is_ok());
        assert!(KeyTarget::OpenAi.check("nope").is_err());
        assert!(KeyTarget::TypeSafe.check("ts_live_0123456789abcdefghij").is_ok());
        assert!(KeyTarget::TypeSafe.check("sk-ant-api03-0123456789abcdefghij").unwrap_err().contains("browser sign-in"));
        assert!(KeyTarget::TypeSafe.check("two words here and more words").is_err());
    }

    #[test]
    fn templates_bind_the_proxy_to_loopback_only() {
        assert!(COMPOSE.contains("\"127.0.0.1:${LITELLM_PORT:-4000}:4000\""));
        assert!(!COMPOSE.contains("5432:5432"), "Postgres must not be published");
        assert!(CONFIG.contains("os.environ/LITELLM_MASTER_KEY"));
    }
}
