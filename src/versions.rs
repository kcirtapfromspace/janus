//! Report versions: what each report was built from, so the same inputs always give the same
//! report back.
//!
//! A report's *key* is a SHA-256 of everything that shapes Claude's answer: the model, the prompt and
//! its schema, and the exact message (transcript, call stats, recording notes). A re-run whose key
//! matches an existing version reuses it, with no new model call, so its verdict, rubric, signals
//! and call stats are identical. Scorer verdicts (Jev's turn checks, answer checks) are stored the
//! same way under their own inputs. This is storage, not model behaviour: asking a model again
//! would give a different answer, so an unchanged report isn't asked again.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::analyze;
use crate::config::ModelRef;
use crate::db::Db;
use crate::llm::schema_for;
use crate::models::{Segment, SessionAnalysis};
use crate::scoring::{Assessment, CheckSet, ScoreInput, Scorer};

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A transcript's identity: the same lines (e.g. after swapping speakers and back) give the same sha.
pub fn transcript_sha(segments: &[Segment]) -> String {
    sha256_hex(serde_json::to_string(segments).expect("segments serialize").as_bytes())
}

/// The key of a report: every input to the analysis request.
pub fn report_key(model: &ModelRef, message: &str) -> String {
    let request = json!({
        "kind": "report",
        "model": model.to_string(),
        "prompt_version": analyze::PROMPT_VERSION,
        "system": analyze::SYSTEM_PROMPT,
        "schema": schema_for::<SessionAnalysis>(),
        "effort": "high",
        "message": message,
    });
    sha256_hex(request.to_string().as_bytes())
}

/// What a report version was built from, stored with it and shown in its history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub key: String,
    /// The transcript run it was built on, and that transcript's sha.
    pub transcript_run_id: i64,
    pub transcript_sha: String,
    /// The interview's title and the company you entered, which the analysis sees.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub company: Option<String>,
    pub model: String,
    pub prompt_version: String,
    pub timeline_method: String,
    /// Which scorer answered the timeline's turn checks (e.g. `typesafe/jev-1.13.0`), if any.
    pub timeline_scorer: Option<String>,
    /// Which scorer checked the answers, if any.
    pub answer_checker: Option<String>,
    /// The app version that made it (measurements can change between versions).
    pub app_version: String,
}

/// Differences between a version's inputs and its parent's, in words.
pub fn changes(parent: Option<&Manifest>, this: &Manifest) -> Vec<String> {
    let Some(parent) = parent else { return vec![] };
    let mut out = vec![];
    if parent.transcript_sha != this.transcript_sha {
        out.push("a different transcript".into());
    }
    if parent.model != this.model {
        out.push(format!("model {} → {}", short_model(&parent.model), short_model(&this.model)));
    }
    if parent.title != this.title {
        out.push("the title".into());
    }
    if parent.company != this.company {
        out.push("the company".into());
    }
    if parent.prompt_version != this.prompt_version {
        out.push(format!("prompt {} → {}", parent.prompt_version, this.prompt_version));
    }
    if parent.app_version != this.app_version && out.is_empty() {
        out.push(format!("app {} → {}", parent.app_version, this.app_version));
    }
    if out.is_empty() && parent.key != this.key {
        out.push("the call stats or recording notes changed".into());
    }
    out
}

/// `anthropic/claude-opus-5-5` → `claude-opus-5-5`.
pub fn short_model(model: &str) -> &str {
    model.split_once('/').map_or(model, |(_, name)| name)
}

/// A scorer whose verdicts are stored under their exact inputs: the check set's wording, the
/// requested model, the option order, and the input. The same input is judged once.
pub struct CachedScorer<'a> {
    pub inner: &'a dyn Scorer,
    pub db: &'a Db,
}

impl CachedScorer<'_> {
    pub fn key(&self, input: &dyn ScoreInput, set: &CheckSet, rotation: usize) -> String {
        let request: Value = json!({
            "scorer": self.inner.name(),
            "set": set.id,
            "checks": format!("{:?}", set.checks),
            "rotation": rotation,
            "input": input.state(),
        });
        sha256_hex(request.to_string().as_bytes())
    }
}

impl Scorer for CachedScorer<'_> {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn assess(&self, input: &dyn ScoreInput, set: &CheckSet, rotation: usize) -> anyhow::Result<Assessment> {
        let key = self.key(input, set, rotation);
        if let Some(stored) = self.db.judgment(&key)? {
            return Ok(stored);
        }
        let assessment = self.inner.assess(input, set, rotation)?;
        self.db.save_judgment(&key, &self.inner.name(), &assessment)?;
        Ok(assessment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;

    fn model(name: &str) -> ModelRef {
        ModelRef { provider: Provider::Anthropic, name: name.into() }
    }

    #[test]
    fn the_key_follows_every_input() {
        let k = report_key(&model("claude-opus-5-5"), "transcript A");
        assert_eq!(k, report_key(&model("claude-opus-5-5"), "transcript A"), "stable");
        assert_eq!(k.len(), 64);
        assert_ne!(k, report_key(&model("claude-haiku-4-5"), "transcript A"), "model");
        assert_ne!(k, report_key(&model("claude-opus-5-5"), "transcript B"), "message");
    }

    fn manifest(key: &str, sha: &str, model: &str) -> Manifest {
        Manifest { key: key.into(), transcript_run_id: 1, transcript_sha: sha.into(), title: "HM screen".into(), company: None, model: model.into(),
                   prompt_version: "session-v2".into(), timeline_method: "timeline-v1".into(), timeline_scorer: None,
                   answer_checker: None, app_version: "0.1.0".into() }
    }

    #[test]
    fn changes_name_what_differs_from_the_parent() {
        let a = manifest("k1", "s1", "anthropic/claude-opus-5-5");
        assert_eq!(changes(Some(&a), &manifest("k2", "s1", "anthropic/claude-haiku-4-5")), ["model claude-opus-5-5 → claude-haiku-4-5"]);
        assert_eq!(changes(Some(&a), &manifest("k3", "s2", "anthropic/claude-opus-5-5")), ["a different transcript"]);
        assert_eq!(changes(Some(&a), &manifest("k4", "s1", "anthropic/claude-opus-5-5")), ["the call stats or recording notes changed"]);
        assert!(changes(None, &a).is_empty());
    }
}
