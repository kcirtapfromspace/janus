//! The video cues against labelled clips (docs/eval/video-decision.md): the `video-v1` and
//! `video-v1.1` arms for `ic eval scorers --set video`, and everything the rule needs beyond the
//! generic summary. That covers:
//! - coverage;
//! - designed against realistic clips;
//! - face-size and layout bands;
//! - positives;
//! - "who is you";
//! - a threshold sweep for tuning, on designed clips only.
//!
//! Plus `ic eval video-health`: how well capture and face reading went across recordings.
//!
//! Each arm scores a clip the way the review would. Faces are read densely once per interview
//! (`faces-dense.json`, 10 a second) and resampled to the review's 6 a second, from three starting
//! points: one per run. A cue that changes when its frames shift by a fraction of a second is on a
//! threshold's edge, and the rule's stability requirement catches it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::eval::{self, ArmStats, Item, Prediction, RawResult};
use crate::models::{Segment, Session, fmt_ts};
use crate::progress::Progress;
use crate::scoring::{Assessment, Check, CheckKind, CheckSet, Rule, ScoreInput, Scorer, Verdict};
use crate::temperature::{self, Kind};
use crate::video::{self, Faces, Params, Sample, Speech, Track, Who};

/// Dense faces for the eval, next to the review's `faces.json`.
pub const DENSE_FILE: &str = "faces-dense.json";
pub const DENSE_FPS: f64 = 10.0;
/// Runs per clip, each sampling from a different starting point.
pub const RUNS: usize = 3;
/// `nod_count` labels, lowest first, scored as levels 1–4.
pub const NOD_LEVELS: [&str; 4] = ["none", "1-2", "3-5", "6+"];
/// Face-size bands, as fractions of the frame's height.
pub const BANDS: [(&str, f64, f64); 3] = [("small (under 8%)", 0.0, 0.08), ("medium (8–15%)", 0.08, 0.15), ("large (over 15%)", 0.15, 1.01)];
/// The rule's extra conditions (docs/eval/video-decision.md).
pub const MIN_POSITIVES: usize = 30;
pub const MAX_DROP: f64 = 0.10;
pub const BAND_FLOOR: f64 = 0.75;
/// A band needs this many clips before it can hide a cue.
const MIN_BAND_CLIPS: usize = 10;

fn yes_no(id: &'static str, instructions: &str, yes: &str, no: &str) -> Check {
    Check { id, instructions: instructions.into(), kind: CheckKind::YesNo { yes: yes.into(), no: no.into() } }
}

fn choice(id: &'static str, instructions: &str, options: &[(&str, &str)]) -> Check {
    Check {
        id,
        instructions: instructions.into(),
        kind: CheckKind::Choice { options: options.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() },
    }
}

/// The clip checks and their rule, as docs/eval/video-decision.md writes them.
pub fn video_set() -> CheckSet {
    CheckSet {
        id: "video",
        checks: vec![
            yes_no("nodded", "Did anyone else nod at least once?", "At least one deliberate down-and-up of the head.", "No nod."),
            Check {
                id: "nod_count",
                instructions: "How many nods, across everyone else?".into(),
                kind: CheckKind::Level { levels: vec!["None".into(), "1–2".into(), "3–5".into(), "6 or more".into()] },
            },
            choice("looked_away", "How much of their time on camera were they turned away from the screen?",
                   &[("rarely", "Under 10%."), ("sometimes", "10–40%."), ("mostly", "Over 40%.")]),
            choice("on_camera", "How many others were on camera for most of the clip?", &[("0", "0"), ("1", "1"), ("2", "2"), ("3+", "3 or more")]),
            yes_no("smiled", "Did anyone else visibly smile?", "Lip corners visibly raised.", "No smile."),
        ],
        claude_prompt: "",
        classify: |_, _| "unclear",
        rule: Rule {
            min_score: 0.85,
            min_score_overrides: &[("looked_away", 0.80), ("on_camera", 0.90)],
            min_within_one: 0.90,
            // Each arm is judged on its own: v1 is kept to measure what v1.1 changed.
            max_behind_best: 1.0,
            min_stability: 0.95,
            max_p95_ms: None,
            max_ece: None,
        },
        input_from: |m| {
            let dir = m.get("session_dir").and_then(Value::as_str).context("clip has no session_dir")?;
            let (start, end) = (m.get("start").and_then(Value::as_f64), m.get("end").and_then(Value::as_f64));
            let (Some(start), Some(end)) = (start, end) else { bail!("clip has no start and end") };
            Ok(Box::new(ClipInput { session_dir: dir.to_string(), start, end }))
        },
    }
}

/// One clip to score: a window of one interview's video.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipInput {
    pub session_dir: String,
    pub start: f64,
    pub end: f64,
}

impl ScoreInput for ClipInput {
    fn state(&self) -> Value {
        json!({"session_dir": self.session_dir, "start": self.start, "end": self.end})
    }

    fn claude_body(&self) -> String {
        String::new()
    }
}

/// The labelled clips from `ic eval label`, ready to score: unlabelled ones left out, and
/// `nod_count` mapped to its level.
pub fn load_clips(path: &Path) -> Result<Vec<Item>> {
    let mut items = eval::load_items(path)?;
    items.retain(|i| i.input.get("labelled_at").is_some_and(|v| !v.is_null()));
    for item in &mut items {
        if let Some(Value::String(count)) = item.labels.get("nod_count").cloned() {
            let level = NOD_LEVELS.iter().position(|l| *l == count).with_context(|| format!("{}: nod_count {count:?}", item.id))?;
            item.labels.insert("nod_count".into(), json!((level + 1).to_string()));
        }
    }
    Ok(items)
}

/// "designed" (staged calls, for tuning) or "realistic" (the test).
pub fn origin(item: &Item) -> &str {
    &item.origin
}

// --- scoring --------------------------------------------------------------------------------------

/// Re-sample dense faces the way the review samples the video: a frame is taken at the first
/// grid point at or after it, and the grid (`fps`, starting at `offset`) moves past it.
pub fn resample(dense: &Faces, fps: f64, offset: f64) -> Faces {
    let interval = 1.0 / fps;
    let mut next = offset;
    let mut samples: Vec<Sample> = vec![];
    for s in &dense.samples {
        if s.t + 1e-6 < next {
            continue;
        }
        samples.push(s.clone());
        next += ((s.t + 1e-6 - next) / interval).floor().max(0.0) * interval;
        while next <= s.t + 1e-6 {
            next += interval;
        }
    }
    Faces { version: 1, fps, samples, complete: dense.complete, skipped: dense.skipped }
}

