//! Which models a report can be written with, and what each costs at list price.
//!
//! Availability comes from the providers themselves, through the AI proxy:
//! - Claude: the models your sign-in can use (`/anthropic/v1/models`);
//! - OpenAI: the models your key can use, when the proxy holds one.
//!
//! Prices come from LiteLLM's own price list (`/public/litellm_model_cost_map`). "Cheapest" is by
//! list price for a typical report. Claude through your sign-in may be billed against a
//! subscription instead, so the list price is a guide, not your bill.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::config::{ModelRef, Settings};
use crate::proxy::{self, KeyTarget, LlmEndpoint};

/// A typical report: the transcript and instructions in, the written analysis (and any thinking) out.
pub const TYPICAL_INPUT_TOKENS: f64 = 15_000.0;
pub const TYPICAL_OUTPUT_TOKENS: f64 = 8_000.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Offer {
    /// `provider/model`, as `--model` takes it.
    pub model: String,
    pub provider: &'static str,
    pub display_name: Option<String>,
    /// Dollars per million tokens, when LiteLLM's price list has the model.
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    /// Dollars for a typical report at list price.
    pub typical_report: Option<f64>,
    pub cheapest: bool,
}

/// `claude-haiku-4-5-20251001` → `claude-haiku-4-5`; `gpt-5-mini-2025-08-07` → `gpt-5-mini`.
fn undated(id: &str) -> &str {
    let dated = |tail: &str| {
        let digits: String = tail.chars().filter(|c| c.is_ascii_digit()).collect();
        digits.len() == 8 && tail.chars().all(|c| c.is_ascii_digit() || c == '-')
    };
    if let Some((head, tail)) = id.rsplit_once('-')
        && tail.len() == 8
        && dated(tail)
    {
        return head;
    }
    if id.len() > 11 && dated(&id[id.len() - 10..]) && id.as_bytes()[id.len() - 11] == b'-' {
        return &id[..id.len() - 11];
    }
    id
}

fn price(cost_map: &Value, id: &str) -> Option<(f64, f64)> {
    let entry = cost_map.get(id).or_else(|| cost_map.get(undated(id)))?;
    Some((
        entry["input_cost_per_token"].as_f64()? * 1e6,
        entry["output_cost_per_token"].as_f64()? * 1e6,
    ))
}

/// OpenAI models the adapter can use: chat models with structured output and reasoning settings.
fn openai_usable(cost_map: &Value, id: &str) -> bool {
    let entry = cost_map.get(id).or_else(|| cost_map.get(undated(id)));
    entry.is_some_and(|e| {
        e["mode"] == "chat"
            && e["supports_response_schema"] == true
            && e["supports_reasoning"] == true
    })
}

