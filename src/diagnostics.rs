//! Anonymous diagnostics, only when you turn them on (`diagnostics = on`). They are counts and
//! outcomes, never content:
//! - no transcripts, questions, names, companies, file paths or error messages;
//! - no audio, video or faces.
//!
//! They show whether recording, video reading, reviews and practice work, so problems in the field
//! show up without anyone sending their interviews. Each event carries this install's own random id
//! (not the registry's) and makes no person profile.
//!
//! The PostHog project key is a public client key, built in at release time (`IC_POSTHOG_KEY`);
//! without one, nothing is sent.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::config::Settings;

/// Set at build time for release builds; `IC_POSTHOG_KEY` at run time overrides it (testing).
const BUILT_IN_KEY: Option<&str> = option_env!("IC_POSTHOG_KEY");
const BUILT_IN_HOST: Option<&str> = option_env!("IC_POSTHOG_HOST");
const DEFAULT_HOST: &str = "https://us.i.posthog.com";

/// Properties allowed on events: anything else is dropped before sending, so content can't slip
/// in through a careless call.
const ALLOWED: &[&str] = &[
    "ok", "stage", "provider", "mode", "video", "practice", "minutes", "warnings", "faces_read", "faces_complete",
    "faces_share", "you_found", "answers", "answers_with_cues", "checks", "method", "questions", "turns", "shared",
    "command",
];

fn id_path(settings: &Settings) -> PathBuf {
    settings.data_dir.join("diagnostics-id")
}

fn install(settings: &Settings) -> Option<String> {
    let path = id_path(settings);
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim().to_string();
        if id.len() == 32 {
            return Some(id);
        }
    }
    let id = format!("{:032x}", rand::random::<u128>());
    std::fs::write(&path, &id).ok()?;
    Some(id)
}

/// The event as it would be sent: only allowed properties, plus the app's version and no person
/// profile. None when diagnostics are off.
pub fn event(settings: &Settings, name: &str, properties: Value) -> Option<Value> {
    if !settings.diagnostics {
        return None;
    }
    let mut props: Map<String, Value> =
        properties.as_object().map(|m| m.iter().filter(|(k, _)| ALLOWED.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    props.insert("app_version".into(), json!(env!("CARGO_PKG_VERSION")));
    props.insert("$process_person_profile".into(), json!(false));
    props.insert("$lib".into(), json!("janus-ic"));
    Some(json!({"event": name, "distinct_id": install(settings)?, "properties": props}))
}

/// Send one event, when diagnostics are on and a key is built in. Never fails the caller, and gives
/// up quickly: diagnostics must not slow anything down.
pub fn capture(settings: &Settings, name: &str, properties: Value) {
    let Some(mut body) = event(settings, name, properties) else { return };
    let key = std::env::var("IC_POSTHOG_KEY").ok().or(BUILT_IN_KEY.map(String::from)).filter(|k| k.starts_with("phc_"));
    let Some(key) = key else { return };
    body["api_key"] = json!(key);
    let host = std::env::var("IC_POSTHOG_HOST").ok().or(BUILT_IN_HOST.map(String::from)).unwrap_or_else(|| DEFAULT_HOST.into());
    if let Ok(client) = reqwest::blocking::Client::builder().timeout(Duration::from_secs(3)).build() {
        let _ = client.post(format!("{}/i/v0/e/", host.trim_end_matches('/'))).json(&body).send();
    }
}

/// Minutes, rounded to a bucket (5, 15, 30, 45, 60, 90+), so lengths don't fingerprint anyone.
pub fn minutes(seconds: f64) -> u32 {
    let m = seconds / 60.0;
    [5, 15, 30, 45, 60].into_iter().find(|&b| m <= b as f64).unwrap_or(90)
}

/// A share, rounded to tenths.
pub fn share(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(on: bool) -> (tempfile::TempDir, Settings) {
        let tmp = tempfile::tempdir().unwrap();
        let s = Settings { data_dir: tmp.path().to_path_buf(), diagnostics: on, ..Settings::load().unwrap() };
        (tmp, s)
    }

    #[test]
    fn nothing_is_made_when_diagnostics_are_off() {
        let (_tmp, s) = settings(false);
        assert_eq!(event(&s, "report_finished", json!({"ok": true})), None);
        assert!(!id_path(&s).exists(), "not even an id");
    }

    #[test]
    fn only_allowed_properties_are_sent() {
        let (_tmp, s) = settings(true);
        let e = event(&s, "report_finished", json!({"ok": true, "provider": "anthropic", "title": "Acme PM", "path": "/Users/x"})).unwrap();
        let props = e["properties"].as_object().unwrap();
        assert_eq!(props["ok"], true);
        assert_eq!(props["provider"], "anthropic");
        assert!(!props.contains_key("title") && !props.contains_key("path"), "content is dropped: {props:?}");
        assert_eq!(props["$process_person_profile"], false);
        assert_eq!(e["distinct_id"].as_str().unwrap().len(), 32);
        assert_eq!(event(&s, "x", json!({})).unwrap()["distinct_id"], e["distinct_id"], "one id per install");
    }

    #[test]
    fn lengths_and_shares_are_coarse() {
        assert_eq!(minutes(42.0 * 60.0), 45);
        assert_eq!(minutes(3.0 * 60.0), 5);
        assert_eq!(minutes(200.0 * 60.0), 90);
        assert_eq!(share(0.8734), 0.9);
    }
}
