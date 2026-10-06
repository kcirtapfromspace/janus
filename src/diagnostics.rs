//! Anonymous diagnostics: on by default, sent only after the notice has been shown (privacy.rs), and
//! off with `diagnostics = off`. They are counts and
//! outcomes, never content:
//! - no transcripts, questions, names, companies, file paths or error messages;
//! - no audio, video or faces.
//!
//! They show whether recording, video reading, reviews and practice work, so problems in the field
//! show up without anyone sending their interviews. Each event carries this install's own random id
//! (not the registry's) and makes no person profile.
//!
//! The PostHog project key is a public client key (PostHog's US cloud): it can only send events,
//! never read them, so it lives here in the source. `IC_POSTHOG_KEY` overrides it for testing.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::config::Settings;

/// Janus's PostHog project: send-only.
const PROJECT_KEY: &str = "phc_qDvAmDDEL5zf3EKtZBrpVGdVwbyvUM4qAC87WbqdKWcd";
const HOST: &str = "https://us.i.posthog.com";

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
    if !settings.diagnostics || !crate::privacy::seen(settings) {
        return None;
    }
    let mut props: Map<String, Value> =
        properties.as_object().map(|m| m.iter().filter(|(k, _)| ALLOWED.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    props.insert("app_version".into(), json!(env!("CARGO_PKG_VERSION")));
    props.insert("$process_person_profile".into(), json!(false));
    props.insert("$lib".into(), json!("janus-ic"));
    Some(json!({"event": name, "distinct_id": install(settings)?, "properties": props}))
}

/// A repeated failure is reported once an hour per command: the app runs `ic` in the background,
/// and one stuck problem shouldn't become thousands of events.
const REPEAT_S: i64 = 3600;

fn repeated(settings: &Settings, key: &str) -> bool {
    let path = settings.data_dir.join("diagnostics-recent.json");
    let mut recent: std::collections::BTreeMap<String, i64> =
        std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    if recent.get(key).is_some_and(|t| now - t < REPEAT_S) {
        return true;
    }
    recent.retain(|_, t| now - *t < REPEAT_S);
    recent.insert(key.to_string(), now);
    let _ = std::fs::write(&path, serde_json::to_string(&recent).unwrap_or_default());
    false
}

/// Send one event, when diagnostics are on and a key is built in. Never fails the caller, and gives
/// up quickly: diagnostics must not slow anything down.
pub fn capture(settings: &Settings, name: &str, properties: Value) {
    if name == "command_failed" && repeated(settings, &format!("{name}:{}", properties["command"])) {
        return;
    }
    let Some(mut body) = event(settings, name, properties) else { return };
    let key = std::env::var("IC_POSTHOG_KEY").unwrap_or_else(|_| PROJECT_KEY.into());
    if !key.starts_with("phc_") {
        return;
    }
    body["api_key"] = json!(key);
    let host = std::env::var("IC_POSTHOG_HOST").unwrap_or_else(|_| HOST.into());
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
        crate::privacy::mark_seen(&s).unwrap();
        (tmp, s)
    }

    #[test]
    fn on_by_default_but_silent_until_the_notice_is_shown() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Settings { data_dir: tmp.path().to_path_buf(), ..Settings::load().unwrap() };
        assert!(s.diagnostics, "on by default");
        assert_eq!(event(&s, "report_finished", json!({"ok": true})), None, "nothing before the notice");
        crate::privacy::mark_seen(&s).unwrap();
        assert!(event(&s, "report_finished", json!({"ok": true})).is_some());
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
    fn a_failing_command_is_reported_once_an_hour() {
        let (_tmp, s) = settings(true);
        assert!(!repeated(&s, "command_failed:\"session\""));
        assert!(repeated(&s, "command_failed:\"session\""), "the same again within the hour");
        assert!(!repeated(&s, "command_failed:\"report\""), "another command is its own");
    }

    #[test]
    fn lengths_and_shares_are_coarse() {
        assert_eq!(minutes(42.0 * 60.0), 45);
        assert_eq!(minutes(3.0 * 60.0), 5);
        assert_eq!(minutes(200.0 * 60.0), 90);
        assert_eq!(share(0.8734), 0.9);
    }
}