/// The offers, cheapest-for-a-typical-report first (unpriced models last), with the cheapest marked.
pub fn offers(claude: &[String], openai: &[String], cost_map: &Value) -> Vec<Offer> {
    let mut out: Vec<Offer> = claude
        .iter()
        .map(|id| ("anthropic", id))
        .chain(
            openai
                .iter()
                .filter(|id| openai_usable(cost_map, id))
                .map(|id| ("openai", id)),
        )
        .map(|(provider, id)| {
            let p = price(cost_map, id);
            Offer {
                model: format!("{provider}/{id}"),
                provider,
                display_name: None,
                input_per_mtok: p.map(|p| p.0),
                output_per_mtok: p.map(|p| p.1),
                typical_report: p
                    .map(|(i, o)| (i * TYPICAL_INPUT_TOKENS + o * TYPICAL_OUTPUT_TOKENS) / 1e6),
                cheapest: false,
            }
        })
        .collect();
    out.sort_by(|a, b| match (a.typical_report, b.typical_report) {
        (Some(x), Some(y)) => x.total_cmp(&y).then_with(|| a.model.cmp(&b.model)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.model.cmp(&b.model),
    });
    if let Some(first) = out.first_mut().filter(|o| o.typical_report.is_some()) {
        first.cheapest = true;
    }
    out
}

fn get_json(req: reqwest::blocking::RequestBuilder) -> Result<Value> {
    let response = req.send()?;
    let status = response.status();
    let text = response.text()?;
    anyhow::ensure!(
        status.is_success(),
        "HTTP {status}: {}",
        text.chars().take(200).collect::<String>()
    );
    Ok(serde_json::from_str(&text)?)
}

fn ids(list: &Value) -> Vec<String> {
    list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect()
}

/// Ask the providers (through the proxy) what's available, and LiteLLM what it costs.
pub fn fetch(settings: &Settings) -> Result<Vec<Offer>> {
    if !proxy::using_external_proxy() {
        return fetch_native(settings);
    }
    let endpoint =
        LlmEndpoint::load(settings).context("the AI proxy isn't set up — open Setup in the app")?;
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let base = endpoint.base_url.trim_end_matches('/');
    let cost_map = get_json(
        http.get(format!("{base}/public/litellm_model_cost_map"))
            .bearer_auth(&endpoint.api_key),
    )
    .context("reading LiteLLM's price list")?;
    let claude = match crate::auth::access_token() {
        Ok(token) => {
            let mut req = http.get(format!("{base}/anthropic/v1/models?limit=100"));
            for (name, value) in
                crate::llm::anthropic::headers(&endpoint.api_key, &token, "claude-opus-5-5")
            {
                if name != "content-type" {
                    req = req.header(name, value);
                }
            }
            ids(&get_json(req).context("listing your Claude models")?)
        }
        Err(_) => vec![],
    };
    let openai = if proxy::using_external_proxy() || proxy::has_key(settings, KeyTarget::OpenAi) {
        get_json(
            http.get(format!("{base}/openai_passthrough/v1/models"))
                .bearer_auth(&endpoint.api_key),
        )
        .map(|v| ids(&v))
        .unwrap_or_default()
    } else {
        vec![]
    };
    Ok(offers(&claude, &openai, &cost_map))
}

/// Native connections do not need a local gateway or a third-party price lookup.
/// Preserve ChatGPT's account-specific model ordering and visibility rules.
fn native_offers(provider: &'static str, list: &Value, chatgpt: bool) -> Vec<Offer> {
    let entries = if chatgpt {
        list["models"].as_array()
    } else {
        list["data"].as_array()
    };
    entries
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            if chatgpt && entry["visibility"] != "list" {
                return None;
            }
            let id = if chatgpt {
                entry["slug"].as_str()?
            } else {
                entry["id"].as_str()?
            };
            // API-key catalogs include audio/embedding and older models without our reasoning settings.
            if provider == "openai"
                && !chatgpt
                && !(id.starts_with("gpt-5")
                    || id.starts_with("gpt-6")
                    || id.starts_with("o3")
                    || id.starts_with("o4"))
            {
                return None;
            }
            if provider == "openai"
                && !chatgpt
                && ["audio", "realtime", "transcribe", "tts", "image", "codex"]
                    .iter()
                    .any(|x| id.contains(x))
            {
                return None;
            }
            Some(Offer {
                model: format!("{provider}/{id}"),
                provider,
                display_name: entry["display_name"].as_str().map(String::from),
                input_per_mtok: None,
                output_per_mtok: None,
                typical_report: None,
                cheapest: false,
            })
        })
        .collect()
}

fn fetch_native(settings: &Settings) -> Result<Vec<Offer>> {
    use crate::{config::Provider, openai_auth};
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mut out = vec![];
    if let Ok(token) = crate::auth::access_token() {
        let mut req = http.get("https://api.anthropic.com/v1/models?limit=100");
        for (name, value) in crate::llm::anthropic::headers("", &token, &settings.model.name) {
            if name != "x-litellm-api-key" {
                req = req.header(name, value);
            }
        }
        match get_json(req) {
            Ok(list) => out.extend(native_offers("anthropic", &list, false)),
            Err(e) if settings.model.provider == Provider::Anthropic => {
                return Err(e.context("listing your Claude models"));
            }
            Err(_) => {}
        }
    }
    let status = openai_auth::status(settings)?;
    let chatgpt = !status.using_api_key && status.active.is_some();
    let token = if chatgpt {
        openai_auth::access_token(settings, false).map(Some)
    } else {
        openai_auth::api_key(settings).map(|key| key.or_else(|| proxy::legacy_openai_key(settings)))
    };
    match token {
        Ok(Some(token)) => match get_json(
            http.get("https://api.openai.com/v1/models")
                .bearer_auth(token),
        ) {
            Ok(list) => out.extend(native_offers("openai", &list, chatgpt)),
            Err(e) if settings.model.provider == Provider::OpenAi => {
                return Err(e.context("listing your OpenAI models"));
            }
            Err(_) => {}
        },
        Err(e) if settings.model.provider == Provider::OpenAi => return Err(e),
        _ => {}
    }
    Ok(out)
}