/// The review's sampling grid, shifted for each run.
pub fn offset(run: usize) -> f64 {
    (run % RUNS) as f64 / RUNS as f64 / video::SAMPLES_PER_S
}

/// One interview, read the way one arm and run read it.
pub struct Prepared {
    pub faces: Faces,
    pub tracks: Vec<Track>,
    pub labels: Vec<Who>,
    pub nods: Vec<Vec<f64>>,
}

impl Prepared {
    pub fn others(&self) -> Vec<(&Track, Vec<f64>)> {
        self.tracks.iter().zip(&self.labels).zip(&self.nods).filter(|((_, w), _)| **w == Who::Other).map(|((t, _), n)| (t, n.clone())).collect()
    }
}

fn dense(dir: &str) -> Result<Arc<Faces>> {
    static DENSE: OnceLock<Mutex<HashMap<String, Arc<Faces>>>> = OnceLock::new();
    let cache = DENSE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().expect("dense cache").get(dir) {
        return Ok(hit.clone());
    }
    let path = Path::new(dir).join(DENSE_FILE);
    let faces: Faces = serde_json::from_str(&std::fs::read_to_string(&path).with_context(|| {
        format!("{} is missing: ic eval scorers --set video reads each interview's faces first", path.display())
    })?)?;
    let faces = Arc::new(faces);
    cache.lock().expect("dense cache").insert(dir.to_string(), faces.clone());
    Ok(faces)
}

fn segments(dir: &str) -> Result<Vec<Segment>> {
    let path = Path::new(dir).join("transcript.json");
    serde_json::from_str(&std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?)
        .with_context(|| format!("reading {}", path.display()))
}

/// Read one interview for an arm and run, without caching (the sweep's many variants).
pub fn prepare(dir: &str, run: usize, p: &Params) -> Result<Prepared> {
    let faces = resample(&*dense(dir)?, video::SAMPLES_PER_S, offset(run));
    let speech = Speech::from_segments(&segments(dir)?);
    let tracks = video::tracks(&faces, p);
    let labels = video::labels(&tracks, &speech, p);
    let nods = tracks.iter().map(|t| video::nods(t, p)).collect();
    Ok(Prepared { faces, tracks, labels, nods })
}

fn prepared(dir: &str, run: usize, p: &Params) -> Result<Arc<Prepared>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Prepared>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = format!("{dir}|{}|{p:?}", run % RUNS);
    if let Some(hit) = cache.lock().expect("prepared cache").get(&key) {
        return Ok(hit.clone());
    }
    let ready = Arc::new(prepare(dir, run, p)?);
    cache.lock().expect("prepared cache").insert(key, ready.clone());
    Ok(ready)
}

fn verdict(pick: String, value: f64, probabilities: BTreeMap<String, f64>) -> Verdict {
    Verdict { pick, value, confidence: None, probabilities }
}

/// The checks' answers from one clip's cues; a check the cues can't answer is left out (the clip
/// abstains, which the coverage table counts).
pub fn verdicts(cues: Option<&video::VideoCues>) -> BTreeMap<String, Verdict> {
    let mut out = BTreeMap::new();
    let Some(c) = cues else { return out };
    let yes = c.nods > 0;
    out.insert("nodded".into(), verdict(if yes { "yes" } else { "no" }.into(), yes as u8 as f64,
                                        BTreeMap::from([("yes".into(), yes as u8 as f64), ("no".into(), 1.0 - yes as u8 as f64)])));
    let level = match c.nods {
        0 => 1,
        1..=2 => 2,
        3..=5 => 3,
        _ => 4,
    };
    out.insert("nod_count".into(), verdict(level.to_string(), level as f64, BTreeMap::from([(level.to_string(), 1.0)])));
    let people = match c.on_camera {
        x if x < 0.5 => "0",
        x if x < 1.5 => "1",
        x if x < 2.5 => "2",
        _ => "3+",
    };
    out.insert("on_camera".into(), verdict(people.into(), 1.0, BTreeMap::from([(people.into(), 1.0)])));
    if let Some(away) = c.looking_away {
        let pick = if away < 0.10 { "rarely" } else if away <= 0.40 { "sometimes" } else { "mostly" };
        out.insert("looked_away".into(), verdict(pick.into(), 1.0, BTreeMap::from([(pick.into(), 1.0)])));
    }
    out
}

/// A local arm: `video.rs` with one version's `Params`. `rotation` is the run, which shifts the
/// sampling grid.
pub struct VideoScorer {
    pub params: Params,
}

impl Scorer for VideoScorer {
    fn name(&self) -> String {
        format!("local/{}", self.params.name)
    }

    fn assess(&self, input: &dyn ScoreInput, _set: &CheckSet, rotation: usize) -> Result<Assessment> {
        let started = Instant::now();
        let state = input.state();
        let dir = state["session_dir"].as_str().context("which interview?")?;
        let (start, end) = (state["start"].as_f64().context("start")?, state["end"].as_f64().context("end")?);
        let ready = prepared(dir, rotation, &self.params)?;
        let cues = video::cues(&ready.faces, &ready.others(), start, end, &self.params);
        Ok(Assessment { scorer: self.name(), verdicts: verdicts(cues.as_ref()), latency_ms: started.elapsed().as_millis() as u64, input_tokens: 0 })
    }
}

/// Read every interview's faces densely, once (the eval's slowest step: about a third of a video's
/// length on Apple silicon). Interviews already read are skipped.
pub fn read_dense(dirs: &[String], progress: &mut dyn Progress) -> Result<Vec<String>> {
    let mut warnings = vec![];
    for dir in dirs {
        let dir = Path::new(dir);
        if dir.join(DENSE_FILE).exists() {
            continue;
        }
        let video_file = dir.join(video::VIDEO_FILE);
        if !video_file.is_file() {
            bail!("{} is missing", video_file.display());
        }
        progress.stage(&format!("Reading the faces in {}", dir.file_name().and_then(|n| n.to_str()).unwrap_or("an interview")));
        warnings.extend(video::extract_with(&video_file, &dir.join(DENSE_FILE), DENSE_FPS, &[], progress)?);
    }
    Ok(warnings)
}

