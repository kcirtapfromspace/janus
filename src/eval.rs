//! The Jev vs Claude comparison (`ic eval scorers`): every arm scores the same labelled answers
//! (tests/fixtures/answers.jsonl) several times, with choice options rotated each run. Raw results
//! are cached, so re-running or re-analysing costs nothing. The summary applies the decision rule
//! written down before the first run (docs/eval/scorer-decision.md).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::llm::{self, Effort};
use crate::proxy::LlmEndpoint;
use crate::scoring::{AnswerInput, Assessment, Check, CheckKind, ClaudeScorer, JevScorer, Scorer, builtin_checks};

// --- the labelled set ----------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub id: String,
    pub set: String,
    pub variant: String,
    pub origin: String,
    pub question: String,
    pub answer: String,
    pub labels: BTreeMap<String, Value>,
}

impl Item {
    /// The expected pick for a check, in the scorer's terms ("yes"/"no", an option id, a level).
    /// None when the item isn't labelled for it (e.g. STAR on an answer that isn't a story).
    pub fn expected(&self, check: &str) -> Option<String> {
        match self.labels.get(check)? {
            Value::Bool(b) => Some(if *b { "yes" } else { "no" }.into()),
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
}

pub fn load_items(path: &Path) -> Result<Vec<Item>> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("bad line in {}: {l}", path.display())))
        .collect()
}

// --- arms ----------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ArmKind {
    Jev { model: String },
    Claude { model: String, effort: Effort },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Arm {
    /// Short name used in tables: jev, haiku, sonnet, opus.
    pub name: String,
    pub kind: ArmKind,
}

impl Arm {
    pub fn parse(name: &str) -> Result<Arm, String> {
        let kind = match name {
            "jev" => ArmKind::Jev { model: crate::llm::jev::DEFAULT_MODEL.into() },
            "haiku" => ArmKind::Claude { model: "claude-haiku-4-5-20251001".into(), effort: Effort::Low },
            "sonnet" => ArmKind::Claude { model: "claude-sonnet-5-5".into(), effort: Effort::Low },
            "opus" => ArmKind::Claude { model: "claude-opus-5-5".into(), effort: Effort::Low },
            other => return Err(format!("unknown arm {other:?} (expected jev, haiku, sonnet, opus)")),
        };
        Ok(Arm { name: name.into(), kind })
    }

    /// Cheapest and fastest first: the decision rule prefers earlier arms when several qualify.
    pub fn cost_rank(&self) -> usize {
        ["jev", "haiku", "sonnet", "opus"].iter().position(|n| *n == self.name).unwrap_or(9)
    }

    fn model(&self) -> &str {
        match &self.kind {
            ArmKind::Jev { model } | ArmKind::Claude { model, .. } => model,
        }
    }
}

/// Run one arm on one item, building its client in the calling thread.
fn assess(arm: &Arm, endpoint: &LlmEndpoint, input: &AnswerInput, checks: &[Check], rotation: usize) -> Result<Assessment> {
    match &arm.kind {
        ArmKind::Jev { model } => {
            let client = crate::llm::jev::Client::new(endpoint.clone());
            JevScorer { client: &client, model: model.clone() }.assess(input, checks, rotation)
        }
        ArmKind::Claude { model, effort } => {
            let model_ref = format!("anthropic/{model}").parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let client = llm::client(&model_ref, endpoint.clone());
            ClaudeScorer { llm: client.as_ref(), model: model.clone(), effort: *effort }.assess(input, checks, rotation)
        }
    }
}

// --- the run -------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawResult {
    pub key: String,
    pub arm: String,
    pub item: String,
    pub run: usize,
    pub result: Result<Assessment, String>,
}

/// Identifies a result: the arm and model, the item's exact text, the run, and the checks' wording.
fn cache_key(arm: &Arm, item: &Item, run: usize, checks: &[Check]) -> String {
    let mut h = Sha256::new();
    h.update(item.question.as_bytes());
    h.update(item.answer.as_bytes());
    for c in checks {
        h.update(format!("{c:?}").as_bytes());
    }
    let digest: String = h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{}|{}|{}|run{run}|{digest}", arm.name, arm.model(), item.id)
}