/// The model `--model cheapest` stands for.
pub fn cheapest(settings: &Settings) -> Result<ModelRef> {
    let offers = fetch(settings)?;
    let best = offers.iter().find(|o| o.cheapest).context("No priced model is available. Native connections do not estimate API or subscription prices; choose a model from ic models instead.")?;
    best.model.parse().map_err(|e: String| anyhow::anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chatgpt_catalog_keeps_account_order_and_hides_unlisted_models_without_fake_prices() {
        let list = serde_json::json!({"models":[
            {"slug":"gpt-6.1-sol", "display_name":"Sol", "visibility":"list"},
            {"slug":"hidden", "visibility":"hide"},
            {"slug":"gpt-5.6", "display_name":"Earlier model", "visibility":"list"}
        ]});
        let offers = native_offers("openai", &list, true);
        assert_eq!(
            offers.iter().map(|o| o.model.as_str()).collect::<Vec<_>>(),
            ["openai/gpt-6.1-sol", "openai/gpt-5.6"]
        );
        assert_eq!(offers[0].display_name.as_deref(), Some("Sol"));
        assert!(
            offers
                .iter()
                .all(|o| o.typical_report.is_none() && !o.cheapest)
        );
    }

    #[test]
    fn dated_ids_find_their_price() {
        assert_eq!(undated("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(undated("gpt-5-mini-2025-08-07"), "gpt-5-mini");
        assert_eq!(undated("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(undated("gpt-5.6"), "gpt-5.6");
    }

    #[test]
    fn the_cheapest_priced_model_is_marked_and_unusable_ones_are_left_out() {
        let cost_map = json!({
            "claude-opus-5-5": {"input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5, "mode": "chat"},
            "claude-haiku-4-5": {"input_cost_per_token": 1e-6, "output_cost_per_token": 5e-6, "mode": "chat"},
            "gpt-5-nano": {"input_cost_per_token": 5e-8, "output_cost_per_token": 4e-7, "mode": "chat",
                           "supports_response_schema": true, "supports_reasoning": true},
            "gpt-4o": {"input_cost_per_token": 2.5e-6, "output_cost_per_token": 1e-5, "mode": "chat",
                       "supports_response_schema": true, "supports_reasoning": false},
        });
        let claude = vec![
            "claude-opus-5-5".to_string(),
            "claude-haiku-4-5-20251001".into(),
            "claude-new-6".into(),
        ];
        let o = offers(&claude, &[], &cost_map);
        let models: Vec<&str> = o.iter().map(|o| o.model.as_str()).collect();
        assert_eq!(
            models,
            [
                "anthropic/claude-haiku-4-5-20251001",
                "anthropic/claude-opus-5-5",
                "anthropic/claude-new-6"
            ]
        );
        assert!(o[0].cheapest && !o[1].cheapest);
        assert!(
            (o[0].typical_report.unwrap() - 0.055).abs() < 1e-9,
            "15k in at $1/M + 8k out at $5/M"
        );
        assert_eq!(
            o[2].typical_report, None,
            "unpriced: listed last, never the cheapest"
        );

        let with_openai = offers(
            &claude,
            &[
                "gpt-5-nano".into(),
                "gpt-4o".into(),
                "text-embedding-3-small".into(),
            ],
            &cost_map,
        );
        assert_eq!(with_openai[0].model, "openai/gpt-5-nano");
        assert!(with_openai[0].cheapest);
        assert!(
            !with_openai
                .iter()
                .any(|o| o.model.contains("gpt-4o") || o.model.contains("embedding")),
            "no reasoning settings or not a chat model"
        );
    }
}
