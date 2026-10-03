//! Provider-neutral LLM layer: one typed interface, a native adapter per provider.
//!
//! Normal requests use native provider APIs; explicitly configured external gateways remain supported.
//! JSON schemas are derived from Rust output types, then responses are parsed and validated.

pub mod anthropic;
pub mod jev;
pub mod openai;

use std::io::BufRead;
use std::time::Duration;

use anyhow::{Context, anyhow};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::{ModelRef, Provider};
use crate::proxy::LlmEndpoint;
use crate::schema;

const MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("not authorized: {0} — check: ic doctor")]
    Auth(String),
    #[error("rate limited by the model provider — wait a minute and try again")]
    RateLimited,
    #[error("the model provider is overloaded right now — try again shortly")]
    Overloaded,
    #[error("model API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("couldn't reach the AI service: {0} — check your internet connection and try again")]
    Network(String),
    #[error(
        "ChatGPT usage limit reached. Review app limits in ChatGPT Settings → Usage (https://chatgpt.com/settings/usage), then try again."
    )]
    PlanLimit,
    #[error(
        "ChatGPT plan usage is unavailable for this account or workspace. Check your account and permissions in Setup."
    )]
    PlanUnavailable,
    #[error("the model declined to analyse this ({0})")]
    Refusal(String),
    #[error("the response was cut off (hit the output-token limit) — try again")]
    MaxTokens,
    #[error("unexpected response from the model API: {0}")]
    Protocol(String),
}

impl LlmError {
    pub fn retryable(&self) -> bool {
        match self {
            LlmError::RateLimited | LlmError::Overloaded | LlmError::Network(_) => true,
            LlmError::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

/// One structured-output request: instructions + a single user message, JSON back.
pub struct StructuredRequest<'a> {
    /// The provider's own model name (no `provider/` prefix).
    pub model: &'a str,
    pub system: &'a str,
    pub user: &'a str,
    pub schema: &'a Value,
    /// Schema name (OpenAI requires one): letters, digits, `_` and `-`.
    pub schema_name: &'a str,
    pub effort: Effort,
    pub max_tokens: u32,
}

/// Anything that can answer a structured request — a provider adapter, or a fake in tests.
/// `on_progress` receives the number of output characters so far (0 while the model is thinking).
pub trait Llm {
    fn structured(
        &self,
        req: &StructuredRequest,
        on_progress: &mut dyn FnMut(usize),
    ) -> Result<String, LlmError>;
}

/// A type the model can be asked to produce.
pub trait StructuredOutput: JsonSchema + DeserializeOwned {
    /// Sent to the provider as the schema's name.
    const NAME: &'static str;
    /// Checks the schema can't express (e.g. numeric ranges), run after parsing.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

pub fn schema_for<T: StructuredOutput>() -> Value {
    schema::strict(&serde_json::to_value(schemars::schema_for!(T)).expect("schemas serialize"))
}

/// How much effort and room to give one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerateOptions {
    pub effort: Effort,
    pub max_tokens: u32,
}

impl GenerateOptions {
    /// A long analysis: deep reasoning and room for a full report.
    pub fn analysis(effort: Effort) -> Self {
        GenerateOptions {
            effort,
            max_tokens: 64_000,
        }
    }
}

/// Ask the model for a `T`: the schema comes from the type, and the answer is parsed and validated.
pub fn generate<T: StructuredOutput>(
    llm: &dyn Llm,
    model: &str,
    system: &str,
    user: &str,
    effort: Effort,
    on_progress: &mut dyn FnMut(usize),
) -> anyhow::Result<T> {
    generate_with(
        llm,
        model,
        system,
        user,
        GenerateOptions::analysis(effort),
        on_progress,
    )
}

pub fn generate_with<T: StructuredOutput>(
    llm: &dyn Llm,
    model: &str,
    system: &str,
    user: &str,
    opts: GenerateOptions,
    on_progress: &mut dyn FnMut(usize),
) -> anyhow::Result<T> {
    let schema = schema_for::<T>();
    let req = StructuredRequest {
        model,
        system,
        user,
        schema: &schema,
        schema_name: T::NAME,
        effort: opts.effort,
        max_tokens: opts.max_tokens,
    };
    let text = llm.structured(&req, on_progress)?;
    let value: T = serde_json::from_str(&text)
        .with_context(|| format!("the model's {} didn't match the expected shape", T::NAME))?;
    value
        .validate()
        .map_err(|e| anyhow!("the model's {} failed validation: {e}", T::NAME))?;
    Ok(value)
}

/// Ask for JSON matching a schema built at runtime (e.g. from a list of checks), parsed but not typed.
pub fn structured_value(
    llm: &dyn Llm,
    model: &str,
    system: &str,
    user: &str,
    schema: &Value,
    name: &str,
    opts: GenerateOptions,
) -> anyhow::Result<Value> {
    let schema = schema::strict(schema);
    let req = StructuredRequest {
        model,
        system,
        user,
        schema: &schema,
        schema_name: name,
        effort: opts.effort,
        max_tokens: opts.max_tokens,
    };
    let text = llm.structured(&req, &mut |_| {})?;
    serde_json::from_str(&text).with_context(|| format!("the model's {name} wasn't valid JSON"))
}

/// The adapter for `model`'s provider, talking to the LiteLLM proxy at `endpoint`.
pub fn client(model: &ModelRef, endpoint: LlmEndpoint) -> Box<dyn Llm> {
    match model.provider {
        Provider::Anthropic => Box::new(anthropic::Client::new(
            endpoint,
            Box::new(|| crate::auth::access_token().map_err(|e| LlmError::Auth(e.to_string()))),
        )),
        Provider::OpenAi => Box::new(openai::Client::new(endpoint)),
    }
}

/// Normal installs use native APIs. Only explicitly configured shared proxies broker core calls.
pub fn configured_client(
    settings: &crate::config::Settings,
    model: &ModelRef,
) -> anyhow::Result<Box<dyn Llm>> {
    if crate::proxy::using_external_proxy() {
        return Ok(client(
            model,
            LlmEndpoint::load(settings).context("IC_LLM_URL requires IC_LLM_KEY")?,
        ));
    }
    match model.provider {
        Provider::Anthropic => Ok(Box::new(anthropic::Client::direct(Box::new(|| {
            crate::auth::access_token().map_err(|e| LlmError::Auth(e.to_string()))
        })))),
        Provider::OpenAi => {
            let status = crate::openai_auth::status(settings)?;
            if !status.using_api_key && status.active.is_some() {
                let settings = settings.clone();
                Ok(Box::new(openai::Client::direct(Box::new(move |force| {
                    crate::openai_auth::access_token(&settings, force)
                        .map_err(|e| LlmError::Auth(e.to_string()))
                }))))
            } else {
                let key = crate::openai_auth::api_key(settings)?
                    .or_else(|| crate::proxy::legacy_openai_key(settings))
                    .context(
                        "Continue with ChatGPT in Setup, or explicitly configure an OpenAI API key",
                    )?;
                Ok(Box::new(openai::Client::direct(Box::new(move |_| {
                    Ok(key.clone())
                }))))
            }
        }
    }
}

// --- shared plumbing for the adapters ---------------------------------------------------------

pub(crate) fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(20 * 60)) // long transcript + deep reasoning can take minutes
        .build()
        .expect("building HTTP client")
}

