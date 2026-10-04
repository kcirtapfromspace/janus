//! Opt-in ChatGPT inference smoke test. Uses a synthetic prompt and the saved account,
//! never an interview transcript or a fallback API key.
use interview_coach::{catalog, config::{ModelRef, Settings}, llm::{self, Effort, StructuredRequest}, openai_auth};
use serde_json::json;

#[test]
#[ignore = "requires saved ChatGPT sign-in and plan permission; sends one synthetic request"]
fn chatgpt_structured_response_completes() {
    let settings = Settings::load().unwrap();
    let status = openai_auth::status(&settings).unwrap();
    assert!(status.signed_in && status.plan_enabled && !status.using_api_key,
            "requires active ChatGPT plan credentials");
    assert!(std::env::var_os("IC_LLM_URL").is_none(), "native authentication only");
    let offers = catalog::fetch(&settings).unwrap();
    let model: ModelRef = offers.iter().find(|offer| offer.provider == "openai")
        .expect("account must offer an OpenAI model").model.parse().unwrap();
    let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},
                       "required":["ok"],"additionalProperties":false});
    let req = StructuredRequest {
        model: &model.name, system: "Return the requested JSON object.",
        user: "Return an object with ok set to true.", schema: &schema,
        schema_name: "smoke_test", effort: Effort::Low, max_tokens: 512,
    };
    let result = llm::configured_client(&settings, &model).unwrap()
        .structured(&req, &mut |_| {}).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&result).unwrap(), json!({"ok":true}));
}
