//! Claude via LiteLLM's Anthropic pass-through (`/anthropic/v1/messages`), in Claude's native API:
//! adaptive thinking, effort, structured outputs, and server-side refusal fallbacks.
//!
//! Auth uses your browser login, not an API key: each request carries a fresh OAuth access token
//! (`Authorization: Bearer`), which LiteLLM forwards to Anthropic, while ic authenticates to the
//! proxy separately with its virtual key in `x-litellm-api-key`.

use std::io::BufRead;

use serde_json::{Value, json};

use super::{Llm, LlmError, StructuredRequest, error_from_response, for_each_event, http_client, with_retries};
use crate::proxy::LlmEndpoint;

const MESSAGES_PATH: &str = "/anthropic/v1/messages";
/// If Claude declines a request, the API re-runs it on Anthropic's recommended fallback model.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Required on every request authenticated with an OAuth token instead of an API key.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Where each request's OAuth access token comes from (`auth::access_token`, or a fake in tests).
pub type TokenSource = Box<dyn Fn() -> Result<String, LlmError>>;

/// Request headers: the OAuth token goes to Anthropic; the virtual key only to the proxy.
pub fn headers(proxy_key: &str, oauth_token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("content-type", "application/json".into()),
        ("anthropic-version", "2023-06-01".into()),
        ("anthropic-beta", format!("{OAUTH_BETA},{FALLBACK_BETA}")),
        ("authorization", format!("Bearer {oauth_token}")),
        ("x-litellm-api-key", proxy_key.into()),
    ]
}

pub fn request_body(req: &StructuredRequest) -> Value {
    json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "fallbacks": "default",
        "thinking": {"type": "adaptive"},
        "output_config": {
            "effort": req.effort.as_str(),
            "format": {"type": "json_schema", "schema": req.schema},
        },
        "system": req.system,
        "messages": [{"role": "user", "content": req.user}],
    })
}

pub struct Client {
    http: reqwest::blocking::Client,
    endpoint: LlmEndpoint,
    token: TokenSource,
}

impl Client {
    pub fn new(endpoint: LlmEndpoint, token: TokenSource) -> Self {
        Client { http: http_client(), endpoint, token }
    }

    fn attempt(&self, body: &Value, on_progress: &mut dyn FnMut(usize)) -> Result<String, LlmError> {
        // Fetched per attempt: access tokens are short-lived, and a retry may come minutes later.
        let token = (self.token)()?;
        let mut request = self.http.post(format!("{}{MESSAGES_PATH}", self.endpoint.base_url.trim_end_matches('/')));
        for (name, value) in headers(&self.endpoint.api_key, &token) {
            request = request.header(name, value);
        }
        let resp = request.body(body.to_string()).send().map_err(|e| LlmError::Network(e.to_string()))?;
        if resp.status().as_u16() != 200 {
            return Err(error_from_response(resp));
        }
        parse_stream(std::io::BufReader::new(resp), on_progress)
    }
}

impl Llm for Client {
    fn structured(&self, req: &StructuredRequest, on_progress: &mut dyn FnMut(usize)) -> Result<String, LlmError> {
        let body = request_body(req);
        with_retries(|| self.attempt(&body, on_progress))
    }
}

