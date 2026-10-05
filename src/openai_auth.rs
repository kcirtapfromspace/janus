//! Sign in with ChatGPT using the documented public-client OAuth/OIDC flow.
//! Credentials are private, atomically replaced, and never included in status or errors.
//! A process lock serializes rotating refresh tokens and account changes.
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::RngCore;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::LoginEvent;
use crate::config::Settings;

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const JWKS: &str = "https://auth.openai.com/.well-known/jwks.json";
const RESOURCE: &str = "https://api.openai.com/v1";
const DYNAMIC: &str = "dynamic_agent_client";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    #[default]
    ChatGpt,
    ApiKey,
}

#[derive(Default, Deserialize, Serialize)]
struct Store {
    host_id: String,
    active: Option<String>,
    #[serde(default)]
    billing: Billing,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    accounts: Vec<Account>,
}

#[derive(Deserialize, Serialize)]
struct Account {
    client_id: String,
    subject: String,
    email: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Deserialize, Serialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    id_token: String,
    scopes: Vec<String>,
    expires_at: i64,
    earliest_refresh_at: Option<i64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: String,
    expires_in: i64,
    scope: Option<String>,
    earliest_refresh_at: Option<i64>,
}

#[derive(Deserialize, Serialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: serde_json::Value,
    exp: u64,
    nonce: Option<String>,
    email: Option<String>,
}