pub struct Plan<'a> {
    pub items: &'a [Item],
    pub arms: &'a [Arm],
    pub runs: usize,
    pub concurrency: usize,
    pub out_dir: &'a Path,
}

/// Score everything not already cached in `out_dir/raw.jsonl`; returns every result (cached + new).
pub fn run(plan: &Plan, endpoint: &LlmEndpoint, on_result: &(dyn Fn(&RawResult, usize, usize) + Sync)) -> Result<Vec<RawResult>> {
    std::fs::create_dir_all(plan.out_dir)?;
    let raw_path = plan.out_dir.join("raw.jsonl");
    let checks = builtin_checks();
    let mut cached: HashMap<String, RawResult> = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&raw_path) {
        for line in text.lines() {
            if let Ok(r) = serde_json::from_str::<RawResult>(line)
                && r.result.is_ok()
            {
                cached.insert(r.key.clone(), r);
            }
        }
    }
    let mut todo = VecDeque::new();
    let mut results = vec![];
    for arm in plan.arms {
        for item in plan.items {
            for run in 0..plan.runs {
                let key = cache_key(arm, item, run, &checks);
                match cached.remove(&key) {
                    Some(r) => results.push(r),
                    None => todo.push_back((arm.clone(), item.clone(), run, key)),
                }
            }
        }
    }
    let total = todo.len();
    let queue = Mutex::new(todo);
    let out = Mutex::new((std::fs::OpenOptions::new().create(true).append(true).open(&raw_path)?, results, 0usize));
    std::thread::scope(|scope| {
        for _ in 0..plan.concurrency.max(1) {
            scope.spawn(|| loop {
                let Some((arm, item, run, key)) = queue.lock().unwrap().pop_front() else { return };
                let input = AnswerInput { question: item.question.clone(), answer: item.answer.clone() };
                let result = assess(&arm, endpoint, &input, &checks, run).map_err(|e| format!("{e:#}"));
                let raw = RawResult { key, arm: arm.name.clone(), item: item.id.clone(), run, result };
                let mut guard = out.lock().unwrap();
                let (file, results, done) = &mut *guard;
                let _ = writeln!(file, "{}", serde_json::to_string(&raw).unwrap_or_default());
                *done += 1;
                on_result(&raw, *done, total);
                results.push(raw);
            });
        }
    });
    Ok(out.into_inner().unwrap().1)
}

// --- metrics (pure) ------------------------------------------------------------------------------

/// One scored prediction for one check.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub item: String,
    pub run: usize,
    pub expected: String,
    pub pick: String,
    /// The scorer's probability for its pick, when it gives one (for calibration).
    pub p_pick: Option<f64>,
    /// P(yes) for yes/no checks.
    pub p_yes: Option<f64>,
}

pub fn balanced_accuracy(preds: &[Prediction]) -> Option<f64> {
    let classes: BTreeSet<&str> = preds.iter().map(|p| p.expected.as_str()).collect();
    let recalls: Vec<f64> = classes
        .iter()
        .map(|c| {
            let of_class: Vec<_> = preds.iter().filter(|p| p.expected == *c).collect();
            of_class.iter().filter(|p| p.pick == p.expected).count() as f64 / of_class.len() as f64
        })
        .collect();
    (!recalls.is_empty()).then(|| recalls.iter().sum::<f64>() / recalls.len() as f64)
}

pub fn accuracy(preds: &[Prediction]) -> Option<f64> {
    (!preds.is_empty()).then(|| preds.iter().filter(|p| p.pick == p.expected).count() as f64 / preds.len() as f64)
}

/// Macro-averaged F1 over the classes that appear in the labels.
pub fn macro_f1(preds: &[Prediction]) -> Option<f64> {
    let classes: BTreeSet<&str> = preds.iter().map(|p| p.expected.as_str()).collect();
    let f1s: Vec<f64> = classes
        .iter()
        .map(|c| {
            let tp = preds.iter().filter(|p| p.pick == *c && p.expected == *c).count() as f64;
            let fp = preds.iter().filter(|p| p.pick == *c && p.expected != *c).count() as f64;
            let fn_ = preds.iter().filter(|p| p.pick != *c && p.expected == *c).count() as f64;
            if tp == 0.0 { 0.0 } else { 2.0 * tp / (2.0 * tp + fp + fn_) }
        })
        .collect();
    (!f1s.is_empty()).then(|| f1s.iter().sum::<f64>() / f1s.len() as f64)
}

