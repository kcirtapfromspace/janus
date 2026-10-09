//! Your progress across interviews: every area the reviews measure, interview by interview, and
//! whether each is getting better or worse. It's rebuilt from the current reviews whenever it's
//! read, so it moves as soon as a new interview is reviewed. Practice, archived and deleted
//! interviews are left out, as they are from the notebook's other counts.
//!
//! **The rule:** an area needs `MIN_POINTS` reviewed interviews before it gets a direction. Then
//! your latest interviews (up to `RECENT`, and never more than half) are compared with the ones
//! before them. A change smaller than the area's `band` is steady; that keeps one noisy review from
//! reading as a trend.

use std::collections::BTreeSet;

use anyhow::Result;
use chrono::{DateTime, Duration, Local, TimeZone};
use serde::Serialize;

use crate::db::{Db, StoredAnalysis};
use crate::models::{Session, Verdict};
use crate::pipeline;
use crate::questions::key_words;
use crate::temperature::Kind;

/// Reviewed interviews an area needs before it shows a direction.
pub const MIN_POINTS: usize = 3;
/// At most this many of your latest interviews make up "lately".
pub const RECENT: usize = 3;
/// Talk measures need at least this many of your words to mean anything.
const MIN_WORDS: usize = 100;
/// The answer checks shown as habits (the level checks repeat the rubric's structure and specificity).
const HABITS: [(&str, &str, &str); 4] = [
    ("leads_with_point", "Leads with the point", "leading with the point"),
    ("has_quantified_result", "Gives a number", "giving numbers"),
    ("star_missing", "Tells the whole story", "telling the whole story"),
    ("ownership", "Your own part is clear", "making your own part clear"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Better {
    Higher,
    Lower,
    /// Neither direction is better (how much of the talking was yours).
    Neither,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Improving,
    Slipping,
    Steady,
    /// A measure where neither direction is better moved by more than its band.
    Up,
    Down,
    /// Fewer than `MIN_POINTS` interviews measured it.
    TooFew,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Interview {
    pub session_id: i64,
    pub created_at: String,
    pub title: String,
    pub company: Option<String>,
    /// The round, e.g. "Hiring manager".
    pub round: String,
    /// The model that wrote its current review.
    pub model: String,
    pub verdict_label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Point {
    pub session_id: i64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Measure {
    pub id: &'static str,
    pub label: &'static str,
    /// overall, rubric, answers, delivery, room.
    pub group: &'static str,
    /// score (1–5), percent (0–1), per_100_words, seconds, share (0–1), outlook (−2…2), warmth (−1…1).
    pub unit: &'static str,
    pub better: Better,
    /// The smallest change that counts as a direction.
    pub band: f64,
    /// Oldest first; interviews that didn't measure it are left out.
    pub points: Vec<Point>,
    /// Mean of your latest interviews (all of them while there are too few for a direction).
    pub recent: Option<f64>,
    pub recent_count: usize,
    /// Mean of the interviews before those.
    pub earlier: Option<f64>,
    pub earlier_count: usize,
    pub change: Option<f64>,
    pub direction: Direction,
    /// More interviews it needs before it shows a direction.
    pub needed: usize,
    /// How it reads in a sentence when it's getting better, and when it's slipping.
    #[serde(skip)]
    pub improving_phrase: String,
    #[serde(skip)]
    pub slipping_phrase: String,
}

/// Coaching that keeps coming up across reviews.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Theme {
    /// The latest review's wording.
    pub title: String,
    /// Reviews it came up in.
    pub count: usize,
    pub last_seen: String,
    /// It came up in your latest review.
    pub in_latest: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Standing {
    pub label: &'static str,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Where you stand overall, in one sentence.
    pub headline: String,
    /// Which areas moved, in one sentence (None when nothing did, or it's too early to say).
    pub detail: Option<String>,
    pub improving: Vec<&'static str>,
    pub slipping: Vec<&'static str>,
    /// Your highest and lowest rubric areas lately.
    pub strongest: Option<Standing>,
    pub weakest: Option<Standing>,
    /// Most frequent first; only coaching that came up in at least two reviews.
    pub recurring: Vec<Theme>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trends {
    /// The window, in days including today (None: all time).
    pub days: Option<i64>,
    /// Reviewed interviews, oldest first.
    pub interviews: Vec<Interview>,
    pub measures: Vec<Measure>,
    pub summary: Summary,
    /// The models that wrote these reviews; scores from different models aren't on quite the same scale.
    pub models: Vec<String>,
}

/// Everything one review measured, by measure id.
struct Scorecard {
    values: Vec<(&'static str, f64)>,
}

impl Scorecard {
    fn get(&self, id: &str) -> Option<f64> {
        self.values.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Mean of the rubric scores the interview gave a chance to show.
pub fn rubric_mean(a: &StoredAnalysis) -> Option<f64> {
    let scores: Vec<f64> = a.analysis.rubric.items().iter().filter_map(|(_, s)| s.score.map(f64::from)).collect();
    mean(&scores)
}

/// Mean temperature of the interviewer's substantive turns.
pub fn room(a: &StoredAnalysis) -> Option<f64> {
    let temps: Vec<f64> = a.turn_signals.iter().filter(|s| s.kind == Kind::Substantive).filter_map(|s| s.temperature).collect();
    mean(&temps)
}

fn outlook(v: Verdict) -> f64 {
    match v {
        Verdict::Strong => 2.0,
        Verdict::LeaningPositive => 1.0,
        Verdict::Mixed => 0.0,
        Verdict::LeaningNegative => -1.0,
        Verdict::Weak => -2.0,
    }
}

fn rubric_id(label: &str) -> &'static str {
    match label {
        "Clarity" => "clarity",
        "Structure" => "structure",
        "Specificity & impact" => "specificity_and_impact",
        "Role fit" => "role_fit",
        "Technical depth" => "technical_depth",
        "Curiosity" => "curiosity",
        _ => "composure",
    }
}

fn scorecard(a: &StoredAnalysis) -> Scorecard {
    let mut values = vec![];
    if let Some(m) = rubric_mean(a) {
        values.push(("overall", m));
    }
    for (label, score) in a.analysis.rubric.items() {
        if let Some(s) = score.score {
            values.push((rubric_id(label), f64::from(s)));
        }
    }
    let answer_scores: Vec<f64> = a.analysis.questions.iter().filter_map(|q| q.score.map(f64::from)).collect();
    if let Some(m) = mean(&answer_scores) {
        values.push(("answer_score", m));
    }
    for (id, _, _) in HABITS {
        let decided: Vec<bool> = a.answer_checks.iter()
            .filter(|c| c.check_id == id && c.verdict != "unclear")
            .map(|c| c.verdict == "pass")
            .collect();
        if !decided.is_empty() {
            values.push((id, decided.iter().filter(|p| **p).count() as f64 / decided.len() as f64));
        }
    }
    // Fewer words than this is a recording that lost your mic, not a way of speaking.
    let m = &a.metrics;
    if m.your_words >= MIN_WORDS {
        values.push(("fillers", m.fillers_per_100_words));
        let hedges: usize = m.hedges.values().sum();
        values.push(("hedges", 100.0 * hedges as f64 / m.your_words as f64));
        if m.answers > 0 {
            values.push(("answer_length", m.avg_answer_s));
        }
        values.push(("talk_share", m.your_share));
    }
    if let Some(t) = room(a) {
        values.push(("warmth", t));
    }
    values.push(("outlook", outlook(a.analysis.outlook.verdict)));
    Scorecard { values }
}

/// (id, label, group, unit, better, band, improving phrase, slipping phrase), in display order.
type Definition = (&'static str, &'static str, &'static str, &'static str, Better, f64, String, String);

fn definitions() -> Vec<Definition> {
    let mut out: Vec<Definition> = vec![(
        "overall", "Overall", "overall", "score", Better::Higher, 0.5, "your reviews overall".into(), "your reviews overall".into(),
    )];
    for (label, phrase) in [
        ("Clarity", "clarity"), ("Structure", "structure"), ("Specificity & impact", "specificity and impact"),
        ("Role fit", "role fit"), ("Technical depth", "technical depth"), ("Curiosity", "curiosity"), ("Composure", "composure"),
    ] {
        out.push((rubric_id(label), label, "rubric", "score", Better::Higher, 0.5, phrase.into(), phrase.into()));
    }
    out.push(("answer_score", "Answer scores", "answers", "score", Better::Higher, 0.5, "answer scores".into(), "answer scores".into()));
    for (id, label, phrase) in HABITS {
        out.push((id, label, "answers", "percent", Better::Higher, 0.15, phrase.into(), phrase.into()));
    }
    out.push(("fillers", "Filler words", "delivery", "per_100_words", Better::Lower, 1.0,
              "fewer filler words".into(), "more filler words".into()));
    out.push(("hedges", "Hedging", "delivery", "per_100_words", Better::Lower, 0.5, "less hedging".into(), "more hedging".into()));
    out.push(("answer_length", "Answer length", "delivery", "seconds", Better::Neither, 15.0, String::new(), String::new()));
    out.push(("talk_share", "Your share of the talking", "delivery", "share", Better::Neither, 0.05, String::new(), String::new()));
    out.push(("warmth", "How the room felt", "room", "warmth", Better::Higher, 0.2,
              "a warmer room".into(), "a cooler room".into()));
    out.push(("outlook", "The review's outlook", "room", "outlook", Better::Higher, 1.0,
              "the reviews' outlook".into(), "the reviews' outlook".into()));
    out
}

/// Compares your latest interviews with the ones before them (see the module's rule).
pub fn judge(values: &[f64], better: Better, band: f64) -> (Option<f64>, usize, Option<f64>, usize, Direction) {
    let n = values.len();
    if n < MIN_POINTS {
        return (mean(values), n, None, 0, Direction::TooFew);
    }
    let k = RECENT.min(n / 2);
    let (earlier, recent) = values.split_at(n - k);
    let (r, e) = (mean(recent).unwrap_or(0.0), mean(earlier).unwrap_or(0.0));
    let change = r - e;
    // A hair under the band still counts, so 3.5 → 4.0 isn't lost to rounding.
    let direction = if change.abs() + 1e-9 < band {
        Direction::Steady
    } else {
        match better {
            Better::Neither if change > 0.0 => Direction::Up,
            Better::Neither => Direction::Down,
            Better::Higher if change > 0.0 => Direction::Improving,
            Better::Lower if change < 0.0 => Direction::Improving,
            _ => Direction::Slipping,
        }
    };
    (Some(r), k, Some(e), n - k, direction)
}

fn measures(cards: &[(i64, Scorecard)]) -> Vec<Measure> {
    definitions()
        .into_iter()
        .filter_map(|(id, label, group, unit, better, band, improving_phrase, slipping_phrase)| {
            let points: Vec<Point> = cards.iter().filter_map(|(sid, c)| Some(Point { session_id: *sid, value: c.get(id)? })).collect();
            // Habits need a scorer; leave them out entirely until one has judged an answer.
            if points.is_empty() && group == "answers" && id != "answer_score" {
                return None;
            }
            let values: Vec<f64> = points.iter().map(|p| p.value).collect();
            let (recent, recent_count, earlier, earlier_count, direction) = judge(&values, better, band);
            Some(Measure {
                id, label, group, unit, better, band,
                change: recent.zip(earlier).map(|(r, e)| r - e),
                needed: MIN_POINTS.saturating_sub(points.len()),
                points, recent, recent_count, earlier, earlier_count, direction, improving_phrase, slipping_phrase,
            })
        })
        .collect()
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A count that starts a sentence: "One", "Two", then digits.
fn counted(n: usize) -> String {
    match n {
        1 => "One".into(),
        2 => "Two".into(),
        n => n.to_string(),
    }
}

fn one_decimal(x: f64) -> String {
    format!("{x:.1}")
}

/// The areas that moved, biggest move (against its band) first; the overall score and the room aren't
/// areas of yours, so they're left to their own lines.
fn moved(measures: &[Measure], direction: Direction) -> Vec<&Measure> {
    let mut out: Vec<&Measure> = measures.iter()
        .filter(|m| m.direction == direction && matches!(m.group, "rubric" | "answers" | "delivery"))
        .collect();
    out.sort_by(|a, b| {
        let size = |m: &Measure| m.change.unwrap_or(0.0).abs() / m.band;
        size(b).total_cmp(&size(a))
    });
    out
}

fn summarise(measures: &[Measure], reviewed: usize, themes: Vec<Theme>) -> Summary {
    let overall = measures.iter().find(|m| m.id == "overall");
    let headline = match overall {
        _ if reviewed == 0 => "Your progress shows here once an interview has been reviewed.".to_string(),
        Some(m) if m.points.is_empty() => format!(
            "{} reviewed interview{}, but {} couldn't score your answers; they may not have been recorded.",
            counted(reviewed),
            if reviewed == 1 { "" } else { "s" },
            if reviewed == 1 { "its review" } else { "the reviews" },
        ),
        Some(m) if m.direction == Direction::TooFew => {
            let scored = m.points.len();
            let so_far = if scored == reviewed {
                format!("{} reviewed interview{} so far.", counted(reviewed), if reviewed == 1 { "" } else { "s" })
            } else {
                format!("{} reviewed interviews so far, {} with scores.", counted(reviewed), scored)
            };
            format!("{so_far} {} more and Janus can show which areas are getting better or worse.", counted(m.needed.max(1)))
        }
        Some(m) => {
            let (r, e) = (one_decimal(m.recent.unwrap_or(0.0)), one_decimal(m.earlier.unwrap_or(0.0)));
            let lately = if m.recent_count == 1 { "in your latest interview".to_string() } else { format!("across your last {}", m.recent_count) };
            match m.direction {
                Direction::Improving => format!("Your overall score is getting better: {r} out of 5 {lately}, up from {e}."),
                Direction::Slipping => format!("Your overall score has slipped: {r} out of 5 {lately}, down from {e}."),
                _ => format!("Your overall score is holding steady at about {r} out of 5."),
            }
        }
        None => format!("{} reviewed interviews, none with rubric scores yet.", counted(reviewed)),
    };
    let improving = moved(measures, Direction::Improving);
    let slipping = moved(measures, Direction::Slipping);
    // "Getting better: a and b." With more than three, the biggest three are named.
    let part = |ms: &[&Measure], heading: &str, slipping: bool| -> Option<String> {
        let phrases: Vec<String> = ms.iter().take(3)
            .map(|m| if slipping { m.slipping_phrase.clone() } else { m.improving_phrase.clone() })
            .collect();
        match ms.len() {
            0 => None,
            1..=3 => Some(format!("{heading}: {}.", and_list(&phrases))),
            n => Some(format!("{heading} in {n} areas, most of all {}.", and_list(&phrases))),
        }
    };
    let parts: Vec<String> = [part(&improving, "Getting better", false), part(&slipping, "Slipping", true)].into_iter().flatten().collect();
    let detail = match parts.is_empty() {
        false => Some(parts.join(" ")),
        true if overall.is_some_and(|m| m.direction != Direction::TooFew) => Some("No single area has moved much.".to_string()),
        true => None,
    };
    // Lately: the mean of the latest interviews, however few there are.
    let rubric: Vec<Standing> = measures.iter()
        .filter(|m| m.group == "rubric")
        .filter_map(|m| Some(Standing { label: m.label, value: m.recent? }))
        .collect();
    let strongest = rubric.iter().max_by(|a, b| a.value.total_cmp(&b.value)).cloned();
    let weakest = rubric.iter().min_by(|a, b| a.value.total_cmp(&b.value)).cloned();
    // Only worth naming when there's a real gap between them.
    let (strongest, weakest) = match (strongest, weakest) {
        (Some(s), Some(w)) if s.value - w.value >= 0.5 => (Some(s), Some(w)),
        _ => (None, None),
    };
    Summary {
        headline,
        detail,
        improving: improving.iter().map(|m| m.label).collect(),
        slipping: slipping.iter().map(|m| m.label).collect(),
        strongest,
        weakest,
        recurring: themes,
    }
}

/// Two coaching titles say the same thing: they share at least two distinctive words, and most of
/// the shorter one's ("Lead with the result" and "Lead with results first").
fn same_coaching(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shared = a.intersection(b).count();
    a == b || (shared >= 2 && shared as f64 / a.len().min(b.len()) as f64 >= 0.6)
}

/// Coaching titles from each review (oldest first), merged across reviews.
fn recurring(reviews: &[(String, Vec<String>)]) -> Vec<Theme> {
    // Each theme keeps every wording it was given, and a new title joins it if it matches any of them.
    let mut themes: Vec<(Vec<BTreeSet<String>>, Theme, usize)> = vec![];
    let latest = reviews.len().saturating_sub(1);
    for (i, (date, titles)) in reviews.iter().enumerate() {
        let mut seen_here: Vec<usize> = vec![];
        for title in titles {
            let words = key_words(title);
            match themes.iter().position(|(wordings, _, _)| wordings.iter().any(|w| same_coaching(w, &words))) {
                Some(t) => {
                    let (wordings, theme, last) = &mut themes[t];
                    wordings.push(words);
                    theme.title = title.clone();
                    theme.last_seen = date.clone();
                    theme.in_latest = i == latest;
                    if !seen_here.contains(&t) {
                        theme.count += 1;
                        seen_here.push(t);
                    }
                    *last = i;
                }
                None => {
                    seen_here.push(themes.len());
                    themes.push((vec![words], Theme { title: title.clone(), count: 1, last_seen: date.clone(), in_latest: i == latest }, i));
                }
            }
        }
    }
    let mut out: Vec<(Theme, usize)> = themes.into_iter().filter(|(_, t, _)| t.count >= 2).map(|(_, t, last)| (t, last)).collect();
    out.sort_by(|(a, la), (b, lb)| b.count.cmp(&a.count).then(lb.cmp(la)));
    out.into_iter().map(|(t, _)| t).take(4).collect()
}

/// Whether an interview counts: reviewed, real, and in the main list.
fn counts(s: &Session, archived_roles: &BTreeSet<i64>) -> bool {
    !s.practice && s.deleted_at.is_none() && s.archived_at.is_none() && !s.role_id.is_some_and(|r| archived_roles.contains(&r))
}

/// The start of the window: today plus the previous `days − 1` days, in local time.
fn window_start(days: i64, now: DateTime<Local>) -> DateTime<Local> {
    let day = now.date_naive() - Duration::days(days.max(1) - 1);
    Local.from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight")).earliest().unwrap_or(now)
}

fn in_window(created_at: &str, start: Option<DateTime<Local>>, now: DateTime<Local>) -> bool {
    match DateTime::parse_from_rfc3339(created_at) {
        Ok(at) => at <= now && start.is_none_or(|s| at >= s),
        Err(_) => false,
    }
}

pub fn build(db: &Db, days: Option<i64>) -> Result<Trends> {
    build_at(db, days, Local::now())
}

pub fn build_at(db: &Db, days: Option<i64>, now: DateTime<Local>) -> Result<Trends> {
    let archived_roles: BTreeSet<i64> = db.roles()?.into_iter().filter(|r| r.archived_at.is_some()).map(|r| r.id).collect();
    let start = days.map(|d| window_start(d, now));
    let mut sessions = db.list_sessions()?;
    sessions.retain(|s| counts(s, &archived_roles) && in_window(&s.created_at, start, now));
    sessions.sort_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));

    let mut interviews = vec![];
    let mut cards = vec![];
    let mut coaching = vec![];
    let mut models: Vec<String> = vec![];
    for s in sessions {
        let Some(report) = pipeline::current_report(db, s.id)? else { continue };
        let context = &report.analysis.context;
        interviews.push(Interview {
            session_id: s.id,
            created_at: s.created_at.clone(),
            title: s.title.clone(),
            company: s.company.clone().or_else(|| context.company.clone()),
            round: s.stage.unwrap_or(context.stage).label().to_string(),
            model: report.model.clone(),
            verdict_label: report.analysis.outlook.verdict.label(),
        });
        if !models.contains(&report.model) {
            models.push(report.model.clone());
        }
        cards.push((s.id, scorecard(&report)));
        coaching.push((s.created_at.clone(), report.analysis.coaching.iter().map(|c| c.title.clone()).collect()));
    }
    let measures = measures(&cards);
    let summary = summarise(&measures, interviews.len(), recurring(&coaching));
    Ok(Trends { days, interviews, measures, summary, models })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_few_interviews_have_no_direction() {
        let (recent, n, earlier, _, d) = judge(&[3.0, 4.0], Better::Higher, 0.5);
        assert_eq!((recent, n, earlier, d), (Some(3.5), 2, None, Direction::TooFew));
    }

    #[test]
    fn latest_interviews_are_compared_with_the_ones_before() {
        // Six points: the last three against the first three.
        let (recent, n, earlier, m, d) = judge(&[2.0, 2.0, 3.0, 4.0, 4.0, 4.0], Better::Higher, 0.5);
        assert_eq!((n, m, d), (3, 3, Direction::Improving));
        assert!((recent.unwrap() - 4.0).abs() < 1e-9 && (earlier.unwrap() - 7.0 / 3.0).abs() < 1e-9);
        // Three points: the latest against the two before; never more than half are "lately".
        let (_, n, _, m, d) = judge(&[4.0, 4.0, 3.0], Better::Higher, 0.5);
        assert_eq!((n, m, d), (1, 2, Direction::Slipping));
        // Ten points: the last three against all seven before.
        let (_, n, _, m, _) = judge(&[3.0; 10], Better::Higher, 0.5);
        assert_eq!((n, m), (3, 7));
    }

    #[test]
    fn small_changes_are_steady_and_exactly_the_band_counts() {
        assert_eq!(judge(&[3.0, 3.0, 3.0, 3.4], Better::Higher, 0.5).4, Direction::Steady);
        assert_eq!(judge(&[3.0, 3.5, 4.0, 4.0], Better::Higher, 0.5).4, Direction::Improving);
    }

    #[test]
    fn fewer_fillers_is_better_and_talk_share_has_no_better() {
        assert_eq!(judge(&[5.0, 5.0, 2.0, 2.0], Better::Lower, 1.0).4, Direction::Improving);
        assert_eq!(judge(&[2.0, 2.0, 5.0, 5.0], Better::Lower, 1.0).4, Direction::Slipping);
        assert_eq!(judge(&[0.4, 0.4, 0.6, 0.6], Better::Neither, 0.05).4, Direction::Up);
        assert_eq!(judge(&[0.6, 0.6, 0.4, 0.4], Better::Neither, 0.05).4, Direction::Down);
    }

    #[test]
    fn coaching_that_keeps_coming_up_is_merged_across_wordings() {
        let reviews = vec![
            ("2026-09-01".to_string(), vec!["Lead with the result".to_string(), "Quantify your impact".to_string()]),
            ("2026-09-08".to_string(), vec!["Lead with results first".to_string(), "Ask sharper questions".to_string()]),
            ("2026-09-15".to_string(), vec!["Quantify impact with numbers".to_string(), "Slow down".to_string()]),
        ];
        let themes = recurring(&reviews);
        assert_eq!(themes.len(), 2, "{themes:?}");
        // Equal counts: the one seen most recently first.
        assert_eq!((themes[0].title.as_str(), themes[0].count, themes[0].in_latest), ("Quantify impact with numbers", 2, true));
        assert_eq!((themes[1].title.as_str(), themes[1].last_seen.as_str(), themes[1].in_latest), ("Lead with results first", "2026-09-08", false));
        // "Lead the conversation" shares only "lead": not the same coaching.
        assert!(!same_coaching(&key_words("Lead with the result"), &key_words("Lead the conversation")));
    }

    #[test]
    fn a_title_repeated_within_one_review_counts_once() {
        let reviews = vec![("2026-09-01".to_string(), vec!["Lead with the result".to_string(), "Lead with results".to_string()])];
        assert!(recurring(&reviews).is_empty());
    }

    #[test]
    fn the_window_includes_today_and_leaves_out_the_future() {
        let now = Local.with_ymd_and_hms(2026, 10, 9, 15, 0, 0).unwrap();
        let start = Some(window_start(30, now));
        let at = |d: DateTime<Local>| d.to_rfc3339();
        assert!(in_window(&at(Local.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap()), start, now));
        assert!(!in_window(&at(Local.with_ymd_and_hms(2026, 9, 9, 23, 59, 0).unwrap()), start, now));
        assert!(!in_window(&at(Local.with_ymd_and_hms(2026, 10, 9, 16, 0, 0).unwrap()), start, now));
        assert!(!in_window("not a date", None, now));
    }

    #[test]
    fn lists_read_naturally() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(and_list(&s(&["structure"])), "structure");
        assert_eq!(and_list(&s(&["structure", "fewer filler words"])), "structure and fewer filler words");
        assert_eq!(and_list(&s(&["a", "b", "c"])), "a, b and c");
    }
}