/// Public status contains identity and permissions only, never tokens.
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub active: Option<String>,
    pub email: Option<String>,
    pub signed_in: bool,
    pub plan_enabled: bool,
    pub api_key: bool,
    pub using_api_key: bool,
    pub accounts: Vec<AccountInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AccountInfo {
    pub client_id: String,
    pub label: String,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn directory(settings: &Settings) -> PathBuf {
    settings.data_dir.join("auth")
}
fn path(settings: &Settings) -> PathBuf {
    directory(settings).join("openai.json")
}

fn private_dir(settings: &Settings) -> Result<()> {
    std::fs::create_dir_all(directory(settings))?;
    std::fs::set_permissions(directory(settings), std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn lock(settings: &Settings) -> Result<File> {
    private_dir(settings)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory(settings).join("openai.lock"))?;
    file.try_lock_exclusive()
        .context("Another OpenAI sign-in or refresh is running. Try again when it finishes.")?;
    Ok(file)
}

fn load(settings: &Settings) -> Result<Store> {
    match std::fs::read(path(settings)) {
        Ok(data) => serde_json::from_slice(&data).map_err(|_| {
            anyhow::anyhow!(
                "OpenAI credentials could not be read; restore a backup or sign in again"
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(e.into()),
    }
}

fn save(settings: &Settings, store: &Store) -> Result<()> {
    private_dir(settings)?;
    let mut temp = tempfile::NamedTempFile::new_in(directory(settings))?;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(temp.as_file_mut(), store)?;
    temp.as_file().sync_all()?;
    temp.persist(path(settings)).map_err(|e| e.error)?;
    File::open(directory(settings))?.sync_all()?;
    Ok(())
}

fn token_usable(tokens: &Tokens) -> bool {
    tokens.expires_at > now() + 60 || tokens.refresh_token.is_some()
}

pub fn status(settings: &Settings) -> Result<Status> {
    let store = load(settings)?;
    let account = store
        .accounts
        .iter()
        .find(|a| Some(&a.client_id) == store.active.as_ref());
    let tokens = account.and_then(|a| a.tokens.as_ref());
    Ok(Status {
        active: store.active,
        email: account.and_then(|a| a.email.clone()),
        signed_in: tokens.is_some_and(token_usable),
        plan_enabled: tokens
            .is_some_and(|t| token_usable(t) && t.scopes.iter().any(|s| s == PLAN_SCOPE)),
        api_key: store.api_key.is_some(),
        using_api_key: store.billing == Billing::ApiKey,
        accounts: store
            .accounts
            .iter()
            .map(|a| AccountInfo {
                client_id: a.client_id.clone(),
                label: format!(
                    "{} · {}",
                    a.email.as_deref().unwrap_or("ChatGPT account"),
                    a.client_id
                ),
            })
            .collect(),
    })
}

pub fn save_api_key(settings: &Settings, key: &str) -> Result<()> {
    crate::proxy::KeyTarget::OpenAi
        .check(key)
        .map_err(anyhow::Error::msg)?;
    let _lock = lock(settings)?;
    let mut store = load(settings)?;
    store.api_key = Some(key.to_string());
    // Choosing a key is an explicit billing choice; never silently bill a key after OAuth fails.
    store.billing = Billing::ApiKey;
    save(settings, &store)
}

pub fn api_key(settings: &Settings) -> Result<Option<String>> {
    Ok(load(settings)?.api_key)
}

fn http() -> Result<Client> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

fn random_value() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

struct Attempt {
    state: String,
    nonce: String,
    verifier: String,
    redirect: String,
    client_id: String,
}

impl Attempt {
    fn new(port: u16, client_id: String) -> Self {
        Self {
            state: random_value(),
            nonce: random_value(),
            verifier: random_value(),
            redirect: format!("http://127.0.0.1:{port}/auth/callback"),
            client_id,
        }
    }

    fn url(
        &self,
        host: &str,
        selected: Option<&Account>,
        enable_plan: bool,
    ) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(AUTHORIZE)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        let mut query = url.query_pairs_mut();
        query.extend_pairs([
            ("client_id", self.client_id.as_str()),
            ("ext_agent_host_id", host),
            ("response_type", "code"),
            ("redirect_uri", self.redirect.as_str()),
            ("scope", SCOPES),
            ("resource", RESOURCE),
            ("state", self.state.as_str()),
            ("nonce", self.nonce.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
        ]);
        if self.client_id == DYNAMIC {
            query.append_pair("agent_name_hint", "Janus");
        }
        if let Some(account) = selected {
            if let Some(tokens) = &account.tokens {
                query.append_pair("id_token_hint", &tokens.id_token);
            }
            if let Some(email) = &account.email {
                query.append_pair("login_hint", email);
            }
        }
        if enable_plan {
            query.append_pair("prompt", "consent");
        }
        drop(query);
        Ok(url)
    }

    fn callback(&self, target: &str) -> Result<Option<(String, String)>> {
        let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))?;
        if url.path() != "/auth/callback" {
            return Ok(None);
        }
        let mut params = HashMap::new();
        for (key, value) in url.query_pairs() {
            ensure!(
                params.insert(key.to_string(), value.to_string()).is_none(),
                "Duplicate sign-in parameter"
            );
        }
        // Ignore unrelated browser requests rather than consuming the pending transaction.
        if params.get("state") != Some(&self.state) {
            return Ok(None);
        }
        if params.contains_key("error") {
            bail!(
                "ChatGPT sign-in was declined or could not be completed. Your existing account is unchanged."
            );
        }
        let code = params
            .get("code")
            .filter(|v| !v.is_empty())
            .context("The sign-in callback contained no code")?;
        let client = match (self.client_id.as_str(), params.get("client_id")) {
            (DYNAMIC, Some(id)) if id.starts_with("oaiapp_") => id.clone(),
            (DYNAMIC, _) => {
                bail!("ChatGPT did not return an issued client ID; registration is incomplete")
            }
            (existing, Some(id)) if id != existing => {
                bail!("ChatGPT returned a different client registration")
            }
            (existing, _) => existing.to_string(),
        };
        Ok(Some((code.clone(), client)))
    }
}

fn browser_response(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Janus</title><p>{message}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn receive(listener: &TcpListener, attempt: &Attempt) -> Result<(String, String)> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut line = String::new();
                if BufReader::new((&stream).take(8192))
                    .read_line(&mut line)
                    .is_err()
                {
                    browser_response(
                        &mut stream,
                        "400 Bad Request",
                        "This request could not be read.",
                    );
                    continue;
                }
                let mut parts = line.split_whitespace();
                let target = match (parts.next(), parts.next()) {
                    (Some("GET"), Some(target))
                        if target.starts_with('/') && !target.starts_with("//") =>
                    {
                        target
                    }
                    _ => {
                        browser_response(&mut stream, "400 Bad Request", "Invalid request.");
                        continue;
                    }
                };
                match attempt.callback(target) {
                    Ok(Some(result)) => {
                        browser_response(
                            &mut stream,
                            "200 OK",
                            "Approval received. Return to Janus to finish sign-in.",
                        );
                        return Ok(result);
                    }
                    Ok(None) => browser_response(
                        &mut stream,
                        "400 Bad Request",
                        "This request does not match the pending sign-in.",
                    ),
                    Err(e) => {
                        browser_response(
                            &mut stream,
                            "400 Bad Request",
                            "Sign-in could not be completed. Return to Janus.",
                        );
                        return Err(e);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(e) => return Err(e.into()),
        }
    }
    bail!("ChatGPT sign-in timed out. Choose Continue with ChatGPT to try again.")
}

fn validate_identity(
    token: &str,
    client_id: &str,
    nonce: Option<&str>,
    keys: &JwkSet,
) -> Result<Claims> {
    let header = decode_header(token).context("The ChatGPT identity token is invalid")?;
    ensure!(
        matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::ES256
                | Algorithm::ES384
        ),
        "Unsupported ChatGPT identity signature"
    );
    let key_id = header
        .kid
        .as_deref()
        .context("The identity token has no signing key ID")?;
    let key = keys
        .find(key_id)
        .context("The identity signing key is not published by OpenAI")?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.validate_nbf = true;
    validation.leeway = 0;
    let claims = decode::<Claims>(token, &DecodingKey::from_jwk(key)?, &validation)
        .context("ChatGPT identity verification failed")?
        .claims;
    ensure!(
        !claims.sub.is_empty(),
        "ChatGPT returned an empty account identity"
    );
    if let Some(nonce) = nonce {
        ensure!(
            claims.nonce.as_deref() == Some(nonce),
            "The ChatGPT sign-in nonce did not match"
        );
    }
    Ok(claims)
}

fn post_tokens(http: &Client, endpoint: &str, form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = http
        .post(endpoint)
        .form(form)
        .send()
        .context("Could not reach ChatGPT sign-in; your saved account is unchanged")?;
    if !resp.status().is_success() {
        // Do not echo the response body: it can include credentials or request parameters.
        let status = resp.status();
        let value: serde_json::Value = resp.json().unwrap_or_default();
        let code = value["error"]
            .as_str()
            .or_else(|| value["error"]["code"].as_str())
            .unwrap_or("");
        if matches!(
            code,
            "invalid_grant"
                | "invalid_refresh_token"
                | "token_expired"
                | "refresh_token_expired"
                | "refresh_token_invalidated"
                | "refresh_token_reused"
        ) {
            return Err(TerminalRefresh.into());
        }
        bail!("ChatGPT authentication failed (HTTP {status}). Try again later or sign in again.");
    }
    resp.json()
        .context("ChatGPT returned an invalid token response")
}

#[derive(Debug, thiserror::Error)]
#[error("Your ChatGPT session has ended. Sign in again (ic login --provider openai).")]
struct TerminalRefresh;

fn tokens(response: TokenResponse, previous: Option<&Tokens>, id_token: String) -> Result<Tokens> {
    ensure!(
        response.token_type.eq_ignore_ascii_case("bearer")
            && !response.access_token.is_empty()
            && response.expires_in > 0,
        "ChatGPT returned unusable credentials"
    );
    let expires_at = now()
        .checked_add(response.expires_in)
        .context("Invalid ChatGPT token expiry")?;
    Ok(Tokens {
        access_token: response.access_token,
        refresh_token: response
            .refresh_token
            .or_else(|| previous.and_then(|t| t.refresh_token.clone())),
        id_token,
        scopes: response
            .scope
            .map(|s| s.split_whitespace().map(String::from).collect())
            .or_else(|| previous.map(|t| t.scopes.clone()))
            .unwrap_or_default(),
        expires_at,
        earliest_refresh_at: response.earliest_refresh_at,
    })
}

/// Reauthorize a saved registration or add a new account without discarding the current one.
pub fn login(
    settings: &Settings,
    new_account: bool,
    account_id: Option<&str>,
    enable_plan: bool,
    on_event: &mut dyn FnMut(LoginEvent),
) -> Result<bool> {
    let _lock = lock(settings)?;
    let mut store = load(settings)?;
    if store.host_id.is_empty() {
        store.host_id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
        save(settings, &store)?;
    }
    let selected_id = if new_account {
        None
    } else {
        account_id.or(store.active.as_deref())
    };
    let selected = selected_id
        .map(|id| {
            store
                .accounts
                .iter()
                .find(|a| a.client_id == id)
                .context("This ChatGPT account registration is not saved")
        })
        .transpose()?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("Could not start the local ChatGPT sign-in callback")?;
    let attempt = Attempt::new(
        listener.local_addr()?.port(),
        selected
            .map(|a| a.client_id.clone())
            .unwrap_or_else(|| DYNAMIC.into()),
    );
    let url = attempt.url(&store.host_id, selected, enable_plan)?;
    // Retained ID tokens in reauthorization URLs must never enter event logs or support output.
    if selected.and_then(|a| a.tokens.as_ref()).is_none() {
        on_event(LoginEvent::OpenUrl(url.to_string()));
    }
    std::process::Command::new("open")
        .arg(url.as_str())
        .status()
        .context("Could not open your browser")?
        .success()
        .then_some(())
        .context("Could not open your browser; try signing in again")?;
    on_event(LoginEvent::Log(
        "Approve access in your browser, then return here.".into(),
    ));
    let (code, issued_client) = receive(&listener, &attempt)?;
    let http = http()?;
    let response = post_tokens(
        &http,
        TOKEN,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &issued_client),
            ("code", &code),
            ("code_verifier", &attempt.verifier),
            ("redirect_uri", &attempt.redirect),
            ("resource", RESOURCE),
        ],
    )?;
    let id_token = response
        .id_token
        .clone()
        .context("ChatGPT returned no identity token")?;
    let keys: JwkSet = http.get(JWKS).send()?.error_for_status()?.json()?;
    let claims = validate_identity(&id_token, &issued_client, Some(&attempt.nonce), &keys)?;
    if let Some(selected) = selected {
        ensure!(
            selected.subject == claims.sub,
            "Sign-in returned a different ChatGPT account. Add it as a new account instead."
        );
    }
    if let Some(existing) = store.accounts.iter().find(|a| a.client_id == issued_client) {
        ensure!(
            existing.subject == claims.sub,
            "The saved client belongs to another account"
        );
    }
    let account = Account {
        client_id: issued_client.clone(),
        subject: claims.sub,
        email: claims.email,
        tokens: Some(tokens(response, None, id_token)?),
    };
    let plan = account
        .tokens
        .as_ref()
        .is_some_and(|t| t.scopes.iter().any(|s| s == PLAN_SCOPE));
    store.accounts.retain(|a| a.client_id != issued_client);
    store.accounts.push(account);
    store.active = Some(issued_client);
    store.billing = Billing::ChatGpt;
    save(settings, &store)?;
    Ok(plan)
}