fn level(s: &str) -> Option<f64> {
    s.parse().ok()
}

pub fn within_one(preds: &[Prediction]) -> Option<f64> {
    let pairs: Vec<(f64, f64)> = preds.iter().filter_map(|p| Some((level(&p.pick)?, level(&p.expected)?))).collect();
    (!pairs.is_empty()).then(|| pairs.iter().filter(|(a, b)| (a - b).abs() <= 1.0).count() as f64 / pairs.len() as f64)
}

pub fn mean_abs_error(preds: &[Prediction]) -> Option<f64> {
    let pairs: Vec<(f64, f64)> = preds.iter().filter_map(|p| Some((level(&p.pick)?, level(&p.expected)?))).collect();
    (!pairs.is_empty()).then(|| pairs.iter().map(|(a, b)| (a - b).abs()).sum::<f64>() / pairs.len() as f64)
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut ranks = vec![0.0; values.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len() && values[order[j + 1]] == values[order[i]] {
            j += 1;
        }
        let rank = (i + j) as f64 / 2.0 + 1.0; // ties share their average rank
        for k in i..=j {
            ranks[order[k]] = rank;
        }
        i = j + 1;
    }
    ranks
}

/// Spearman's rank correlation between predicted and expected levels.
pub fn spearman(preds: &[Prediction]) -> Option<f64> {
    let pairs: Vec<(f64, f64)> = preds.iter().filter_map(|p| Some((level(&p.pick)?, level(&p.expected)?))).collect();
    if pairs.len() < 3 {
        return None;
    }
    let (a, b): (Vec<f64>, Vec<f64>) = pairs.into_iter().unzip();
    let (ra, rb) = (ranks(&a), ranks(&b));
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (ma, mb) = (mean(&ra), mean(&rb));
    let cov: f64 = ra.iter().zip(&rb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let var = |v: &[f64], m: f64| v.iter().map(|x| (x - m).powi(2)).sum::<f64>();
    let denom = (var(&ra, ma) * var(&rb, mb)).sqrt();
    (denom > 0.0).then(|| cov / denom)
}

/// Brier score of P(yes) against the yes/no label (lower is better).
pub fn brier(preds: &[Prediction]) -> Option<f64> {
    let scored: Vec<f64> = preds
        .iter()
        .filter_map(|p| Some((p.p_yes? - if p.expected == "yes" { 1.0 } else { 0.0 }).powi(2)))
        .collect();
    (!scored.is_empty()).then(|| scored.iter().sum::<f64>() / scored.len() as f64)
}

/// Expected calibration error over 10 bins of the pick's probability.
pub fn ece(preds: &[Prediction]) -> Option<f64> {
    let scored: Vec<(f64, bool)> = preds.iter().filter_map(|p| Some((p.p_pick?, p.pick == p.expected))).collect();
    if scored.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for bin in 0..10 {
        let (lo, hi) = (bin as f64 / 10.0, (bin + 1) as f64 / 10.0);
        let in_bin: Vec<_> = scored.iter().filter(|(p, _)| *p >= lo && (*p < hi || (bin == 9 && *p <= 1.0))).collect();
        if in_bin.is_empty() {
            continue;
        }
        let confidence = in_bin.iter().map(|(p, _)| p).sum::<f64>() / in_bin.len() as f64;
        let accuracy = in_bin.iter().filter(|(_, ok)| *ok).count() as f64 / in_bin.len() as f64;
        total += (confidence - accuracy).abs() * in_bin.len() as f64;
    }
    Some(total / scored.len() as f64)
}

/// Share of items whose pick was the same on every run.
pub fn stability(preds: &[Prediction]) -> Option<f64> {
    let mut by_item: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in preds {
        by_item.entry(&p.item).or_default().insert(&p.pick);
    }
    (!by_item.is_empty()).then(|| by_item.values().filter(|picks| picks.len() == 1).count() as f64 / by_item.len() as f64)
}

pub fn percentile(values: &[u64], q: f64) -> Option<u64> {
    let mut v = values.to_vec();
    v.sort_unstable();
    (!v.is_empty()).then(|| v[((v.len() - 1) as f64 * q).round() as usize])
}

// --- summary -------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CheckStats {
    pub n: usize,
    /// Balanced accuracy (yes/no, choice) or within-1 agreement (levels): the rule's main number.
    pub score: Option<f64>,
    pub accuracy: Option<f64>,
    pub macro_f1: Option<f64>,
    pub within_one: Option<f64>,
    pub spearman: Option<f64>,
    pub mae: Option<f64>,
    pub brier: Option<f64>,
    pub ece: Option<f64>,
    pub stability: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmStats {
    pub arm: String,
    pub model: String,
    pub calls: usize,
    pub errors: usize,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub input_tokens_per_answer: Option<f64>,
    pub checks: BTreeMap<String, CheckStats>,
}

pub fn predictions(results: &[RawResult], items: &[Item], arm: &str, check: &str) -> Vec<Prediction> {
    let by_id: HashMap<&str, &Item> = items.iter().map(|i| (i.id.as_str(), i)).collect();
    results
        .iter()
        .filter(|r| r.arm == arm)
        .filter_map(|r| {
            let item = by_id.get(r.item.as_str())?;
            let expected = item.expected(check)?;
            let v = r.result.as_ref().ok()?.verdicts.get(check)?;
            let p_pick = v.probabilities.get(&v.pick).copied();
            Some(Prediction { item: r.item.clone(), run: r.run, expected, pick: v.pick.clone(), p_pick,
                              p_yes: v.probabilities.get("yes").copied() })
        })
        .collect()
}

pub fn summarize(results: &[RawResult], items: &[Item], arms: &[Arm]) -> Vec<ArmStats> {
    let checks = builtin_checks();
    arms.iter()
        .map(|arm| {
            let mine: Vec<_> = results.iter().filter(|r| r.arm == arm.name).collect();
            let ok: Vec<&Assessment> = mine.iter().filter_map(|r| r.result.as_ref().ok()).collect();
            let latencies: Vec<u64> = ok.iter().map(|a| a.latency_ms).collect();
            let tokens: Vec<u64> = ok.iter().map(|a| a.input_tokens).filter(|t| *t > 0).collect();
            let model = ok.first().map(|a| a.scorer.clone()).unwrap_or_else(|| arm.model().to_string());
            let mut stats = BTreeMap::new();
            for check in &checks {
                let preds = predictions(results, items, &arm.name, check.id);
                let is_level = matches!(check.kind, CheckKind::Level { .. });
                let s = CheckStats {
                    n: preds.len(),
                    score: if is_level { within_one(&preds) } else { balanced_accuracy(&preds) },
                    accuracy: accuracy(&preds),
                    macro_f1: if is_level { None } else { macro_f1(&preds) },
                    within_one: if is_level { within_one(&preds) } else { None },
                    spearman: if is_level { spearman(&preds) } else { None },
                    mae: if is_level { mean_abs_error(&preds) } else { None },
                    brier: brier(&preds),
                    ece: ece(&preds),
                    stability: stability(&preds),
                };
                stats.insert(check.id.to_string(), s);
            }
            ArmStats {
                arm: arm.name.clone(),
                model,
                calls: mine.len(),
                errors: mine.len() - ok.len(),
                p50_ms: percentile(&latencies, 0.5),
                p95_ms: percentile(&latencies, 0.95),
                input_tokens_per_answer: (!tokens.is_empty()).then(|| tokens.iter().sum::<u64>() as f64 / tokens.len() as f64),
                checks: stats,
            }
        })
        .collect()
}

/// The decision rule from docs/eval/scorer-decision.md, per check: the cheapest qualifying arm.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub check: String,
    pub winner: Option<String>,
    /// When nothing qualifies: the most accurate arm, used but not fast enough for drills.
    pub fallback: Option<String>,
    pub qualified: Vec<String>,
    pub reasons: BTreeMap<String, Vec<String>>,
}