/// The interviews a set of clips and you-marks come from.
pub fn session_dirs(items: &[Item], marks: &[Value]) -> Vec<String> {
    let mut dirs: Vec<String> = items
        .iter()
        .filter_map(|i| i.input.get("session_dir")?.as_str().map(String::from))
        .chain(marks.iter().filter_map(|m| m["session_dir"].as_str().map(String::from)))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

// --- the report -----------------------------------------------------------------------------------

fn pct(v: Option<f64>) -> String {
    v.map_or("—".into(), |v| format!("{:.0}%", v * 100.0))
}

fn subset(results: &[RawResult], items: &[Item], keep: impl Fn(&Item) -> bool) -> (Vec<RawResult>, Vec<Item>) {
    let items: Vec<Item> = items.iter().filter(|i| keep(i)).cloned().collect();
    let ids: HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
    (results.iter().filter(|r| ids.contains(r.item.as_str())).cloned().collect(), items)
}

fn score(stats: &[ArmStats], arm: &str, check: &str) -> Option<f64> {
    stats.iter().find(|s| s.arm == arm)?.checks.get(check)?.score
}

/// How many of each label a check has: the rule wants 30 of every answer in the realistic set.
pub fn positives(items: &[Item], check: &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in items {
        if let Some(expected) = item.expected(check) {
            *counts.entry(expected).or_default() += 1;
        }
    }
    counts
}

/// The answers a check's 30-each requirement covers (yes/no and choices; a level check needs 30 clips).
fn required_answers(check: &Check) -> Vec<String> {
    match &check.kind {
        CheckKind::YesNo { .. } => vec!["yes".into(), "no".into()],
        CheckKind::Choice { options } => options.iter().map(|(id, _)| id.clone()).collect(),
        CheckKind::Level { .. } => vec![],
    }
}

/// One arm's verdict on one check under the whole rule.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoDecision {
    pub check: String,
    pub arm: String,
    pub realistic: Option<f64>,
    pub designed: Option<f64>,
    /// Empty: passed.
    pub reasons: Vec<String>,
    /// Too little realistic data to decide, as opposed to failing.
    pub undecided: bool,
    /// Faces below this height should hide the cue (a size band under the floor).
    pub hide_below: Option<f64>,
}

/// The rule (docs/eval/video-decision.md): `eval::decide` on the realistic clips, plus the
/// designed→realistic drop, the 30-each positives and the size bands.
pub fn decisions(results: &[RawResult], items: &[Item], arms: &[eval::Arm], set: &CheckSet) -> Vec<VideoDecision> {
    let (real_results, real_items) = subset(results, items, |i| origin(i) == "realistic");
    let (des_results, des_items) = subset(results, items, |i| origin(i) == "designed");
    let real_stats = eval::summarize(&real_results, &real_items, arms, set);
    let des_stats = eval::summarize(&des_results, &des_items, arms, set);
    let base = eval::decide(&real_stats, arms, set);
    let mut out = vec![];
    for check in &set.checks {
        let counts = positives(&real_items, check.id);
        let labelled = counts.values().sum::<usize>();
        let mut short: Vec<String> = required_answers(check)
            .into_iter()
            .filter(|a| counts.get(a).copied().unwrap_or(0) < MIN_POSITIVES)
            .map(|a| format!("{a}: {} of {MIN_POSITIVES}", counts.get(&a).copied().unwrap_or(0)))
            .collect();
        // A level check has no answers to count, but still needs as many clips as one answer would.
        if matches!(check.kind, CheckKind::Level { .. }) && labelled < MIN_POSITIVES {
            short.push(format!("{labelled} of {MIN_POSITIVES} clips"));
        }
        for arm in arms {
            let realistic = score(&real_stats, &arm.name, check.id);
            let designed = score(&des_stats, &arm.name, check.id);
            let mut reasons: Vec<String> = base
                .iter()
                .find(|d| d.check == check.id)
                .and_then(|d| d.reasons.get(&arm.name).cloned())
                .unwrap_or_default();
            let undecided = labelled == 0 || realistic.is_none() || !short.is_empty();
            if labelled == 0 || realistic.is_none() {
                reasons = vec!["not scored on any realistic clip yet".into()];
            } else if !short.is_empty() {
                reasons.push(format!("too few realistic examples ({})", short.join(", ")));
            }
            if let (Some(r), Some(d)) = (realistic, designed)
                && d - r > MAX_DROP
            {
                reasons.push(format!("{:.2} lower on realistic clips than designed ones (limit {MAX_DROP})", d - r));
            }
            let hide_below = band_scores(&real_results, &real_items, &arm.name, check)
                .iter()
                .filter(|(_, n, s)| *n >= MIN_BAND_CLIPS && s.is_some_and(|s| s < BAND_FLOOR))
                .map(|((_, _, hi), _, _)| *hi)
                .fold(None, |acc: Option<f64>, hi| Some(acc.map_or(hi, |a| a.max(hi))));
            out.push(VideoDecision { check: check.id.into(), arm: arm.name.clone(), realistic, designed, reasons, undecided, hide_below });
        }
    }
    out
}

fn check_score(preds: &[Prediction], check: &Check) -> Option<f64> {
    if matches!(check.kind, CheckKind::Level { .. }) { eval::within_one(preds) } else { eval::balanced_accuracy(preds) }
}

