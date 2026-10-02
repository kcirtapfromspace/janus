//! Talk metrics computed in code — deterministic, so they're comparable across interviews over time.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::merge::{talk_time, to_turns};
use crate::models::{INTERVIEWER, Mode, Segment, YOU};

/// Shorter turns are acknowledgements ("Yeah, exactly."), not answers.
const ANSWER_MIN_WORDS: usize = 8;
/// Shorter overlaps are backchannels ("mm-hmm"), not interruptions.
const INTERRUPT_MIN_WORDS: usize = 3;

// Whisper surrounds filler words with commas, which separates them from real uses
// ("I like it" vs. "it was, like, fine").
static FILLERS: LazyLock<Vec<(&str, Regex)>> = LazyLock::new(|| {
    [
        ("um", r"\bu+m+\b"),
        ("uh", r"\bu+h+\b"),
        ("like", r",\s*like\b|\blike\s*,"),
        ("you know", r",\s*you know\b|\byou know\s*[,.?]"),
        ("i mean", r"(?:^|[,.]\s*)i mean\b"),
    ]
    .into_iter()
    .map(|(k, p)| (k, Regex::new(p).unwrap()))
    .collect()
});
static HEDGES: LazyLock<Vec<(&str, Regex)>> = LazyLock::new(|| {
    [
        ("i guess", r"\bi guess\b"),
        ("honestly", r"\bhonestly\b|\bto be honest\b"),
        ("not sure", r"\bi'?m not (?:totally |really |entirely |quite )?sure\b"),
        ("probably / maybe", r"\bprobably\b|\bmaybe\b"),
    ]
    .into_iter()
    .map(|(k, p)| (k, Regex::new(p).unwrap()))
    .collect()
});
/// "kind of" / "sort of" as a hedge — but not "what kind of product" (the regex crate has no lookbehind).
static KIND_OF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:(\w+)\s+)?(?:kind|sort) of\b").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9']+").unwrap());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TalkMetrics {
    pub duration_s: f64,
    pub your_talk_s: f64,
    pub their_talk_s: f64,
    /// Share of total talk time, 0..1.
    pub your_share: f64,
    pub answers: usize,
    pub avg_answer_s: f64,
    pub longest_answer_s: f64,
    pub longest_answer_at: f64,
    pub your_words: usize,
    pub words_per_minute: Option<f64>,
    pub fillers: BTreeMap<String, usize>,
    pub fillers_per_100_words: f64,
    pub hedges: BTreeMap<String, usize>,
    pub questions_you_asked: usize,
    pub median_response_gap_s: Option<f64>,
    /// Dual-track only: overlaps are unreliable in mixed audio.
    pub you_interrupted: Option<usize>,
    pub they_interrupted: Option<usize>,
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

fn count_fillers(text: &str) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> =
        FILLERS.iter().map(|(k, re)| (k.to_string(), re.find_iter(text).count())).collect();
    let hedge_kind_of = KIND_OF
        .captures_iter(text)
        .filter(|c| !matches!(c.get(1).map(|m| m.as_str()), Some("what" | "this" | "that" | "the" | "any")))
        .count();
    out.insert("kind of / sort of".into(), hedge_kind_of);
    out
}

fn interruptions(segments: &[Segment], by: &str) -> usize {
    segments
        .iter()
        .filter(|seg| seg.speaker == by && seg.text.split_whitespace().count() >= INTERRUPT_MIN_WORDS)
        .filter(|seg| segments.iter().any(|o| o.speaker != by && o.start < seg.start && seg.start < o.end - 0.3))
        .count()
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    Some(if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 })
}

