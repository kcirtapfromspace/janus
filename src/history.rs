//! An interview's history as a tree: each transcript revision, the report versions built on it
//! (with what changed from the version each was re-run from), and the next steps planned from each.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use crate::db::{Db, StoredAnalysis};
use crate::models::{Direction, RunStatus, Step};
use crate::{trends, versions};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct History {
    pub revisions: Vec<Revision>,
    /// The version the report stage shows now.
    pub current: Option<i64>,
    pub total_versions: usize,
}

/// One transcript the reports could be built on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Revision {
    /// 1, 2, …; 0 for reports whose transcript wasn't recorded.
    pub number: usize,
    /// "transcribed", "speakers swapped by you", "speakers swapped by the analysis".
    pub how: String,
    pub created_at: String,
    /// The earlier revision with exactly the same lines (e.g. after swapping back).
    pub same_as: Option<usize>,
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Version {
    pub analysis_id: i64,
    /// v1, v2, … in the order they were made.
    pub number: usize,
    pub created_at: String,
    pub model: String,
    pub verdict: &'static str,
    pub confidence: &'static str,
    /// Your share of talk time (None when part of the call wasn't recorded).
    pub your_share: Option<f64>,
    /// Mean temperature of the interviewer's turns.
    pub room: Option<f64>,
    pub positive_signals: usize,
    pub negative_signals: usize,
    pub rubric_mean: Option<f64>,
    /// The version it was re-run from, and what's different from it.
    pub parent: Option<usize>,
    pub changes: Vec<String>,
    pub inputs_recorded: bool,
    /// How many later re-runs found nothing changed and showed this version again.
    pub reruns_reused: usize,
    /// Next-steps plans made from this version: (date, model).
    pub next_steps: Vec<(String, String)>,
}

fn how(params: &serde_json::Value) -> String {
    match (params["kind"].as_str(), params["by"].as_str()) {
        (Some("swap"), Some("analysis")) => "speakers swapped by the analysis".into(),
        (Some("swap"), _) => "speakers swapped by you".into(),
        _ => "transcribed".into(),
    }
}

fn version(a: &StoredAnalysis, number: usize) -> Version {
    let signals = |d: Direction| a.analysis.outlook.signals.iter().filter(|s| s.direction == d).count();
    Version {
        analysis_id: a.id,
        number,
        created_at: a.created_at.clone(),
        model: a.model.clone(),
        verdict: a.analysis.outlook.verdict.label(),
        confidence: a.analysis.outlook.confidence.as_str(),
        your_share: (a.metrics.your_talk_s + a.metrics.their_talk_s > 0.0).then_some(a.metrics.your_share),
        room: trends::room(a),
        positive_signals: signals(Direction::Positive),
        negative_signals: signals(Direction::Negative),
        rubric_mean: trends::rubric_mean(a),
        parent: None,
        changes: vec![],
        inputs_recorded: a.inputs.is_some(),
        reruns_reused: 0,
        next_steps: vec![],
    }
}

pub fn build(db: &Db, session_id: i64) -> Result<History> {
    let runs = db.runs(session_id)?;
    let mut analyses = db.analyses(session_id)?;
    analyses.sort_by_key(|a| a.id);
    let number: HashMap<i64, usize> = analyses.iter().enumerate().map(|(i, a)| (a.id, i + 1)).collect();
    let by_id: HashMap<i64, &StoredAnalysis> = analyses.iter().map(|a| (a.id, a)).collect();

    // Transcript revisions: every successful transcript run.
    let shas: HashMap<i64, String> = db.transcript_revisions(session_id)?.into_iter().collect();
    let mut revisions: Vec<Revision> = vec![];
    let mut revision_of_run: HashMap<i64, usize> = HashMap::new();
    let mut first_with_sha: HashMap<&str, usize> = HashMap::new();
    for run in runs.iter().filter(|r| r.step == Step::Transcript && r.status == RunStatus::Succeeded) {
        let n = revisions.len() + 1;
        let sha = shas.get(&run.id).map(String::as_str);
        let same_as = sha.and_then(|sha| first_with_sha.get(sha).copied());
        if let Some(sha) = sha {
            first_with_sha.entry(sha).or_insert(n);
        }
        revision_of_run.insert(run.id, n);
        revisions.push(Revision { number: n, how: how(&run.params), created_at: run.started_at.clone(), same_as,
                                  versions: vec![] });
    }

    // The report run that first made each version tells which transcript it was built on.
    let report_runs: Vec<_> = runs.iter().filter(|r| r.step == Step::Report && r.status == RunStatus::Succeeded).collect();
    let mut unplaced = vec![];
    for a in &analyses {
        let mut v = version(a, number[&a.id]);
        let made_by = report_runs.iter().filter(|r| r.output_id == Some(a.id)).collect::<Vec<_>>();
        v.reruns_reused = made_by.len().saturating_sub(1);
        if let Some(parent) = a.parent_id.and_then(|p| by_id.get(&p)) {
            v.parent = Some(number[&parent.id]);
            v.changes = match (&parent.inputs, &a.inputs) {
                (Some(pi), Some(ti)) => versions::changes(Some(pi), ti),
                // A version made before inputs were recorded still has its model and prompt.
                _ => {
                    let mut out = vec![];
                    if parent.model != a.model {
                        out.push(format!("model {} → {}", versions::short_model(&parent.model), versions::short_model(&a.model)));
                    }
                    if parent.prompt_version != a.prompt_version {
                        out.push(format!("prompt {} → {}", parent.prompt_version, a.prompt_version));
                    }
                    out
                }
            };
        }
        let transcript_run = a.inputs.as_ref().map(|m| m.transcript_run_id).or_else(|| made_by.first()?.input_run_id);
        match transcript_run.and_then(|r| revision_of_run.get(&r)) {
            Some(&n) => revisions[n - 1].versions.push(v),
            None => unplaced.push(v),
        }
    }
    for (_, created_at, model, analysis_id) in db.next_steps_index(session_id)? {
        if let Some(v) = revisions.iter_mut().flat_map(|r| r.versions.iter_mut()).chain(unplaced.iter_mut())
            .find(|v| Some(v.analysis_id) == analysis_id)
        {
            v.next_steps.push((created_at, model));
        }
    }
    if !unplaced.is_empty() {
        revisions.insert(0, Revision { number: 0, how: "not kept".into(), created_at: String::new(),
                                       same_as: None, versions: unplaced });
    }
    Ok(History { revisions, current: crate::pipeline::current_report(db, session_id)?.map(|r| r.id),
                 total_versions: analyses.len() })
}

impl History {
    pub fn version(&self, analysis_id: i64) -> Option<&Version> {
        self.revisions.iter().flat_map(|r| &r.versions).find(|v| v.analysis_id == analysis_id)
    }
}