/// A face-size band: its label and bounds.
type Band = (&'static str, f64, f64);

/// A check's score within each face-size band: (band, clips, score).
fn band_scores(results: &[RawResult], items: &[Item], arm: &str, check: &Check) -> Vec<(Band, usize, Option<f64>)> {
    BANDS
        .iter()
        .map(|&(name, lo, hi)| {
            let (r, i) = subset(results, items, |item| {
                item.input.get("face_height").and_then(Value::as_f64).is_some_and(|h| h >= lo && h < hi)
            });
            let preds = eval::predictions(&r, &i, arm, check.id);
            let clips: HashSet<&str> = preds.iter().map(|p| p.item.as_str()).collect();
            ((name, lo, hi), clips.len(), check_score(&preds, check))
        })
        .collect()
}

fn layout_scores(results: &[RawResult], items: &[Item], arm: &str, check: &Check) -> Vec<(String, usize, Option<f64>)> {
    ["gallery", "speaker", "other"]
        .iter()
        .map(|layout| {
            let (r, i) = subset(results, items, |item| item.input.get("layout").and_then(Value::as_str) == Some(layout));
            let preds = eval::predictions(&r, &i, arm, check.id);
            let clips: HashSet<&str> = preds.iter().map(|p| p.item.as_str()).collect();
            (layout.to_string(), clips.len(), check_score(&preds, check))
        })
        .collect()
}

/// The video-specific sections of `summary.md`, after the generic ones.
pub fn markdown(results: &[RawResult], items: &[Item], arms: &[eval::Arm], set: &CheckSet, decisions: &[VideoDecision]) -> String {
    let mut md = String::new();
    let (real_results, real_items) = subset(results, items, |i| origin(i) == "realistic");
    let (bands_from, band_items, band_note) = if real_items.is_empty() {
        (results.to_vec(), items.to_vec(), " (designed clips: no realistic ones yet)")
    } else {
        (real_results.clone(), real_items.clone(), " (realistic clips)")
    };

    md += "\n## Coverage\n\nClips labelled for a check, and how many each arm answered. An arm abstains when the video \
           covered too little of a clip, or when no face's angle was measured (looking away).\n\n\
           | Check | Arm | Labelled | Answered | Abstained |\n|---|---|---|---|---|\n";
    for check in &set.checks {
        let labelled: HashSet<&str> = items.iter().filter(|i| i.expected(check.id).is_some()).map(|i| i.id.as_str()).collect();
        for arm in arms {
            let answered: HashSet<String> = eval::predictions(results, items, &arm.name, check.id).into_iter().map(|p| p.item).collect();
            md += &format!("| {} | {} | {} | {} | {} |\n", check.id, arm.name, labelled.len(), answered.len(),
                           labelled.len().saturating_sub(answered.len()));
        }
    }

    md += "\n## The rule, check by check\n\nOn the realistic clips: the floor and stability (`eval::decide`), at least \
           30 of every answer, and no more than 0.10 below the designed clips.\n\n\
           | Check | Arm | Realistic | Designed | Result | Why |\n|---|---|---|---|---|---|\n";
    for d in decisions {
        let result = if d.reasons.is_empty() { "**passes**" } else if d.undecided { "not yet" } else { "fails" };
        md += &format!("| {} | {} | {} | {} | {result} | {} |\n", d.check, d.arm, pct(d.realistic), pct(d.designed), d.reasons.join("; "));
    }

    md += &format!("\n## By face size{band_note}\n\nA band under {:.0}% with at least {MIN_BAND_CLIPS} clips hides that cue for faces \
                    that small.\n\n| Check | Arm | {} |\n|---|---|---|---|---|\n",
                   BAND_FLOOR * 100.0, BANDS.iter().map(|b| b.0).collect::<Vec<_>>().join(" | "));
    for check in &set.checks {
        for arm in arms {
            let cells: Vec<String> = band_scores(&bands_from, &band_items, &arm.name, check)
                .into_iter()
                .map(|(_, n, s)| if n == 0 { "—".into() } else { format!("{} ({n})", pct(s)) })
                .collect();
            md += &format!("| {} | {} | {} |\n", check.id, arm.name, cells.join(" | "));
        }
    }

    md += &format!("\n## By layout{band_note}\n\nReported, not a gate: the review can't tell the layout.\n\n\
                    | Check | Arm | Gallery | Speaker view | Other |\n|---|---|---|---|---|\n");
    for check in &set.checks {
        for arm in arms {
            let cells: Vec<String> = layout_scores(&bands_from, &band_items, &arm.name, check)
                .into_iter()
                .map(|(_, n, s)| if n == 0 { "—".into() } else { format!("{} ({n})", pct(s)) })
                .collect();
            md += &format!("| {} | {} | {} |\n", check.id, arm.name, cells.join(" | "));
        }
    }

    md += "\n## Labels\n\n| Check | Origin | Counts |\n|---|---|---|\n";
    for check in &set.checks {
        for which in ["designed", "realistic"] {
            let of: Vec<Item> = items.iter().filter(|i| origin(i) == which).cloned().collect();
            let counts = positives(&of, check.id);
            if !counts.is_empty() {
                let text: Vec<String> = counts.iter().map(|(k, n)| format!("{k}: {n}")).collect();
                md += &format!("| {} | {which} | {} |\n", check.id, text.join(", "));
            }
        }
    }
    md
}

/// The `GATE` the decisions suggest for the review's arm, as code to paste into video.rs once the
/// rule has really been met. Undecided checks stay untested (shown as experimental).
pub fn suggested_gate(decisions: &[VideoDecision], arm: &str) -> String {
    let mine: Vec<&VideoDecision> = decisions.iter().filter(|d| d.arm == arm).collect();
    let list = |f: &dyn Fn(&VideoDecision) -> bool| -> String {
        mine.iter().filter(|d| f(d)).map(|d| format!("{:?}", d.check)).collect::<Vec<_>>().join(", ")
    };
    let passed = list(&|d| d.reasons.is_empty());
    let failed = list(&|d| !d.reasons.is_empty() && !d.undecided);
    let hidden: Vec<String> = mine
        .iter()
        .filter(|d| d.reasons.is_empty())
        .filter_map(|d| Some(format!("({:?}, {:.2})", d.check, d.hide_below?)))
        .collect();
    format!("pub const GATE: Gate = Gate {{ passed: &[{passed}], failed: &[{failed}], hidden_below: &[{}] }};", hidden.join(", "))
}

// --- corrections from reviews ---------------------------------------------------------------------

/// How the corrections made in reviews compare with what was measured then, and with each arm now.
/// Corrections are biased (people correct what looks wrong), so they never count towards the rule:
/// they show where the cues fail in real reviews, and which fixes would help.
pub fn corrections_markdown(results: &[RawResult], corrections: &[Item], arms: &[eval::Arm], set: &CheckSet) -> String {
    if corrections.is_empty() {
        return "\n## Corrections from reviews\n\nNone yet. In Janus, Correct video cues on a review adds one.\n".into();
    }
    let mut md = format!(
        "\n## Corrections from reviews\n\n{} answers corrected. Not part of the rule: people correct what looks wrong. \
         \"Then\" is the review's measurement when it was corrected; each arm's column is how often it agrees with the \
         correction now.\n\n| Check | Corrected | Then wrong | {} |\n|---|---|---|{}\n",
        corrections.len(),
        arms.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(" | "),
        "---|".repeat(arms.len())
    );
    for check in &set.checks {
        let labelled: Vec<&Item> = corrections.iter().filter(|i| i.expected(check.id).is_some()).collect();
        if labelled.is_empty() {
            continue;
        }
        let wrong_then = labelled
            .iter()
            .filter(|i| {
                let measured: Option<video::VideoCues> = i.input.get("measured").and_then(|m| serde_json::from_value(m.clone()).ok());
                verdicts(measured.as_ref()).get(check.id).map(|v| v.pick.clone()) != i.expected(check.id)
            })
            .count();
        let now: Vec<String> = arms
            .iter()
            .map(|arm| {
                let preds = eval::predictions(results, corrections, &arm.name, check.id);
                let agree = preds.iter().filter(|p| p.pick == p.expected).count();
                if preds.is_empty() { "—".into() } else { pct(Some(agree as f64 / preds.len() as f64)) }
            })
            .collect();
        md += &format!("| {} | {} | {wrong_then} | {} |\n", check.id, labelled.len(), now.join(" | "));
    }
    md
}

// --- who is you -----------------------------------------------------------------------------------

/// One interview's "which face is yours", scored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct YouRow {
    pub set: String,
    pub visible: bool,
    /// What the arm called the track at your mark (None: no face was found there).
    pub yours: Option<String>,
    /// Other faces on screen at that moment, and how many of them it called you.
    pub others: usize,
    pub others_as_you: usize,
    /// With your face not in the video: tracks it called you anyway.
    pub phantom_you: usize,
}