pub const MIN_SCORE: f64 = 0.85;
pub const MIN_WITHIN_ONE: f64 = 0.90;
pub const MAX_BEHIND_BEST: f64 = 0.05;
pub const MIN_STABILITY: f64 = 0.95;
pub const MAX_P95_MS: u64 = 1_000;
pub const MAX_ECE: f64 = 0.10;

pub fn decide(stats: &[ArmStats], arms: &[Arm]) -> Vec<Decision> {
    let rank = |name: &str| arms.iter().find(|a| a.name == name).map(Arm::cost_rank).unwrap_or(9);
    builtin_checks()
        .iter()
        .map(|check| {
            let is_level = matches!(check.kind, CheckKind::Level { .. });
            let best = stats.iter().filter_map(|s| s.checks.get(check.id)?.score).fold(0.0, f64::max);
            let mut reasons = BTreeMap::new();
            let mut qualified = vec![];
            for s in stats {
                let Some(c) = s.checks.get(check.id) else { continue };
                let mut why = vec![];
                let score = c.score.unwrap_or(0.0);
                let floor = if is_level { MIN_WITHIN_ONE } else { MIN_SCORE };
                if score < floor {
                    why.push(format!("{} {:.2} < {floor}", if is_level { "within-1" } else { "balanced accuracy" }, score));
                }
                if !is_level && best - score > MAX_BEHIND_BEST {
                    why.push(format!("{:.2} behind the best arm", best - score));
                }
                if c.stability.unwrap_or(0.0) < MIN_STABILITY {
                    why.push(format!("stability {:.2} < {MIN_STABILITY}", c.stability.unwrap_or(0.0)));
                }
                if s.p95_ms.unwrap_or(u64::MAX) > MAX_P95_MS {
                    why.push(format!("p95 {} ms > {MAX_P95_MS} ms", s.p95_ms.unwrap_or(0)));
                }
                if s.arm == "jev" && c.ece.is_some_and(|e| e > MAX_ECE) {
                    why.push(format!("ECE {:.2} > {MAX_ECE}", c.ece.unwrap_or(0.0)));
                }
                if s.errors > 0 {
                    why.push(format!("{} failed calls", s.errors));
                }
                if why.is_empty() {
                    qualified.push(s.arm.clone());
                }
                reasons.insert(s.arm.clone(), why);
            }
            qualified.sort_by_key(|a| rank(a));
            let fallback = if qualified.is_empty() {
                stats
                    .iter()
                    .filter_map(|s| Some((s.arm.clone(), s.checks.get(check.id)?.score?)))
                    .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| rank(&b.0).cmp(&rank(&a.0))))
                    .map(|(arm, _)| arm)
            } else {
                None
            };
            Decision { check: check.id.to_string(), winner: qualified.first().cloned(), fallback, qualified, reasons }
        })
        .collect()
}