/// Map a non-200 response to an error. Anthropic, OpenAI, and LiteLLM all put a human-readable
/// message under `error.message`.
pub(crate) fn error_from_response(resp: reqwest::blocking::Response) -> LlmError {
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text).unwrap_or_default();
    let message = value["error"]["message"]
        .as_str()
        .or_else(|| value["detail"].as_str())
        .unwrap_or(&text)
        .to_string();
    provider_error(status, value["error"]["code"].as_str(), message)
}

pub(crate) fn provider_error(status: u16, code: Option<&str>, message: String) -> LlmError {
    match code.unwrap_or_default() {
        "subscription_sharing_usage_limit_exceeded" => return LlmError::PlanLimit,
        "subscription_sharing_user_not_eligible" => return LlmError::PlanUnavailable,
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            return LlmError::Api {
                status: 503,
                message,
            };
        }
        "subscription_sharing_unsupported_capability"
        | "subscription_sharing_route_not_supported"
        | "chatpass_v2_scope_not_authorized"
        | "chatpass_v2_invalid_authorization_context" => {
            return LlmError::Api {
                status: 400,
                message,
            };
        }
        "subscription_sharing_invalid_user" => return LlmError::Auth(message),
        _ => {}
    }
    match status {
        401 | 403 => LlmError::Auth(message),
        429 => LlmError::RateLimited,
        529 => LlmError::Overloaded,
        _ => LlmError::Api { status, message },
    }
}

/// Run `attempt` up to MAX_ATTEMPTS times, backing off on retryable errors.
pub(crate) fn with_retries<T>(
    mut attempt: impl FnMut() -> Result<T, LlmError>,
) -> Result<T, LlmError> {
    let mut n = 1;
    loop {
        match attempt() {
            Err(e) if e.retryable() && n < MAX_ATTEMPTS => {
                std::thread::sleep(Duration::from_secs(2u64.pow(n)));
                n += 1;
            }
            other => return other,
        }
    }
}

/// Feed each server-sent event's JSON payload to `on_event` until it returns `false`.
pub(crate) fn for_each_event(
    reader: impl BufRead,
    mut on_event: impl FnMut(Value) -> Result<bool, LlmError>,
) -> Result<(), LlmError> {
    let mut data = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| LlmError::Network(e.to_string()))?;
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
            continue;
        }
        if !line.is_empty() || data.is_empty() {
            continue; // `event:` lines repeat the JSON "type" field
        }
        let event: Value = serde_json::from_str(&std::mem::take(&mut data))
            .map_err(|e| LlmError::Protocol(format!("bad event JSON: {e}")))?;
        if !on_event(event)? {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}