fn contains(face: &video::Face, x: f64, y: f64) -> bool {
    x >= face.x && x <= face.x + face.w && y >= face.y && y <= face.y + face.h
}

/// Score the you-marks against one arm (run 0).
pub fn you_rows(marks: &[Value], p: &Params) -> Result<Vec<YouRow>> {
    let mut rows = vec![];
    for mark in marks {
        let set = mark["set"].as_str().unwrap_or_default().to_string();
        let dir = mark["session_dir"].as_str().context("you-mark has no session_dir")?;
        let ready = prepare(dir, 0, p)?;
        let visible = mark["labels"]["you_visible"].as_bool().unwrap_or(false);
        let name = |w: Who| format!("{w:?}").to_lowercase();
        if !visible {
            let phantom_you = ready.labels.iter().filter(|w| **w == Who::You).count();
            rows.push(YouRow { set, visible, yours: None, others: 0, others_as_you: 0, phantom_you });
            continue;
        }
        let (at, x, y) = (mark["at"].as_f64().unwrap_or(0.0), mark["x"].as_f64().unwrap_or(-1.0), mark["y"].as_f64().unwrap_or(-1.0));
        let near = |t: &Track| t.points.iter().any(|(pt, _)| (pt - at).abs() <= 0.5);
        let mine = ready.tracks.iter().position(|t| t.points.iter().any(|(pt, f)| (pt - at).abs() <= 0.5 && contains(f, x, y)));
        let others: Vec<usize> = (0..ready.tracks.len()).filter(|&i| Some(i) != mine && near(&ready.tracks[i])).collect();
        rows.push(YouRow {
            set,
            visible,
            yours: mine.map(|i| name(ready.labels[i])),
            others: others.len(),
            others_as_you: others.iter().filter(|&&i| ready.labels[i] == Who::You).count(),
            phantom_you: 0,
        });
    }
    Ok(rows)
}

/// The "who is you" section: accuracy over the tracks the marks check, and your face counted as
/// someone else (the limit that matters: your head movements would become "their nods").
pub fn you_markdown(rows_by_arm: &[(String, Vec<YouRow>)]) -> String {
    let mut md = String::from(
        "\n## Who is you\n\nFrom `you.jsonl`. The rule: accuracy ≥ 95% over the tracks checked, and you counted as \
         someone else in at most 5% of interviews.\n\n\
         | Arm | Interviews | Accuracy | You as someone else | You not told apart | No face at your mark | Someone else as you |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for (arm, rows) in rows_by_arm {
        let visible: Vec<&YouRow> = rows.iter().filter(|r| r.visible).collect();
        let checked: usize = visible.iter().map(|r| r.yours.is_some() as usize + r.others).sum::<usize>()
            + rows.iter().filter(|r| !r.visible).count();
        let right: usize = visible.iter().map(|r| (r.yours.as_deref() == Some("you")) as usize + r.others - r.others_as_you).sum::<usize>()
            + rows.iter().filter(|r| !r.visible && r.phantom_you == 0).count();
        let as_other = visible.iter().filter(|r| r.yours.as_deref() == Some("other")).count();
        let unknown = visible.iter().filter(|r| r.yours.as_deref() == Some("unknown")).count();
        let missed = visible.iter().filter(|r| r.yours.is_none()).count();
        let swapped: usize = visible.iter().map(|r| r.others_as_you).sum::<usize>() + rows.iter().map(|r| r.phantom_you).sum::<usize>();
        let share = |n: usize| if visible.is_empty() { "—".to_string() } else { format!("{n} ({})", pct(Some(n as f64 / visible.len() as f64))) };
        md += &format!("| {arm} | {} | {} | {} | {} | {} | {swapped} |\n", rows.len(),
                       if checked == 0 { "—".into() } else { pct(Some(right as f64 / checked as f64)) },
                       share(as_other), share(unknown), share(missed));
    }
    md
}

// --- tuning ---------------------------------------------------------------------------------------

/// One variant of the review's parameters, scored on the designed clips (run 0).
#[derive(Debug, Clone, Serialize)]
pub struct SweepRow {
    pub change: String,
    pub scores: BTreeMap<String, Option<f64>>,
}

/// What each threshold change would win or lose, on the designed clips only: thresholds may be
/// tuned there, never on the realistic clips that test them.
pub fn sweep(items: &[Item], base: &Params, set: &CheckSet) -> Result<Vec<SweepRow>> {
    let designed: Vec<&Item> = items.iter().filter(|i| origin(i) == "designed").collect();
    if designed.is_empty() {
        return Ok(vec![]);
    }
    let mut variants: Vec<(String, Params)> = vec![("as is".into(), *base)];
    for v in [0.03, 0.04, 0.05, 0.08, 0.10] {
        variants.push((format!("nod_drop {v}"), Params { nod_drop: v, ..*base }));
    }
    for v in [0.5, 1.2] {
        variants.push((format!("nod_window_s {v}"), Params { nod_window_s: v, ..*base }));
    }
    for v in [20.0, 25.0, 35.0, 45.0] {
        variants.push((format!("away_yaw {v}"), Params { away_yaw: v, ..*base }));
    }
    for v in [15.0, 20.0, 30.0, 35.0] {
        variants.push((format!("away_pitch {v}"), Params { away_pitch: v, ..*base }));
    }
    for v in [0.03, 0.06] {
        variants.push((format!("min_face_h {v}"), Params { min_face_h: v, ..*base }));
    }
    let mut rows = vec![];
    for (change, p) in variants {
        let mut by_dir: HashMap<&str, Prepared> = HashMap::new();
        let mut preds: BTreeMap<&str, Vec<Prediction>> = BTreeMap::new();
        for item in &designed {
            let Some(dir) = item.input.get("session_dir").and_then(Value::as_str) else { continue };
            let (Some(start), Some(end)) = (item.input.get("start").and_then(Value::as_f64), item.input.get("end").and_then(Value::as_f64))
            else {
                continue;
            };
            if !by_dir.contains_key(dir) {
                by_dir.insert(dir, prepare(dir, 0, &p)?);
            }
            let ready = &by_dir[dir];
            let answers = verdicts(video::cues(&ready.faces, &ready.others(), start, end, &p).as_ref());
            for check in &set.checks {
                if let (Some(expected), Some(v)) = (item.expected(check.id), answers.get(check.id)) {
                    preds.entry(check.id).or_default().push(Prediction {
                        item: item.id.clone(), run: 0, expected, pick: v.pick.clone(), p_pick: None, p_yes: None,
                    });
                }
            }
        }
        let scores = set.checks.iter().map(|c| (c.id.to_string(), preds.get(c.id).and_then(|p| check_score(p, c)))).collect();
        rows.push(SweepRow { change, scores });
    }
    Ok(rows)
}