pub const CASCADE_MAX_ESCALATION: f64 = 0.25;
pub const CASCADE_MAX_BEHIND_BEST: f64 = 0.02;

/// The lowest-escalation threshold whose accuracy is within 0.02 of `best` while escalating at most
/// 25% of answers (the rule's cascade condition), if any.
pub fn cascade_choice(rows: &[(f64, f64, f64)], best: f64) -> Option<(f64, f64, f64)> {
    rows.iter()
        .filter(|(_, acc, esc)| best - acc <= CASCADE_MAX_BEHIND_BEST && *esc <= CASCADE_MAX_ESCALATION)
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .copied()
}

/// "Use Jev when it's confident, otherwise ask `fallback`": accuracy and escalation rate per threshold,
/// over the yes/no and choice checks.
pub fn cascade(results: &[RawResult], items: &[Item], fallback: &str) -> Vec<(f64, f64, f64)> {
    let checks: Vec<&'static str> = builtin_checks()
        .iter()
        .filter(|c| !matches!(c.kind, CheckKind::Level { .. }))
        .map(|c| c.id)
        .collect();
    let mut rows = vec![];
    for t in [0.5, 0.6, 0.7, 0.8, 0.9, 0.95] {
        let (mut right, mut total, mut escalated) = (0usize, 0usize, 0usize);
        for check in &checks {
            let jev = predictions(results, items, "jev", check);
            let other: HashMap<(String, usize), Prediction> =
                predictions(results, items, fallback, check).into_iter().map(|p| ((p.item.clone(), p.run), p)).collect();
            for p in jev {
                let confident = p.p_pick.unwrap_or(0.0) >= t;
                let used = if confident { Some(&p) } else { other.get(&(p.item.clone(), p.run)) };
                let Some(used) = used else { continue };
                total += 1;
                escalated += usize::from(!confident);
                right += usize::from(used.pick == used.expected);
            }
        }
        if total > 0 {
            rows.push((t, right as f64 / total as f64, escalated as f64 / total as f64));
        }
    }
    rows
}

