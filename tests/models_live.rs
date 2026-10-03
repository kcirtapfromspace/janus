//! Every Claude model your sign-in can use, sent one tiny structured request through the real
//! adapter, to check the settings it sends (thinking, effort, betas) are accepted. Costs well
//! under a cent in total.
//!
//!   IC_LLM_URL=http://127.0.0.1:4000 IC_LLM_KEY=sk-... cargo test --release --test models_live -- --ignored --nocapture

use interview_coach::config::{ModelRef, Provider};
use interview_coach::llm::{self, Effort, StructuredRequest};
use interview_coach::proxy::LlmEndpoint;
use serde_json::json;

#[test]
#[ignore = "needs the proxy and a Claude sign-in: set IC_LLM_URL and IC_LLM_KEY"]
fn every_listed_claude_model_accepts_the_adapters_settings() {
    let endpoint = LlmEndpoint { base_url: std::env::var("IC_LLM_URL").expect("IC_LLM_URL"),
                                 api_key: std::env::var("IC_LLM_KEY").expect("IC_LLM_KEY") };
    let models: Vec<String> = std::env::var("IC_MODELS").expect("IC_MODELS: comma-separated").split(',').map(String::from).collect();
    let client = llm::client(&ModelRef { provider: Provider::Anthropic, name: String::new() }, endpoint);
    let schema = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"], "additionalProperties": false});
    let mut failed = vec![];
    for model in &models {
        let req = StructuredRequest { model, system: "Answer with JSON.", user: "Is water wet? Set ok to true.", schema: &schema,
                                      schema_name: "probe", effort: Effort::Low, max_tokens: 2000 };
        match client.structured(&req, &mut |_| {}) {
            Ok(text) => println!("ok    {model:32} {}", text.trim()),
            Err(e) => {
                println!("FAIL  {model:32} {e}");
                failed.push(model.clone());
            }
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}