pub fn sweep_markdown(rows: &[SweepRow], set: &CheckSet, base: &Params) -> String {
    if rows.is_empty() {
        return "\n## Tuning\n\nNo designed clips labelled yet, so there's nothing to tune on.\n".into();
    }
    let ids: Vec<&str> = set.checks.iter().map(|c| c.id).filter(|id| rows.iter().any(|r| r.scores.get(*id).is_some_and(|s| s.is_some()))).collect();
    let mut md = format!("\n## Tuning ({}, designed clips only)\n\nEach row changes one threshold. A change that helps here \
                          must then pass the rule on realistic clips, as a new method name.\n\n| Change | {} |\n|---|{}\n",
                         base.name, ids.join(" | "), "---|".repeat(ids.len()));
    for row in rows {
        let cells: Vec<String> = ids.iter().map(|id| pct(row.scores.get(*id).copied().flatten())).collect();
        md += &format!("| {} | {} |\n", row.change, cells.join(" | "));
    }
    md
}

// --- health ---------------------------------------------------------------------------------------

/// How capture and face reading went for one recording with video, for `ic eval video-health`.
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub id: i64,
    pub title: String,
    pub video_s: Option<f64>,
    pub frames: Option<u64>,
    pub dropped: Option<u64>,
    pub first_frame_s: Option<f64>,
    pub windows: Vec<String>,
    pub faces_read: bool,
    pub complete: bool,
    pub samples: usize,
    /// Share of samples with at least one readable face.
    pub with_faces: Option<f64>,
    pub face_h: Option<f64>,
    pub tracks: usize,
    pub you_found: bool,
    /// People besides you typically on screen at once.
    pub others: Option<usize>,
    /// Share of face samples whose track couldn't be told apart (left out of the cues).
    pub unknown: Option<f64>,
    pub answers: usize,
    pub answers_with_cues: usize,
    pub nods: usize,
    pub problems: Vec<String>,
}