/// Assemble the final text from a Messages API event stream.
pub fn parse_stream(reader: impl BufRead, on_progress: &mut dyn FnMut(usize)) -> Result<String, LlmError> {
    let mut text = String::new();
    let mut stop_reason: Option<String> = None;
    let mut refusal_category: Option<String> = None;
    for_each_event(reader, |event| {
        match event["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                // Text written before a fallback came from the model that declined; discard it.
                if event["content_block"]["type"] == "fallback" {
                    text.clear();
                }
                on_progress(text.len());
            }
            "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                text.push_str(event["delta"]["text"].as_str().unwrap_or_default());
                on_progress(text.len());
            }
            "message_delta" => {
                let delta = &event["delta"];
                if let Some(reason) = delta["stop_reason"].as_str() {
                    stop_reason = Some(reason.into());
                }
                let details = if delta["stop_details"].is_object() { &delta["stop_details"] } else { &event["stop_details"] };
                if let Some(category) = details["category"].as_str() {
                    refusal_category = Some(category.into());
                }
            }
            "error" => {
                let kind = event["error"]["type"].as_str().unwrap_or_default();
                let message = event["error"]["message"].as_str().unwrap_or(kind).to_string();
                return Err(match kind {
                    "overloaded_error" => LlmError::Overloaded,
                    "rate_limit_error" => LlmError::RateLimited,
                    "api_error" => LlmError::Api { status: 500, message },
                    _ => LlmError::Api { status: 400, message },
                });
            }
            "message_stop" => return Ok(false),
            _ => {}
        }
        Ok(true)
    })?;
    match stop_reason.as_deref() {
        Some("refusal") => Err(LlmError::Refusal(format!("category: {}", refusal_category.as_deref().unwrap_or("unspecified")))),
        Some("max_tokens") => Err(LlmError::MaxTokens),
        // No stop reason means the connection dropped mid-stream: the text is truncated.
        None => Err(LlmError::Network("the stream ended before the model finished".into())),
        _ if text.trim().is_empty() => Err(LlmError::Protocol(format!("no text in the response (stop_reason: {stop_reason:?})"))),
        _ => Ok(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Effort, sse};

    #[test]
    fn parses_text_deltas_and_stop_reason() {
        let stream = sse(&[
            json!({"type": "message_start", "message": {"id": "msg_1"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "{\"a\":"}}),
            json!({"type": "ping"}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "1}"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 9}}),
            json!({"type": "message_stop"}),
        ]);
        let mut seen = vec![];
        assert_eq!(parse_stream(stream.as_bytes(), &mut |n| seen.push(n)).unwrap(), "{\"a\":1}");
        assert_eq!(seen.last(), Some(&7));
    }

    #[test]
    fn text_before_a_fallback_is_discarded() {
        let stream = sse(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "{\"partial"}}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "fallback"}}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "{}"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
        ]);
        assert_eq!(parse_stream(stream.as_bytes(), &mut |_| {}).unwrap(), "{}");
    }

    #[test]
    fn refusal_and_stream_errors_surface() {
        let refused = sse(&[json!({"type": "message_delta", "delta": {"stop_reason": "refusal",
                                   "stop_details": {"type": "refusal", "category": "cyber"}}})]);
        let err = parse_stream(refused.as_bytes(), &mut |_| {}).unwrap_err();
        assert!(matches!(err, LlmError::Refusal(ref c) if c.contains("cyber")), "{err}");

        let overloaded = sse(&[json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})]);
        let err = parse_stream(overloaded.as_bytes(), &mut |_| {}).unwrap_err();
        assert!(matches!(err, LlmError::Overloaded) && err.retryable());
    }

    #[test]
    fn a_stream_cut_off_mid_answer_is_a_retryable_error() {
        let cut = sse(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "{\"summ"}}),
        ]);
        let err = parse_stream(cut.as_bytes(), &mut |_| {}).unwrap_err();
        assert!(matches!(err, LlmError::Network(_)) && err.retryable(), "{err}");
    }

    #[test]
    fn oauth_token_goes_to_anthropic_and_the_virtual_key_to_the_proxy() {
        let h = headers("sk-litellm-virtual", "sk-ant-oat01-token");
        let get = |name: &str| h.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str());
        assert_eq!(get("authorization"), Some("Bearer sk-ant-oat01-token"));
        assert_eq!(get("x-litellm-api-key"), Some("sk-litellm-virtual"));
        assert_eq!(get("anthropic-beta"), Some("oauth-2025-04-20,server-side-fallback-2026-07-01"));
        assert_eq!(get("x-api-key"), None, "no static Anthropic key is ever sent");
    }

    #[test]
    fn an_expired_login_fails_before_any_request_is_sent() {
        let endpoint = LlmEndpoint { base_url: "http://127.0.0.1:9".into(), api_key: "k".into() };
        let client = Client::new(endpoint, Box::new(|| Err(LlmError::Auth("your Claude login has expired".into()))));
        let schema = json!({"type": "object"});
        let req = StructuredRequest { model: "m", system: "s", user: "u", schema: &schema, schema_name: "x",
                                      effort: Effort::High, max_tokens: 10 };
        let err = client.structured(&req, &mut |_| {}).unwrap_err();
        assert!(matches!(err, LlmError::Auth(ref m) if m.contains("expired")), "{err}");
    }

    #[test]
    fn request_body_uses_native_claude_features() {
        let schema = json!({"type": "object"});
        let body = request_body(&StructuredRequest {
            model: "claude-opus-5-5", system: "sys", user: "hi", schema: &schema, schema_name: "x",
            effort: Effort::High, max_tokens: 64000,
        });
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["stream"], true);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["messages"][0]["content"], "hi");
    }
}