/// Read a fresh token for each request. Refreshes are bounded and serialized across processes.
pub fn access_token(settings: &Settings, force_refresh: bool) -> Result<String> {
    refresh_access_token(settings, force_refresh, TOKEN, JWKS)
}

fn refresh_access_token(
    settings: &Settings,
    force_refresh: bool,
    token_endpoint: &str,
    jwks_endpoint: &str,
) -> Result<String> {
    let _lock = lock(settings)?;
    let mut store = load(settings)?;
    let account = store
        .accounts
        .iter_mut()
        .find(|a| Some(&a.client_id) == store.active.as_ref())
        .context("Sign in with ChatGPT first (ic login --provider openai)")?;
    let previous = account
        .tokens
        .as_ref()
        .context("Sign in with ChatGPT again (ic login --provider openai)")?;
    ensure!(
        previous.scopes.iter().any(|s| s == PLAN_SCOPE),
        "ChatGPT plan usage is not enabled. Enable it in Setup or explicitly choose an API key."
    );
    if !force_refresh && previous.expires_at > now() + 60 {
        return Ok(previous.access_token.clone());
    }
    if let Some(earliest) = previous.earliest_refresh_at {
        ensure!(
            now() >= earliest,
            "ChatGPT access cannot be refreshed yet. Try again later."
        );
    }
    let refresh = previous
        .refresh_token
        .as_deref()
        .context("ChatGPT sign-in expired. Sign in again (ic login --provider openai)")?;
    let http = http()?;
    let response = match post_tokens(
        &http,
        token_endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &account.client_id),
            ("refresh_token", refresh),
            ("resource", RESOURCE),
        ],
    ) {
        Ok(r) => r,
        Err(e) if e.is::<TerminalRefresh>() => {
            account.tokens = None;
            save(settings, &store)?;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    let id_token = response
        .id_token
        .clone()
        .unwrap_or_else(|| previous.id_token.clone());
    if response.id_token.is_some() {
        let keys: JwkSet = http.get(jwks_endpoint).send()?.error_for_status()?.json()?;
        let claims = validate_identity(&id_token, &account.client_id, None, &keys)?;
        ensure!(
            claims.sub == account.subject,
            "ChatGPT refresh returned a different account identity"
        );
    }
    let replacement = tokens(response, Some(previous), id_token)?;
    let plan = replacement.scopes.iter().any(|s| s == PLAN_SCOPE);
    let access = replacement.access_token.clone();
    account.tokens = Some(replacement);
    save(settings, &store)?;
    ensure!(
        plan,
        "ChatGPT plan permission is no longer enabled. Enable it in Setup or explicitly choose an API key."
    );
    Ok(access)
}

/// Clear local tokens even when remote revocation cannot be confirmed; retain the registration.
pub fn logout(settings: &Settings) -> Result<bool> {
    let _lock = lock(settings)?;
    let mut store = load(settings)?;
    let account = store
        .accounts
        .iter_mut()
        .find(|a| Some(&a.client_id) == store.active.as_ref());
    let mut confirmed = true;
    if let Some(account) = account {
        if let Some(refresh) = account
            .tokens
            .as_ref()
            .and_then(|t| t.refresh_token.as_deref())
        {
            confirmed = (|| -> Result<bool> {
                let http = http()?;
                let discovery: serde_json::Value = http
                    .get(format!("{ISSUER}/.well-known/openid-configuration"))
                    .send()?
                    .error_for_status()?
                    .json()?;
                let endpoint = reqwest::Url::parse(
                    discovery["revocation_endpoint"]
                        .as_str()
                        .context("No revocation endpoint")?,
                )?;
                ensure!(
                    endpoint.scheme() == "https"
                        && endpoint.host_str() == Some("auth.openai.com")
                        && endpoint.username().is_empty(),
                    "Untrusted revocation endpoint"
                );
                for attempt in 0..3 {
                    match http
                        .post(endpoint.clone())
                        .form(&[
                            ("token", refresh),
                            ("token_type_hint", "refresh_token"),
                            ("client_id", account.client_id.as_str()),
                        ])
                        .send()
                    {
                        Ok(r) if r.status().as_u16() == 200 => return Ok(true),
                        Ok(r) if !r.status().is_server_error() => return Ok(false),
                        _ => {
                            if attempt < 2 {
                                std::thread::sleep(Duration::from_secs(1 << attempt));
                            }
                        }
                    }
                }
                Ok(false)
            })()
            .unwrap_or(false);
        }
        account.tokens = None;
    }
    save(settings, &store)?;
    Ok(confirmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    fn settings(dir: &std::path::Path) -> Settings {
        Settings {
            data_dir: dir.into(),
            models_dir: dir.join("models"),
            whisper_model: "large-v3-turbo".into(),
            language: Some("en".into()),
            model: "openai/gpt-5.6".parse().unwrap(),
            scorer: crate::config::ScorerRef::Off,
            share_questions: false,
            share_company: false,
            diagnostics: false,
            registry_url: crate::registry::DEFAULT_URL.into(),
        }
    }

    fn account(client: &str, expired: bool) -> Account {
        Account {
            client_id: client.into(),
            subject: "person".into(),
            email: Some("person@example.test".into()),
            tokens: Some(Tokens {
                access_token: "old-access".into(),
                refresh_token: Some("old-refresh".into()),
                id_token: "old-identity".into(),
                scopes: vec![PLAN_SCOPE.into()],
                expires_at: now() + if expired { -10 } else { 3600 },
                earliest_refresh_at: None,
            }),
        }
    }

    fn store(settings: &Settings, expired: bool) {
        save(
            settings,
            &Store {
                host_id: "urn:uuid:test-host".into(),
                active: Some("oaiapp_one".into()),
                accounts: vec![account("oaiapp_one", expired), account("oaiapp_two", false)],
                ..Store::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn callback_checks_state_path_and_registration_before_consuming_code() {
        let a = Attempt::new(1455, DYNAMIC.into());
        assert!(a.callback("/favicon.ico").unwrap().is_none());
        assert!(
            a.callback("/auth/callback?state=wrong&code=secret&client_id=oaiapp_one")
                .unwrap()
                .is_none()
        );
        assert!(
            a.callback(&format!(
                "/auth/callback?state={}&error=access_denied",
                a.state
            ))
            .is_err()
        );
        assert!(
            a.callback(&format!("/auth/callback?state={}&code=x", a.state))
                .is_err()
        );
        assert!(
            a.callback(&format!(
                "/auth/callback?state={}&state={}&code=x",
                a.state, a.state
            ))
            .is_err()
        );
        let (code, client) = a
            .callback(&format!(
                "/auth/callback?state={}&code=secret&client_id=oaiapp_one",
                a.state
            ))
            .unwrap()
            .unwrap();
        assert_eq!((code.as_str(), client.as_str()), ("secret", "oaiapp_one"));
        let returning = Attempt::new(1456, "oaiapp_one".into());
        assert!(
            returning
                .callback(&format!(
                    "/auth/callback?state={}&code=x&client_id=oaiapp_two",
                    returning.state
                ))
                .is_err()
        );
        assert_eq!(
            returning
                .callback(&format!("/auth/callback?state={}&code=x", returning.state))
                .unwrap()
                .unwrap()
                .1,
            "oaiapp_one"
        );
    }

    #[test]
    fn authorization_has_pkce_nonce_exact_loopback_and_stable_host() {
        let a = Attempt::new(23456, DYNAMIC.into());
        let url = a.url("urn:uuid:host", None, false).unwrap();
        let q: HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:23456/auth/callback");
        assert_eq!(q["ext_agent_host_id"], "urn:uuid:host");
        assert_eq!(
            q["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(a.verifier.as_bytes()))
        );
        assert_eq!(q["nonce"], a.nonce);
        assert_eq!(q["scope"], SCOPES);
        assert!(q.contains_key("agent_name_hint"));
        assert!(!q.contains_key("prompt"));
        let returning = Attempt::new(23457, "oaiapp_one".into());
        let url = returning
            .url("urn:uuid:host", Some(&account("oaiapp_one", false)), true)
            .unwrap();
        let q: HashMap<_, _> = url.query_pairs().collect();
        assert!(!q.contains_key("agent_name_hint"));
        assert_eq!(q["prompt"], "consent");
    }

    fn signed(claims: &Claims) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".into());
        encode(
            &header,
            claims,
            &EncodingKey::from_rsa_pem(include_bytes!("../tests/fixtures/oauth/test-key.pem"))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn identity_requires_valid_signature_issuer_audience_expiry_and_nonce() {
        let keys: JwkSet =
            serde_json::from_str(include_str!("../tests/fixtures/oauth/jwks.json")).unwrap();
        let mut claims = Claims {
            iss: ISSUER.into(),
            sub: "person".into(),
            aud: json!("oaiapp_one"),
            exp: (now() + 300) as u64,
            nonce: Some("nonce".into()),
            email: None,
        };
        assert_eq!(
            validate_identity(&signed(&claims), "oaiapp_one", Some("nonce"), &keys)
                .unwrap()
                .sub,
            "person"
        );
        assert!(validate_identity(&signed(&claims), "oaiapp_two", Some("nonce"), &keys).is_err());
        assert!(
            validate_identity(&signed(&claims), "oaiapp_one", Some("other-nonce"), &keys).is_err()
        );
        claims.iss = "https://attacker.example".into();
        assert!(validate_identity(&signed(&claims), "oaiapp_one", Some("nonce"), &keys).is_err());
        claims.iss = ISSUER.into();
        claims.exp = (now() - 10) as u64;
        assert!(validate_identity(&signed(&claims), "oaiapp_one", Some("nonce"), &keys).is_err());
        claims.exp = (now() + 300) as u64;
        let token = signed(&claims);
        let (payload, _) = token.rsplit_once('.').unwrap();
        assert!(
            validate_identity(
                &format!("{payload}.invalid-signature"),
                "oaiapp_one",
                Some("nonce"),
                &keys
            )
            .is_err()
        );
    }

    #[test]
    fn credentials_are_private_status_is_redacted_and_account_tokens_stay_separate() {
        let temp = tempfile::tempdir().unwrap();
        let s = settings(temp.path());
        store(&s, false);
        assert_eq!(
            std::fs::metadata(path(&s)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(directory(&s))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let json = serde_json::to_string(&status(&s).unwrap()).unwrap();
        assert!(
            !json.contains("old-access")
                && !json.contains("old-refresh")
                && !json.contains("old-identity")
        );
        let mut saved = load(&s).unwrap();
        saved.accounts[0].tokens = None;
        save(&s, &saved).unwrap();
        let again = load(&s).unwrap();
        assert!(again.accounts[0].tokens.is_none());
        assert_eq!(
            again.accounts[1].tokens.as_ref().unwrap().access_token,
            "old-access"
        );
        assert_eq!(again.host_id, "urn:uuid:test-host");
    }

    #[test]
    fn a_missing_plan_grant_never_uses_a_saved_key_implicitly() {
        let temp = tempfile::tempdir().unwrap();
        let s = settings(temp.path());
        store(&s, false);
        let mut saved = load(&s).unwrap();
        saved.api_key = Some("sk-saved-key".into());
        saved.accounts[0].tokens.as_mut().unwrap().scopes.clear();
        save(&s, &saved).unwrap();
        assert!(!status(&s).unwrap().plan_enabled);
        assert!(
            access_token(&s, false)
                .unwrap_err()
                .to_string()
                .contains("not enabled")
        );
        assert!(!status(&s).unwrap().using_api_key);
        save_api_key(&s, "sk-explicitly-selected-test-key").unwrap();
        assert!(status(&s).unwrap().using_api_key);
    }

    #[test]
    fn simultaneous_process_operations_cannot_rotate_the_same_refresh_token() {
        let temp = tempfile::tempdir().unwrap();
        let s = settings(temp.path());
        let guard = lock(&s).unwrap();
        assert!(lock(&s).is_err());
        drop(guard);
        assert!(lock(&s).is_ok());
    }

    // Local HTTP fixtures test the actual refresh form and persisted rotation, with no paid requests.
    fn server(code: u16, body: serde_json::Value) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut request = vec![0; length];
            reader.read_exact(&mut request).unwrap();
            drop(reader);
            let body = body.to_string();
            write!(stream, "HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, task)
    }

    #[test]
    fn refresh_uses_issued_client_and_rotates_tokens_together_without_touching_other_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let s = settings(temp.path());
        store(&s, true);
        let (url, task) = server(
            200,
            json!({"access_token":"new-access", "refresh_token":"new-refresh",
            "expires_in":3600, "token_type":"Bearer", "scope":PLAN_SCOPE}),
        );
        assert_eq!(
            refresh_access_token(&s, false, &url, "unused").unwrap(),
            "new-access"
        );
        let request = task.join().unwrap();
        let form: HashMap<_, _> = reqwest::Url::parse(&format!("http://localhost/?{request}"))
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(form["client_id"], "oaiapp_one");
        assert_eq!(form["refresh_token"], "old-refresh");
        assert_eq!(form["resource"], RESOURCE);
        assert!(!form.contains_key("scope"));
        let saved = load(&s).unwrap();
        assert_eq!(
            saved.accounts[0]
                .tokens
                .as_ref()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("new-refresh")
        );
        assert_eq!(
            saved.accounts[1]
                .tokens
                .as_ref()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("old-refresh")
        );
        // A fresh access token does not make another HTTP request.
        assert_eq!(
            refresh_access_token(&s, false, "unused", "unused").unwrap(),
            "new-access"
        );
    }

    #[test]
    fn terminal_refresh_clears_only_selected_tokens_but_transient_errors_preserve_them() {
        let temp = tempfile::tempdir().unwrap();
        let s = settings(temp.path());
        store(&s, true);
        let (url, task) = server(503, json!({"error":"temporarily_unavailable"}));
        assert!(refresh_access_token(&s, false, &url, "unused").is_err());
        task.join().unwrap();
        assert!(load(&s).unwrap().accounts[0].tokens.is_some());
        let (url, task) = server(400, json!({"error":"invalid_grant"}));
        assert!(refresh_access_token(&s, false, &url, "unused").is_err());
        task.join().unwrap();
        let saved = load(&s).unwrap();
        assert!(saved.accounts[0].tokens.is_none());
        assert!(saved.accounts[1].tokens.is_some());
        assert_eq!(saved.active.as_deref(), Some("oaiapp_one"));
    }
}