pub fn health(session: &Session, segments: &[Segment]) -> Result<Health> {
    let dir = Path::new(&session.dir);
    let report = crate::capture::read_report(dir).unwrap_or(Value::Null);
    let rv = &report["video"];
    let mut h = Health {
        id: session.id,
        title: session.title.clone(),
        video_s: rv["duration_seconds"].as_f64(),
        frames: rv["frames"].as_u64(),
        dropped: rv["dropped_frames"].as_u64(),
        first_frame_s: rv["first_frame_seconds"].as_f64(),
        windows: rv["windows"].as_array().map(|w| w.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        faces_read: false,
        complete: true,
        samples: 0,
        with_faces: None,
        face_h: None,
        tracks: 0,
        you_found: false,
        others: None,
        unknown: None,
        answers: 0,
        answers_with_cues: 0,
        nods: 0,
        problems: vec![],
    };
    for w in report["warnings"].as_array().into_iter().flatten() {
        if w["code"].as_str().is_some_and(|c| c.starts_with("video_")) {
            h.problems.push(w["message"].as_str().unwrap_or_default().to_string());
        }
    }
    if let (Some(frames), Some(dropped)) = (h.frames, h.dropped)
        && dropped * 20 > frames + dropped
    {
        h.problems.push(format!("{dropped} frames dropped ({:.0}%): the Mac was too busy to encode them all", dropped as f64 * 100.0 / (frames + dropped) as f64));
    }
    if let Some(first) = h.first_frame_s.filter(|f| *f > 60.0) {
        h.problems.push(format!("the call window first appeared at {}; the video is black before that", fmt_ts(first)));
    }
    let Some(faces) = video::load(dir)? else {
        h.problems.push("the video's faces haven't been read: ic run recording to read them".into());
        return Ok(h);
    };
    let p = video::CURRENT;
    h.faces_read = true;
    h.complete = faces.complete;
    h.samples = faces.samples.len();
    if !faces.complete {
        h.problems.push("the video couldn't be read to the end".into());
    }
    if !faces.samples.is_empty() {
        let with = faces.samples.iter().filter(|s| s.faces.iter().any(|f| f.h >= p.min_face_h)).count();
        h.with_faces = Some(with as f64 / faces.samples.len() as f64);
        if with * 2 < faces.samples.len() {
            h.problems.push(format!("no readable face in {:.0}% of the video", (1.0 - with as f64 / faces.samples.len() as f64) * 100.0));
        }
    }
    let mut heights: Vec<f64> = faces.samples.iter().flat_map(|s| s.faces.iter().map(|f| f.h)).filter(|h| *h >= p.min_face_h).collect();
    if !heights.is_empty() {
        heights.sort_by(f64::total_cmp);
        let median = heights[heights.len() / 2];
        h.face_h = Some((median * 1000.0).round() / 1000.0);
        if median < 0.08 {
            h.problems.push(format!("faces are small (typically {:.0}% of the frame's height): nods are hard to see", median * 100.0));
        }
    }
    let speech = Speech::from_segments(segments);
    let tracks = video::tracks(&faces, &p);
    let labels = video::labels(&tracks, &speech, &p);
    h.tracks = tracks.len();
    h.you_found = labels.contains(&Who::You);
    let points = |w: Who| tracks.iter().zip(&labels).filter(|(_, l)| **l == w).map(|(t, _)| t.points.len()).sum::<usize>();
    let all = tracks.iter().map(|t| t.points.len()).sum::<usize>();
    if all > 0 {
        h.unknown = Some(points(Who::Unknown) as f64 / all as f64);
    }
    let mut per_sample: BTreeMap<u64, usize> = BTreeMap::new();
    for (t, l) in tracks.iter().zip(&labels) {
        if *l == Who::Other {
            for (at, _) in &t.points {
                *per_sample.entry((at * 1000.0) as u64).or_default() += 1;
            }
        }
    }
    let mut counts: Vec<usize> = per_sample.values().copied().collect();
    counts.sort();
    h.others = counts.get(counts.len() / 2).copied();
    if !h.you_found && h.with_faces.is_some_and(|w| w > 0.5) && !segments.is_empty() {
        h.problems.push("your face wasn't told apart (camera off, or too little of your speech on camera)".into());
    }
    if h.unknown.is_some_and(|u| u > 0.25) {
        h.problems.push(format!("{:.0}% of face time couldn't be told apart, so it's left out of the cues", h.unknown.unwrap_or(0.0) * 100.0));
    }
    let others = video::others(&tracks, &speech, &p);
    for answer in temperature::conversation(segments).into_iter().filter(|t| t.kind == Kind::Answer) {
        h.answers += 1;
        if let Some(c) = video::cues(&faces, &others, answer.start, answer.end, &p) {
            h.answers_with_cues += 1;
            h.nods += c.nods;
        }
    }
    Ok(h)
}

/// Where the dense faces of `dir` would go (for tests and the CLI's messages).
pub fn dense_path(dir: &str) -> PathBuf {
    Path::new(dir).join(DENSE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{INTERVIEWER, YOU};
    use crate::video::Face;

    fn face(x: f64, y: f64, mouth: f64) -> Face {
        Face { x, y, w: 0.15, h: 0.25, yaw: Some(0.0), pitch: Some(-5.0), mouth: Some(mouth) }
    }

    fn talking(t: f64) -> f64 {
        if (t * 10.0).round() as i64 % 2 == 0 { 0.02 } else { 0.25 }
    }

    /// A 60 s interview at 10 frames a second: you answer 0–30 s while they nod at 5, 12 and 20 s
    /// and look down for the first 6 s; they ask 30–60 s.
    fn interview(dir: &Path) {
        let segments = vec![Segment::new(0.0, 30.0, "my answer", YOU), Segment::new(30.0, 60.0, "their question", INTERVIEWER)];
        std::fs::write(dir.join("transcript.json"), serde_json::to_string(&segments).unwrap()).unwrap();
        let samples: Vec<Sample> = (0..600)
            .map(|i| {
                let t = i as f64 / 10.0;
                let (mine, theirs) = if t < 30.0 { (talking(t), 0.03) } else { (0.03, talking(t)) };
                let nod = [5.0, 12.0, 20.0].iter().any(|n| (t - n).abs() < 0.15);
                let pitch = if t < 6.0 { -35.0 } else { -5.0 };
                Sample {
                    t,
                    // A 7% dip of their head: over the shipped 6% threshold, under a stricter 8%.
                    faces: vec![face(0.1, 0.2, mine), Face { pitch: Some(pitch), ..face(0.6, if nod { 0.2175 } else { 0.2 }, theirs) }],
                }
            })
            .collect();
        let dense = Faces { version: 1, fps: 10.0, samples, complete: true, skipped: 0 };
        std::fs::write(dir.join(DENSE_FILE), serde_json::to_string(&dense).unwrap()).unwrap();
    }

    fn clip(id: &str, dir: &Path, start: f64, end: f64, origin: &str, labels: Value) -> Value {
        json!({"id": id, "set": "s001", "variant": id, "origin": origin, "session_dir": dir, "start": start, "end": end,
               "face_height": 0.25, "layout": "gallery", "labels": labels, "labelled_at": "2026-10-05T00:00:00+00:00"})
    }

    /// A correction that says they nodded, where the review had measured none: "then wrong", and
    /// each arm's agreement now.
    #[test]
    fn corrections_show_where_the_review_was_wrong() {
        let tmp = tempfile::tempdir().unwrap();
        interview(tmp.path());
        let mut fix = clip("s001-a0.0-fix", tmp.path(), 0.0, 30.0, "correction", json!({"nodded": true, "on_camera": "1"}));
        fix["measured"] = json!({"on_camera": 1.0, "nods": 0, "looking_away": null, "seen_s": 30.0});
        let path = tmp.path().join("corrections.jsonl");
        std::fs::write(&path, fix.to_string()).unwrap();
        let corrections = load_clips(&path).unwrap();
        let set = video_set();
        let arms = vec![eval::Arm::parse("video-v1.1").unwrap()];
        let settings = crate::config::Settings::load().unwrap();
        let out = tmp.path().join("out");
        let plan = eval::Plan { set: &set, items: &corrections, arms: &arms, runs: 1, concurrency: 1, out_dir: &out };
        let results = eval::run(&plan, &settings, &|_, _, _| {}).unwrap();
        let md = corrections_markdown(&results, &corrections, &arms, &set);
        assert!(md.contains("1 answers corrected"), "{md}");
        assert!(md.contains("| nodded | 1 | 1 | 100% |"), "the review said no nod; v1.1 now finds them: {md}");
        assert!(md.contains("| on_camera | 1 | 0 | 100% |"), "{md}");
        assert!(corrections_markdown(&results, &[], &arms, &set).contains("None yet"));
    }

    #[test]
    fn resampling_follows_the_reviews_grid_from_each_starting_point() {
        let dense = Faces {
            version: 1,
            fps: 10.0,
            samples: (0..30).map(|i| Sample { t: i as f64 / 10.0, faces: vec![] }).collect(),
            complete: true,
            skipped: 0,
        };
        let ts = |r: usize| resample(&dense, 6.0, offset(r)).samples.iter().map(|s| (s.t * 10.0).round() as i64).collect::<Vec<_>>();
        assert_eq!(ts(0)[..6], [0, 2, 4, 5, 7, 9], "the same frames the review takes at 6 a second");
        assert_ne!(ts(0), ts(1), "a shifted grid takes different frames");
        assert_eq!(offset(3), offset(0));
        // A gap (frames that stood still) is crossed without drifting off the grid.
        let gappy = Faces { samples: vec![Sample { t: 0.0, faces: vec![] }, Sample { t: 5.05, faces: vec![] }, Sample { t: 5.1, faces: vec![] }], ..dense };
        let r = resample(&gappy, 6.0, 0.0);
        assert_eq!(r.samples.len(), 2, "5.05 is taken, and the next grid point (5.1667) skips 5.1");
    }

    #[test]
    fn labels_load_as_levels_and_unlabelled_clips_are_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("clips.jsonl");
        let mut lines = vec![];
        for (i, count) in NOD_LEVELS.iter().enumerate() {
            lines.push(clip(&format!("c{i}"), tmp.path(), 0.0, 20.0, "designed", json!({"nod_count": count})).to_string());
        }
        let mut unlabelled = clip("u", tmp.path(), 0.0, 20.0, "designed", json!({}));
        unlabelled["labelled_at"] = Value::Null;
        lines.push(unlabelled.to_string());
        std::fs::write(&path, lines.join("\n")).unwrap();
        let items = load_clips(&path).unwrap();
        assert_eq!(items.iter().map(|i| i.expected("nod_count").unwrap()).collect::<Vec<_>>(), ["1", "2", "3", "4"]);
        // Every value the labelling page accepts maps to a level.
        let accepted = crate::label::CHOICES.iter().find(|(id, _)| *id == "nod_count").unwrap().1;
        assert_eq!(accepted, NOD_LEVELS);
    }

    #[test]
    fn the_scorer_reads_a_clip_like_the_review_and_abstains_without_video() {
        let tmp = tempfile::tempdir().unwrap();
        interview(tmp.path());
        let input = ClipInput { session_dir: tmp.path().display().to_string(), start: 0.0, end: 30.0 };
        let set = video_set();
        let scorer = VideoScorer { params: video::V1_1 };
        for run in 0..RUNS {
            let a = scorer.assess(&input, &set, run).unwrap();
            assert_eq!(a.scorer, "local/video-v1.1");
            assert_eq!(a.verdicts["nodded"].pick, "yes", "run {run}");
            assert_eq!(a.verdicts["nod_count"].pick, "3", "3 nods is level 3 (3–5), run {run}");
            assert_eq!(a.verdicts["on_camera"].pick, "1", "you aren't counted, run {run}");
            assert_eq!(a.verdicts["looked_away"].pick, "sometimes", "6 s of 30 is 20%, run {run}");
            assert!(!a.verdicts.contains_key("smiled"), "v1.1 doesn't measure smiles");
        }
        let empty = ClipInput { start: 100.0, end: 120.0, ..input };
        assert!(scorer.assess(&empty, &set, 0).unwrap().verdicts.is_empty(), "no video there: it abstains");
    }

    /// The whole flow on a small set: scoring through `eval::run`, the rule's decisions (too few
    /// examples to decide), the report's sections, the sweep and the you-marks.
    #[test]
    fn a_labelled_set_gets_the_whole_report() {
        let tmp = tempfile::tempdir().unwrap();
        interview(tmp.path());
        let path = tmp.path().join("clips.jsonl");
        let lines = [
            clip("d1", tmp.path(), 0.0, 20.0, "designed", json!({"nodded": true, "nod_count": "1-2", "on_camera": "1", "looked_away": "sometimes"})),
            clip("r1", tmp.path(), 0.0, 30.0, "realistic", json!({"nodded": true, "nod_count": "3-5", "on_camera": "1", "looked_away": "sometimes"})),
            clip("r2", tmp.path(), 30.0, 50.0, "realistic", json!({"nodded": false, "nod_count": "none", "on_camera": "1"})),
        ];
        std::fs::write(&path, lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let items = load_clips(&path).unwrap();
        let set = video_set();
        let arms = vec![eval::Arm::parse("video-v1.1").unwrap(), eval::Arm::parse("video-v1").unwrap()];
        let out = tmp.path().join("out");
        let settings = crate::config::Settings::load().unwrap();
        let plan = eval::Plan { set: &set, items: &items, arms: &arms, runs: RUNS, concurrency: 2, out_dir: &out };
        let results = eval::run(&plan, &settings, &|_, _, _| {}).unwrap();
        assert_eq!(results.len(), 3 * 2 * RUNS);
        assert!(results.iter().all(|r| r.result.is_ok()), "{:?}", results.iter().find(|r| r.result.is_err()));

        let decided = decisions(&results, &items, &arms, &set);
        let nodded = decided.iter().find(|d| d.check == "nodded" && d.arm == "video-v1.1").unwrap();
        assert!(nodded.undecided && nodded.reasons.iter().any(|r| r.contains("too few realistic examples")), "{nodded:?}");
        let smiled = decided.iter().find(|d| d.check == "smiled" && d.arm == "video-v1.1").unwrap();
        assert!(smiled.reasons.iter().any(|r| r.contains("not scored")), "{smiled:?}");
        let gate = suggested_gate(&decided, "video-v1.1");
        assert_eq!(gate, "pub const GATE: Gate = Gate { passed: &[], failed: &[], hidden_below: &[] };", "nothing is decided yet");

        let md = markdown(&results, &items, &arms, &set, &decided);
        for section in ["## Coverage", "## The rule, check by check", "## By face size (realistic clips)", "## By layout", "## Labels"] {
            assert!(md.contains(section), "{section}");
        }
        assert!(md.contains("| smiled | video-v1.1 | 0 | 0 | 0 |"), "nobody labelled smiles, nobody answered");

        let rows = sweep(&items, &video::V1_1, &set).unwrap();
        assert_eq!(rows[0].change, "as is");
        let strict = rows.iter().find(|r| r.change == "nod_drop 0.08").unwrap();
        assert_eq!(rows[0].scores["nodded"], Some(1.0), "the designed clip's nods are found");
        assert_eq!(strict.scores["nodded"], Some(0.0), "a stricter threshold misses them: the sweep shows what tuning costs");
        assert!(sweep_markdown(&rows, &set, &video::V1_1).contains("designed clips only"));

        let marks = vec![
            json!({"set": "s001", "session_dir": tmp.path(), "at": 10.0, "x": 0.17, "y": 0.3, "labels": {"you_visible": true}}),
        ];
        let rows = you_rows(&marks, &video::V1_1).unwrap();
        assert_eq!(rows[0].yours.as_deref(), Some("you"));
        assert_eq!((rows[0].others, rows[0].others_as_you), (1, 0));
        let md = you_markdown(&[("video-v1.1".into(), rows)]);
        assert!(md.contains("| video-v1.1 | 1 | 100% | 0 (0%) | 0 (0%) | 0 (0%) | 0 |"), "{md}");
    }
}