pub fn compute(segments: &[Segment], mode: Mode) -> TalkMetrics {
    let turns = to_turns(segments);
    let talk = talk_time(segments);
    let yours = talk.get(YOU).copied().unwrap_or(0.0);
    let theirs: f64 = talk.iter().filter(|(k, _)| k.as_str() != YOU).map(|(_, v)| v).sum();

    let your_turns: Vec<_> = turns.iter().filter(|t| t.speaker == YOU).collect();
    let answers: Vec<_> = your_turns.iter().filter(|t| t.text.split_whitespace().count() >= ANSWER_MIN_WORDS).collect();
    let longest = answers.iter().max_by(|a, b| (a.end - a.start).total_cmp(&(b.end - b.start)));

    let your_text = your_turns.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ").to_lowercase();
    let words = WORD.find_iter(&your_text).count();
    let fillers = count_fillers(&your_text);
    let filler_total: usize = fillers.values().sum();

    let gaps: Vec<f64> = turns
        .windows(2)
        .filter(|w| w[0].speaker.starts_with(INTERVIEWER) && w[1].speaker == YOU)
        .map(|w| w[1].start - w[0].end)
        .collect();

    let first = segments.iter().map(|s| s.start).fold(f64::INFINITY, f64::min);
    let last = segments.iter().map(|s| s.end).fold(f64::NEG_INFINITY, f64::max);

    TalkMetrics {
        duration_s: if segments.is_empty() { 0.0 } else { round(last - first, 1) },
        your_talk_s: round(yours, 1),
        their_talk_s: round(theirs, 1),
        your_share: if yours + theirs > 0.0 { round(yours / (yours + theirs), 3) } else { 0.0 },
        answers: answers.len(),
        avg_answer_s: if answers.is_empty() {
            0.0
        } else {
            round(answers.iter().map(|t| t.end - t.start).sum::<f64>() / answers.len() as f64, 1)
        },
        longest_answer_s: longest.map_or(0.0, |t| round(t.end - t.start, 1)),
        longest_answer_at: longest.map_or(0.0, |t| round(t.start, 1)),
        your_words: words,
        words_per_minute: (yours > 30.0).then(|| (words as f64 / (yours / 60.0)).round()),
        fillers,
        fillers_per_100_words: if words > 0 { round(100.0 * filler_total as f64 / words as f64, 1) } else { 0.0 },
        hedges: HEDGES.iter().map(|(k, re)| (k.to_string(), re.find_iter(&your_text).count())).collect(),
        questions_you_asked: your_turns.iter().map(|t| t.text.matches('?').count()).sum(),
        median_response_gap_s: median(gaps).map(|g| round(g, 2)),
        you_interrupted: (mode == Mode::Dual).then(|| interruptions(segments, YOU)),
        they_interrupted: (mode == Mode::Dual).then(|| interruptions(segments, INTERVIEWER)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample_segments() -> Vec<Segment> {
        vec![
            Segment::new(0.0, 4.0, "Tell me about a tough prioritization call.", INTERVIEWER),
            Segment::new(5.0, 20.0, "Um, so, like, we shipped the onboarding redesign in eight weeks.", YOU),
            Segment::new(21.0, 30.0, "Activation went from sixty to seventy eight percent. What does success look like?", YOU),
            Segment::new(31.0, 36.0, "Great. Our recruiter will reach out tomorrow to set up the onsite.", INTERVIEWER),
        ]
    }

    #[test]
    fn metrics_from_segments() {
        let m = compute(&sample_segments(), Mode::Dual);
        assert!((m.your_share - 24.0 / 33.0).abs() < 0.01);
        assert_eq!((m.fillers["um"], m.fillers["like"]), (1, 1));
        assert_eq!(m.questions_you_asked, 1);
        assert_eq!(m.answers, 1); // the two "you" segments form one turn
        assert_eq!(m.median_response_gap_s, Some(1.0));
        assert_eq!((m.you_interrupted, m.they_interrupted), (Some(0), Some(0)));
        assert_eq!(compute(&sample_segments(), Mode::Single).you_interrupted, None);
    }

    #[test]
    fn interruption_counts_real_overlaps_but_not_backchannels() {
        let segs = [
            Segment::new(0.0, 10.0, "So the question I have is about how you plan roadmaps", INTERVIEWER),
            Segment::new(4.0, 5.0, "Mm-hmm.", YOU),
            Segment::new(6.0, 9.0, "Sorry, can I jump in here quickly?", YOU),
        ];
        let m = compute(&segs, Mode::Dual);
        assert_eq!((m.you_interrupted, m.they_interrupted), (Some(1), Some(0)));
    }

    #[test]
    fn kind_of_counts_hedges_but_not_what_kind_of() {
        let f = count_fillers("i kind of moved into product. what kind of team is it? sort of, yeah");
        assert_eq!(f["kind of / sort of"], 2);
    }
}