fn pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| "—".into())
}

fn num(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".into())
}

pub fn markdown(stats: &[ArmStats], decisions: &[Decision], cascade_rows: &[(f64, f64, f64)], fallback: &str, items: usize,
                runs: usize) -> String {
    let mut md = format!("# Jev vs Claude: answer-check comparison\n\n{items} labelled answers × {runs} runs per arm, choice \
                          options rotated each run. Rule: docs/eval/scorer-decision.md.\n\n## Speed and cost\n\n\
                          | Arm | Model | Calls | Failed | p50 | p95 | Input tokens / answer |\n|---|---|---|---|---|---|---|\n");
    for s in stats {
        md += &format!("| {} | {} | {} | {} | {} ms | {} ms | {} |\n", s.arm, s.model, s.calls, s.errors,
                       s.p50_ms.unwrap_or(0), s.p95_ms.unwrap_or(0),
                       s.input_tokens_per_answer.map(|t| format!("{t:.0}")).unwrap_or_else(|| "—".into()));
    }
    md += "\n## Accuracy by check\n\nYes/no and choice: balanced accuracy (macro-F1). Levels (1–5): within-1 agreement \
           (exact, Spearman). Stability: same pick on every run.\n\n| Check | Arm | n | Score | Detail | Stability | ECE |\n\
           |---|---|---|---|---|---|---|\n";
    for check in builtin_checks() {
        for s in stats {
            let Some(c) = s.checks.get(check.id) else { continue };
            let detail = match check.kind {
                CheckKind::Level { .. } => format!("exact {}, ρ {}", pct(c.accuracy), num(c.spearman)),
                _ => format!("F1 {}", num(c.macro_f1)),
            };
            md += &format!("| {} | {} | {} | {} | {} | {} | {} |\n", check.id, s.arm, c.n, pct(c.score), detail,
                           pct(c.stability), num(c.ece));
        }
    }
    md += "\n## Decision\n\n| Check | Winner | Qualified | Why others didn't |\n|---|---|---|---|\n";
    for d in decisions {
        let why: Vec<String> = d.reasons.iter().filter(|(_, r)| !r.is_empty()).map(|(a, r)| format!("{a}: {}", r.join("; "))).collect();
        let winner = match (&d.winner, &d.fallback) {
            (Some(w), _) => w.clone(),
            (None, Some(f)) => format!("none (most accurate: {f})"),
            (None, None) => "none".into(),
        };
        md += &format!("| {} | {} | {} | {} |\n", d.check, winner,
                       if d.qualified.is_empty() { "—".to_string() } else { d.qualified.join(", ") }, why.join("<br>"));
    }
    if !cascade_rows.is_empty() {
        md += &format!("\n## Cascade: Jev when confident, otherwise {fallback}\n\n| Threshold | Accuracy | Escalated |\n|---|---|---|\n");
        for (t, acc, esc) in cascade_rows {
            md += &format!("| {t:.2} | {} | {} |\n", pct(Some(*acc)), pct(Some(*esc)));
        }
        // The best single arm's plain accuracy over the same yes/no and choice checks.
        let best = stats
            .iter()
            .map(|s| {
                let accs: Vec<f64> = builtin_checks()
                    .iter()
                    .filter(|c| !matches!(c.kind, CheckKind::Level { .. }))
                    .filter_map(|c| s.checks.get(c.id)?.accuracy)
                    .collect();
                accs.iter().sum::<f64>() / accs.len().max(1) as f64
            })
            .fold(0.0, f64::max);
        md += &match cascade_choice(cascade_rows, best) {
            Some((t, acc, esc)) => format!("\nThe cascade qualifies at threshold {t:.2}: {} accurate (best single arm {}), \
                                            escalating {} of answers.\n", pct(Some(acc)), pct(Some(best)), pct(Some(esc))),
            None => format!("\nNo threshold qualifies: none is within 2 points of the best single arm ({}) while escalating \
                             25% or less.\n", pct(Some(best))),
        };
    }
    md
}

