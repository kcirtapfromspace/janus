//! Live wiring checks against a running LiteLLM proxy, without spending anything: requests carry a
//! fake credential, so success means the provider's own auth error came back through the proxy.
//!
//!   IC_LIVE_PROXY_URL=http://127.0.0.1:4000 IC_LIVE_PROXY_KEY=sk-... \
//!     cargo test --test proxy_live -- --ignored

use interview_coach::llm::anthropic;
use interview_coach::llm::{Effort, Llm, LlmError, StructuredRequest};
use interview_coach::proxy::LlmEndpoint;
use serde_json::json;

fn endpoint() -> Option<LlmEndpoint> {
    Some(LlmEndpoint {
        base_url: std::env::var("IC_LIVE_PROXY_URL").ok()?,
        api_key: std::env::var("IC_LIVE_PROXY_KEY").ok()?,
    })
}

#[test]
#[ignore = "needs a running proxy: set IC_LIVE_PROXY_URL and IC_LIVE_PROXY_KEY"]
fn claude_adapter_sends_the_login_token_through_the_proxy_to_anthropic() {
    let endpoint = endpoint().expect("set IC_LIVE_PROXY_URL and IC_LIVE_PROXY_KEY");
    let client = anthropic::Client::new(endpoint, Box::new(|| Ok("sk-ant-oat01-fake-login-token".into())));
    let schema = json!({"type": "object", "properties": {}, "additionalProperties": false, "required": []});
    let req = StructuredRequest {
        model: "claude-opus-5-5", system: "s", user: "Say OK", schema: &schema, schema_name: "x",
        effort: Effort::Low, max_tokens: 16,
    };
    match client.structured(&req, &mut |_| {}) {
        // Anthropic's message for a bad OAuth token — the proxy accepted ic's key and forwarded the token.
        Err(LlmError::Auth(m)) => assert!(m.contains("OAuth access token is invalid"), "unexpected auth error: {m}"),
        other => panic!("expected Anthropic's OAuth auth error, got {other:?}"),
    }
}