/// Every output of a comparison, written next to the raw results.
pub fn write_summary(out_dir: &Path, stats: &[ArmStats], decisions: &[Decision], markdown: &str) -> Result<PathBuf> {
    std::fs::write(out_dir.join("summary.json"), serde_json::to_string_pretty(&json!({"arms": stats, "decisions": decisions}))?)?;
    let path = out_dir.join("summary.md");
    std::fs::write(&path, markdown)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(item: &str, run: usize, expected: &str, pick: &str, p_pick: Option<f64>) -> Prediction {
        Prediction { item: item.into(), run, expected: expected.into(), pick: pick.into(), p_pick,
                     p_yes: p_pick.map(|x| if pick == "yes" { x } else { 1.0 - x }) }
    }

    #[test]
    fn balanced_accuracy_weights_each_class_equally() {
        // 9 yes all right, 1 no wrong: plain accuracy 90%, balanced 50%.
        let mut preds: Vec<_> = (0..9).map(|i| p(&format!("y{i}"), 0, "yes", "yes", None)).collect();
        preds.push(p("n0", 0, "no", "yes", None));
        assert_eq!(accuracy(&preds), Some(0.9));
        assert_eq!(balanced_accuracy(&preds), Some(0.5));
    }

    #[test]
    fn macro_f1_and_levels() {
        let preds = [p("a", 0, "i", "i", None), p("b", 0, "we", "i", None), p("c", 0, "we", "we", None)];
        // i: tp1 fp1 fn0 → 2/3; we: tp1 fp0 fn1 → 2/3.
        assert!((macro_f1(&preds).unwrap() - 2.0 / 3.0).abs() < 1e-9);
        let levels = [p("a", 0, "5", "4", None), p("b", 0, "3", "1", None), p("c", 0, "2", "2", None), p("d", 0, "4", "5", None)];
        assert_eq!(within_one(&levels), Some(0.75));
        assert_eq!(mean_abs_error(&levels), Some(1.0));
        assert!((spearman(&levels).unwrap() - 0.6).abs() < 1e-9, "ranks differ by 1 on every pair: 1 - 6·4/(4·15)");
    }

    #[test]
    fn spearman_handles_ties_and_perfect_order() {
        let same = [p("a", 0, "1", "1", None), p("b", 0, "3", "3", None), p("c", 0, "5", "5", None), p("d", 0, "5", "5", None)];
        assert!((spearman(&same).unwrap() - 1.0).abs() < 1e-9);
        let reversed = [p("a", 0, "1", "5", None), p("b", 0, "3", "3", None), p("c", 0, "5", "1", None)];
        assert!((spearman(&reversed).unwrap() + 1.0).abs() < 1e-9);
    }

    #[test]
    fn calibration_and_brier() {
        // Confident and right: well calibrated.
        let good = [p("a", 0, "yes", "yes", Some(0.95)), p("b", 0, "no", "no", Some(0.95))];
        assert!(ece(&good).unwrap() < 0.06);
        assert!(brier(&good).unwrap() < 0.01);
        // Confident and wrong: badly calibrated.
        let bad = [p("a", 0, "yes", "no", Some(0.95)), p("b", 0, "no", "yes", Some(0.95))];
        assert!(ece(&bad).unwrap() > 0.9);
    }

    #[test]
    fn stability_counts_items_with_one_pick_across_runs() {
        let preds = [p("a", 0, "i", "i", None), p("a", 1, "i", "i", None), p("b", 0, "i", "i", None), p("b", 1, "i", "we", None)];
        assert_eq!(stability(&preds), Some(0.5));
        assert_eq!(percentile(&[100, 200, 300, 400, 1000], 0.95), Some(1000));
        assert_eq!(percentile(&[100, 200, 300], 0.5), Some(200));
    }

    fn stats(arm: &str, score: f64, stability: f64, p95: u64, ece: Option<f64>) -> ArmStats {
        let checks = builtin_checks()
            .iter()
            .map(|c| (c.id.to_string(), CheckStats { n: 70, score: Some(score), accuracy: None, macro_f1: None, within_one: None,
                                                      spearman: None, mae: None, brier: None, ece, stability: Some(stability) }))
            .collect();
        ArmStats { arm: arm.into(), model: arm.into(), calls: 210, errors: 0, p50_ms: Some(p95 / 2), p95_ms: Some(p95),
                   input_tokens_per_answer: None, checks }
    }

    #[test]
    fn the_rule_picks_the_cheapest_arm_that_qualifies() {
        let arms: Vec<Arm> = ["jev", "haiku", "opus"].iter().map(|a| Arm::parse(a).unwrap()).collect();
        // Jev is accurate and fast; Haiku is as good but slower than the 1 s budget; Opus is best but slow.
        let s = [stats("jev", 0.93, 0.97, 400, Some(0.05)), stats("haiku", 0.93, 0.99, 1800, None), stats("opus", 0.96, 1.0, 6000, None)];
        let d = decide(&s, &arms);
        assert!(d.iter().all(|d| d.winner.as_deref() == Some("jev")), "{d:#?}");
        // Jev badly calibrated: nobody qualifies (the others are too slow for drills).
        let s = [stats("jev", 0.93, 0.97, 400, Some(0.3)), stats("haiku", 0.93, 0.99, 1800, None)];
        let d = decide(&s, &arms);
        assert!(d.iter().all(|d| d.winner.is_none()));
        assert!(d[0].reasons["jev"][0].contains("ECE"));
        // Jev too far behind the best arm on a yes/no check.
        let s = [stats("jev", 0.86, 0.97, 400, Some(0.05)), stats("haiku", 0.95, 0.99, 900, None)];
        assert_eq!(decide(&s, &arms)[0].winner.as_deref(), Some("haiku"));
    }

    #[test]
    fn a_cascade_qualifies_only_close_to_the_best_and_with_few_escalations() {
        let rows = [(0.5, 0.80, 0.05), (0.7, 0.90, 0.20), (0.9, 0.93, 0.40)];
        assert_eq!(cascade_choice(&rows, 0.91), Some((0.7, 0.90, 0.20)));
        assert_eq!(cascade_choice(&rows, 0.95), None, "the only close one escalates 40%");
    }

    #[test]
    fn without_a_qualifier_the_most_accurate_arm_is_the_fallback() {
        let arms: Vec<Arm> = ["jev", "opus"].iter().map(|a| Arm::parse(a).unwrap()).collect();
        let s = [stats("jev", 0.70, 0.97, 400, Some(0.05)), stats("opus", 0.96, 1.0, 6000, None)];
        let d = decide(&s, &arms);
        assert_eq!((d[0].winner.as_deref(), d[0].fallback.as_deref()), (None, Some("opus")));
    }

    #[test]
    fn the_labelled_set_loads_and_every_check_has_labels() {
        let items = load_items(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/answers.jsonl").as_path()).unwrap();
        assert_eq!(items.len(), 70);
        for check in builtin_checks() {
            let labelled = items.iter().filter(|i| i.expected(check.id).is_some()).count();
            assert!(labelled >= 60, "{} has {labelled} labels", check.id);
            for item in &items {
                if let (Some(expected), CheckKind::Choice { options }) = (item.expected(check.id), &check.kind) {
                    assert!(options.iter().any(|(id, _)| *id == expected), "{}: unknown label {expected}", item.id);
                }
            }
        }
    }
}
