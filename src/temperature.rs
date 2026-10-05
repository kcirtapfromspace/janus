//! The room's temperature: how the interviewer reacted, turn by turn, from what they said (Jev's
//! checks) and how they said it (prosody.rs), with your answers in between. Behaviour, not
//! mind-reading: every number is an observable cue with a timestamp.
//!
//! The conversation is rebuilt from segments rather than `merge::to_turns`, because in dual-track
//! transcripts an interviewer's "mm-hmm" splits your answer into pieces. Here an answer is all of
//! your speech between two substantive interviewer turns, and the "mm-hmm"s are listening cues.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::models::{Segment, YOU};
use crate::prosody::{self, Frame, VoiceFeatures};
use crate::scoring::{Assessment, Check, CheckKind, CheckSet, Rule, ScoreInput, Scorer, Verdict};

/// The formula below, versioned so stored timelines say how they were computed.
pub const METHOD: &str = "timeline-v1";
/// Turns at least this long are always substantive.
const SUBSTANTIVE_MIN_WORDS: usize = 4;
/// Short replies that are listening, not taking a turn.
const BACKCHANNELS: &[&str] = &[
    "mm-hmm", "mhm", "mm", "hmm", "uh-huh", "yeah", "yep", "yes", "right", "okay", "ok", "great", "awesome", "sure",
    "got it", "interesting", "nice", "cool", "exactly", "totally", "wow", "gotcha", "perfect", "sounds good",
];
/// Same-speaker segments further apart than this are separate turns.
const TURN_BREAK_S: f64 = 8.0;
/// Words of your answer Jev sees (the end of it, which the interviewer is reacting to).
const CANDIDATE_WORDS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An interviewer turn that says something: a question, a reaction, information.
    Substantive,
    /// An interviewer "mm-hmm" while you talk.
    Backchannel,
    /// Your answer, joined across the interviewer's backchannels.
    Answer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConvTurn {
    pub kind: Kind,
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: usize,
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn is_backchannel(text: &str) -> bool {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == ' ' { c } else { ' ' })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() || word_count(&cleaned) >= SUBSTANTIVE_MIN_WORDS {
        return false;
    }
    // Every word (or two-word phrase) must be a listening sound.
    let words: Vec<&str> = cleaned.split(' ').collect();
    let mut i = 0;
    while i < words.len() {
        if i + 1 < words.len() && BACKCHANNELS.contains(&format!("{} {}", words[i], words[i + 1]).as_str()) {
            i += 2;
        } else if BACKCHANNELS.contains(&words[i]) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

/// The conversation in time order: substantive interviewer turns, their backchannels, and your
/// answers joined across those backchannels.
pub fn conversation(segments: &[Segment]) -> Vec<ConvTurn> {
    let mut sorted: Vec<&Segment> = segments.iter().filter(|s| !s.text.trim().is_empty()).collect();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Same-speaker runs first (like `to_turns`), then classify and join.
    let mut runs: Vec<ConvTurn> = vec![];
    for s in sorted {
        let you = s.speaker == YOU;
        match runs.last_mut() {
            Some(last) if last.speaker == s.speaker && s.start - last.end < TURN_BREAK_S => {
                last.end = last.end.max(s.end);
                last.text.push(' ');
                last.text.push_str(s.text.trim());
                last.words += word_count(&s.text);
            }
            _ => runs.push(ConvTurn {
                kind: if you { Kind::Answer } else { Kind::Substantive },
                speaker: s.speaker.clone(),
                start: s.start,
                end: s.end,
                text: s.text.trim().to_string(),
                words: word_count(&s.text),
            }),
        }
    }
    for run in runs.iter_mut().filter(|r| r.kind == Kind::Substantive) {
        if is_backchannel(&run.text) {
            run.kind = Kind::Backchannel;
        }
    }
    // Join answers separated only by backchannels; the backchannels stay as listening cues.
    let mut out: Vec<ConvTurn> = vec![];
    for run in runs {
        if run.kind == Kind::Answer {
            let joinable = out.iter().rposition(|t| t.kind != Kind::Backchannel).filter(|&i| out[i].kind == Kind::Answer);
            if let Some(i) = joinable {
                let answer = &mut out[i];
                answer.end = answer.end.max(run.end);
                answer.text.push(' ');
                answer.text.push_str(&run.text);
                answer.words += run.words;
                continue;
            }
        }
        out.push(run);
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

// --- Jev's checks on interviewer turns ------------------------------------------------------------

/// One substantive interviewer turn, with the question it follows and what you'd just said.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnInput {
    pub question: String,
    pub candidate_said: String,
    pub interviewer_says: String,
}

impl ScoreInput for TurnInput {
    fn state(&self) -> Value {
        json!({"question": self.question, "candidate_said": self.candidate_said, "interviewer_says": self.interviewer_says})
    }

    fn claude_body(&self) -> String {
        format!("<question>\n{}\n</question>\n\n<candidate_said>\n{}\n</candidate_said>\n\n<interviewer_says>\n{}\n</interviewer_says>",
                self.question, self.candidate_said, self.interviewer_says)
    }
}

pub const CLAUDE_PROMPT: &str = include_str!("../prompts/interviewer_scorer_v1.md");

fn yes_no(id: &'static str, instructions: &str, yes: &str, no: &str) -> Check {
    Check { id, instructions: instructions.into(), kind: CheckKind::YesNo { yes: yes.into(), no: no.into() } }
}

/// The checks each substantive interviewer turn gets.
pub fn interviewer_checks() -> Vec<Check> {
    vec![
        Check {
            id: "tone",
            instructions: "What is the tone of `interviewer_says`, towards the candidate?".into(),
            kind: CheckKind::Choice {
                options: vec![
                    ("warm".into(), "Warm: friendly, enthusiastic or appreciative, such as praise, agreement, laughter, or \
                                     building rapport."
                        .into()),
                    ("neutral".into(), "Neutral: matter-of-fact, such as a plain question, information or a transition, with \
                                        no clear warmth or coolness."
                        .into()),
                    ("cool".into(), "Cool: curt, sceptical, impatient or flat, such as pushback, cutting the candidate off, \
                                     or moving on abruptly."
                        .into()),
                ],
            },
        },
        yes_no("positive_reaction", "Does `interviewer_says` react positively to `candidate_said`?",
               "It praises, agrees with, or shows enthusiasm or appreciation for what the candidate just said.",
               "No reaction to it, or a neutral or negative one. A plain 'okay' or simply asking the next question doesn't count."),
        yes_no("builds_on_answer", "Does `interviewer_says` follow up on something specific from `candidate_said`?",
               "It asks about, or refers to, a detail the candidate just gave.",
               "It moves to a new topic, or asks something that doesn't depend on what the candidate said."),
        yes_no("pushback", "Does `interviewer_says` challenge, doubt or correct the candidate, or ask again because \
                            `candidate_said` didn't answer `question`?",
               "It disagrees, questions a claim, corrects the candidate, or re-asks the same thing.",
               "It accepts the answer, or simply moves on."),
        yes_no("selling", "Is `interviewer_says` selling the role, team or company, or sharing inside information?",
               "It talks up the opportunity, the team or the mission, or shares non-public details.",
               "It asks questions or gives neutral information."),
        yes_no("next_steps", "Does `interviewer_says` mention next steps, a timeline, or who the candidate would meet next?",
               "Next rounds, an onsite, the recruiter following up, a timeline, or named next interviewers.",
               "No mention of what happens next."),
    ]
}

fn classify_turn(check_id: &str, v: &Verdict) -> &'static str {
    match check_id {
        "tone" => match v.pick.as_str() {
            "warm" => "pass",
            "cool" => "fail",
            _ => "unclear",
        },
        _ => match v.value {
            p if p >= 0.7 => "pass",
            p if p <= 0.3 => "fail",
            _ => "unclear",
        },
    }
}

/// The interviewer-turn check set and its pass rule (docs/eval/interviewer-decision.md).
pub fn interviewer_set() -> CheckSet {
    CheckSet {
        id: "interviewer",
        checks: interviewer_checks(),
        claude_prompt: CLAUDE_PROMPT,
        classify: classify_turn,
        rule: Rule {
            min_score: 0.85,
            min_score_overrides: &[("tone", 0.80)],
            min_within_one: 0.90,
            max_behind_best: 0.05,
            min_stability: 0.95,
            max_p95_ms: None,
            max_ece: None,
        },
        input_from: |m| {
            let field = |k: &str| m.get(k).and_then(Value::as_str).map(String::from).with_context(|| format!("item has no {k}"));
            Ok(Box::new(TurnInput {
                question: field("question")?,
                candidate_said: field("candidate_said")?,
                interviewer_says: field("interviewer_says")?,
            }))
        },
    }
}

/// Which checks passed their comparison (docs/eval/interviewer-decision.md). A check that didn't
/// weighs nothing in the temperature and isn't marked or quoted in the report.
pub const EVAL_PASSED: &[&str] = &["tone", "positive_reaction", "builds_on_answer", "pushback", "selling", "next_steps"];

/// Chart markers: cues shown on the timeline, but (except pushback) not weighed, because they
/// cluster in the ritual close of every interview.
pub const MARKERS: [(&str, &str, &str); 3] =
    [("next_steps", "★", "next steps"), ("selling", "$", "selling the role"), ("pushback", "!", "pushback")];

fn last_words(text: &str, n: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words[words.len().saturating_sub(n)..].join(" ")
}

/// Jev's input for each substantive turn, by index into the conversation.
pub fn turn_inputs(convo: &[ConvTurn]) -> Vec<(usize, TurnInput)> {
    let mut out = vec![];
    let mut question = String::new();
    let mut answer = String::new();
    for (i, t) in convo.iter().enumerate() {
        match t.kind {
            Kind::Answer => answer = last_words(&t.text, CANDIDATE_WORDS),
            Kind::Backchannel => {}
            Kind::Substantive => {
                out.push((i, TurnInput { question: question.clone(), candidate_said: answer.clone(), interviewer_says: t.text.clone() }));
                question = t.text.clone();
                answer.clear();
            }
        }
    }
    out
}

// --- the timeline --------------------------------------------------------------------------------

/// Everything measured about one conversation turn, as stored in `turn_signals`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub turn_idx: usize,
    pub kind: Kind,
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Jev's verdicts by check id (substantive turns only).
    pub checks: BTreeMap<String, Verdict>,
    pub voice: Option<VoiceFeatures>,
    /// Robust z-scores against the same speaker's own interview.
    pub z: BTreeMap<String, f64>,
    /// Interviewer backchannels per minute during the answer just before (substantive turns).
    pub backchannel_rate: Option<f64>,
    /// Seconds from the end of your answer to this turn; negative = they started while you talked.
    pub latency_s: Option<f64>,
    /// What the call's video showed of the other people while you answered (answers only).
    pub video: Option<crate::video::VideoCues>,
    pub temperature: Option<f64>,
    /// The smoothed line at this turn.
    pub smoothed: Option<f64>,
}

/// Weights of `timeline-v1` (see `temperature_of`).
const W_POSITIVE: f64 = 0.3;
const W_BUILDS: f64 = 0.15;
const W_PUSHBACK: f64 = 0.4;
const W_ENERGY: f64 = 0.1;
const W_PITCH_VAR: f64 = 0.1;
const W_BACKCHANNEL: f64 = 0.1;
const Z_CLIP: f64 = 1.5;
/// Smoothing time constant for the line, in seconds.
const TAU_S: f64 = 120.0;
/// A turn at least this warm or cool shows in colour and can be a moment; below it, it's neutral.
pub const CLEAR: f64 = 0.3;

/// Turns after your first answer: the greeting before it can't be a reaction to you, and every
/// interview opens warmly, so it doesn't count towards shifts or moments.
fn after_opening(signals: &[Signal]) -> impl Iterator<Item = &Signal> {
    let first_answer = signals.iter().position(|s| s.kind == Kind::Answer).unwrap_or(signals.len());
    signals[first_answer..].iter()
}

fn p_yes(checks: &BTreeMap<String, Verdict>, id: &str) -> Option<f64> {
    checks.get(id).and_then(|v| v.probabilities.get("yes").copied().or(Some(v.value)))
}

/// Whether a yes/no check that passed its comparison clearly says yes for this turn.
pub fn flagged(signal: &Signal, id: &str) -> bool {
    EVAL_PASSED.contains(&id) && p_yes(&signal.checks, id).is_some_and(|p| p >= 0.7)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) { (values[mid - 1] + values[mid]) / 2.0 } else { values[mid] })
}

/// `timeline-v1`: tone (P(warm) − P(cool)), plus the probability of each weighted yes/no cue,
/// plus how the interviewer sounded as z-scores against their own call, clipped to ±1.5. A plain
/// question with no cues lands near 0. Checks that didn't pass their comparison weigh nothing.
pub fn temperature_of(signal: &Signal) -> f64 {
    temperature_with(signal, EVAL_PASSED)
}

fn temperature_with(signal: &Signal, eval_passed: &[&str]) -> f64 {
    let passed = |id: &str| eval_passed.contains(&id);
    let rel = |id: &str| if passed(id) { p_yes(&signal.checks, id).unwrap_or(0.0) } else { 0.0 };
    let tone = match (passed("tone"), signal.checks.get("tone")) {
        (true, Some(v)) => v.probabilities.get("warm").unwrap_or(&0.0) - v.probabilities.get("cool").unwrap_or(&0.0),
        _ => 0.0,
    };
    let z = |k: &str| signal.z.get(k).copied().unwrap_or(0.0).clamp(-Z_CLIP, Z_CLIP);
    let s = tone + W_POSITIVE * rel("positive_reaction") + W_BUILDS * rel("builds_on_answer") - W_PUSHBACK * rel("pushback")
        + W_ENERGY * z("energy") + W_PITCH_VAR * z("pitch_var") + W_BACKCHANNEL * z("backchannel_rate");
    (1.2 * s).tanh()
}

/// Fill in temperatures and the smoothed line for the substantive turns.
pub fn score(signals: &mut [Signal]) {
    let mut ema: Option<(f64, f64)> = None; // (time, value)
    for s in signals.iter_mut().filter(|s| s.kind == Kind::Substantive) {
        let t = temperature_of(s);
        s.temperature = Some(t);
        let next = match ema {
            None => t,
            Some((at, v)) => {
                let alpha = 1.0 - (-(s.start - at).max(0.0) / TAU_S).exp();
                v + alpha * (t - v)
            }
        };
        ema = Some((s.start, next));
        s.smoothed = Some(next);
    }
}

/// Voice features and listening cues for every turn, from the session's audio.
pub struct Audio {
    /// The interviewer's frames (system track, or the mixed track for single-track sessions).
    pub interviewer: Vec<Frame>,
    /// Your frames (mic track, or the same mixed track).
    pub you: Vec<Frame>,
    /// Whether the tracks are separate (backchannels are only detected then).
    pub separate: bool,
}

/// Build the timeline: the conversation, Jev's verdicts (when `assessments` has them), voice
/// features from `audio`, z-scores per speaker, then the temperature.
pub fn build(convo: &[ConvTurn], assessments: &BTreeMap<usize, Assessment>, audio: Option<&Audio>) -> Vec<Signal> {
    let mut signals: Vec<Signal> = convo
        .iter()
        .enumerate()
        .map(|(i, t)| Signal {
            turn_idx: i,
            kind: t.kind,
            speaker: t.speaker.clone(),
            start: t.start,
            end: t.end,
            text: t.text.clone(),
            checks: assessments.get(&i).map(|a| a.verdicts.clone()).unwrap_or_default(),
            voice: None,
            z: BTreeMap::new(),
            backchannel_rate: None,
            latency_s: None,
            video: None,
            temperature: None,
            smoothed: None,
        })
        .collect();
    // Latency, and the answer each substantive turn follows.
    let mut last_answer: Option<usize> = None;
    for i in 0..signals.len() {
        match signals[i].kind {
            Kind::Answer => last_answer = Some(i),
            Kind::Backchannel => {}
            Kind::Substantive => {
                if let Some(a) = last_answer.take() {
                    signals[i].latency_s = Some(signals[i].start - signals[a].end);
                    if let Some(audio) = audio.filter(|a| a.separate) {
                        let (start, end) = (signals[a].start, signals[a].end);
                        let bursts = prosody::voiced_bursts(&audio.interviewer, start, end, 0.15, 1.5).len();
                        let minutes = ((end - start) / 60.0).max(0.25);
                        signals[i].backchannel_rate = Some(bursts as f64 / minutes);
                    }
                }
            }
        }
    }
    if let Some(audio) = audio {
        for s in signals.iter_mut() {
            let frames = if s.kind == Kind::Answer { &audio.you } else { &audio.interviewer };
            if s.kind != Kind::Backchannel {
                s.voice = prosody::span_features(frames, s.start, s.end, word_count(&s.text));
            }
        }
    }
    add_z_scores(&mut signals);
    score(&mut signals);
    signals
}

/// Energy is compared with the same speaker's turns within ±5 minutes (call volume drifts);
/// pitch variation and pace with their whole interview.
fn add_z_scores(signals: &mut [Signal]) {
    for kind in [Kind::Substantive, Kind::Answer] {
        let idx: Vec<usize> = signals.iter().enumerate().filter(|(_, s)| s.kind == kind && s.voice.is_some()).map(|(i, _)| i).collect();
        let voice = |i: usize| signals[i].voice.expect("filtered");
        let local_energy: Vec<f64> = idx
            .iter()
            .map(|&i| {
                let mut near: Vec<f64> = idx
                    .iter()
                    .filter(|&&j| (signals[j].start - signals[i].start).abs() <= 300.0)
                    .map(|&j| voice(j).energy_db)
                    .collect();
                voice(i).energy_db - median(&mut near).unwrap_or(voice(i).energy_db)
            })
            .collect();
        let pitch: Vec<f64> = idx.iter().map(|&i| voice(i).pitch_var_st).collect();
        let pace: Vec<f64> = idx.iter().map(|&i| voice(i).words_per_s).collect();
        let (ez, pz, rz) = (prosody::robust_z(&local_energy), prosody::robust_z(&pitch), prosody::robust_z(&pace));
        for (k, &i) in idx.iter().enumerate() {
            signals[i].z.insert("energy".into(), ez[k]);
            signals[i].z.insert("pitch_var".into(), pz[k]);
            signals[i].z.insert("pace".into(), rz[k]);
        }
    }
    let rated: Vec<usize> = signals.iter().enumerate().filter(|(_, s)| s.backchannel_rate.is_some()).map(|(i, _)| i).collect();
    let rates: Vec<f64> = rated.iter().map(|&i| signals[i].backchannel_rate.unwrap_or(0.0)).collect();
    for (k, z) in prosody::robust_z(&rates).into_iter().enumerate() {
        signals[rated[k]].z.insert("backchannel_rate".into(), z);
    }
}

/// Jev's verdicts for every substantive turn (one request each). Turns it fails on are skipped,
/// and it gives up when the first 3 all fail (the scorer is down; no point waiting on the rest).
pub fn assess_turns(scorer: &dyn Scorer, convo: &[ConvTurn]) -> (BTreeMap<usize, Assessment>, Vec<String>) {
    let set = interviewer_set();
    let mut out = BTreeMap::new();
    let mut errors = vec![];
    for (i, input) in turn_inputs(convo) {
        match scorer.assess(&input, &set, 0) {
            Ok(a) => {
                out.insert(i, a);
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
        if out.is_empty() && errors.len() >= 3 {
            break;
        }
    }
    (out, errors)
}

// --- moments, for the report ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Shift {
    /// The answer just before the shift.
    pub after_answer: usize,
    pub delta: f64,
}

/// The biggest change in the room: the mean of the 2 turns after an answer minus the 2 before it,
/// reported only when it's at least 0.3. The opening greeting isn't counted.
pub fn biggest_shift(signals: &[Signal]) -> Option<Shift> {
    let turns: Vec<(usize, f64)> = after_opening(signals).filter_map(|s| Some((s.turn_idx, s.temperature?))).collect();
    let mut best: Option<Shift> = None;
    for k in 2..turns.len().saturating_sub(1) {
        let before = (turns[k - 2].1 + turns[k - 1].1) / 2.0;
        let after = (turns[k].1 + turns[k + 1].1) / 2.0;
        let delta = after - before;
        // The answer between the two windows.
        let answer = signals
            .iter()
            .filter(|s| s.kind == Kind::Answer && s.turn_idx > turns[k - 1].0 && s.turn_idx < turns[k].0)
            .map(|s| s.turn_idx)
            .next_back();
        if let Some(answer) = answer
            && delta.abs() >= 0.3
            && best.as_ref().is_none_or(|b| delta.abs() > b.delta.abs())
        {
            best = Some(Shift { after_answer: answer, delta });
        }
    }
    best
}

/// What stood out in the room, for the report.
#[derive(Debug, Clone, PartialEq)]
pub struct Moments<'a> {
    /// Up to 3 of the warmest turns (at least `CLEAR`, with a cue), warmest first.
    pub warmest: Vec<&'a Signal>,
    /// Up to 3 of the coolest (at most −`CLEAR`), coolest first.
    pub coolest: Vec<&'a Signal>,
    pub next_steps: Vec<&'a Signal>,
    pub shift: Option<Shift>,
}

/// Only turns with at least one observable cue are quoted (a number alone isn't a moment), and
/// not the opening greeting.
pub fn moments(signals: &[Signal]) -> Moments<'_> {
    let mut scored: Vec<(&Signal, f64)> =
        after_opening(signals).filter(|s| !cues(s).is_empty()).filter_map(|s| Some((s, s.temperature?))).collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.turn_idx.cmp(&b.0.turn_idx)));
    Moments {
        warmest: scored.iter().filter(|(_, t)| *t >= CLEAR).take(3).map(|(s, _)| *s).collect(),
        coolest: scored.iter().rev().filter(|(_, t)| *t <= -CLEAR).take(3).map(|(s, _)| *s).collect(),
        next_steps: signals.iter().filter(|s| flagged(s, "next_steps")).collect(),
        shift: biggest_shift(signals),
    }
}

/// The observable cues behind an interviewer turn's temperature, in words.
pub fn cues(signal: &Signal) -> Vec<&'static str> {
    let mut out = vec![];
    if EVAL_PASSED.contains(&"tone")
        && let Some(tone) = signal.checks.get("tone")
    {
        match tone.pick.as_str() {
            "warm" => out.push("warm tone"),
            "cool" => out.push("cool tone"),
            _ => {}
        }
    }
    for (id, cue) in [("positive_reaction", "reacted well to your answer"), ("builds_on_answer", "followed up on what you said"),
                      ("pushback", "pushed back"), ("selling", "sold the role"), ("next_steps", "talked about next steps")] {
        if flagged(signal, id) {
            out.push(cue);
        }
    }
    let z = |k: &str| signal.z.get(k).copied().unwrap_or(0.0);
    for (k, up, down) in [("energy", "louder than their usual", "quieter than their usual"),
                          ("pitch_var", "more animated than their usual", "flatter than their usual"),
                          ("backchannel_rate", "lots of mm-hmms while you answered", "few mm-hmms while you answered")] {
        if z(k) >= 1.0 {
            out.push(up);
        } else if z(k) <= -1.0 {
            out.push(down);
        }
    }
    if signal.latency_s.is_some_and(|l| l <= -0.5) {
        out.push("came in before you'd finished");
    }
    out
}

/// Notes on how your voice differed from your usual in one answer, e.g. "faster than usual".
pub fn voice_notes(signal: &Signal) -> Vec<&'static str> {
    let z = |k: &str| signal.z.get(k).copied().unwrap_or(0.0);
    let mut notes = vec![];
    if z("pace") >= 1.0 {
        notes.push("faster than your usual");
    } else if z("pace") <= -1.0 {
        notes.push("slower than your usual");
    }
    if z("pitch_var") <= -1.0 {
        notes.push("flatter than your usual");
    } else if z("pitch_var") >= 1.0 {
        notes.push("more animated than your usual");
    }
    if z("energy") <= -1.0 {
        notes.push("quieter than your usual");
    } else if z("energy") >= 1.0 {
        notes.push("louder than your usual");
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::INTERVIEWER;

    fn seg(start: f64, end: f64, speaker: &str, text: &str) -> Segment {
        Segment::new(start, end, text, speaker)
    }

    #[test]
    fn backchannels_are_recognised_and_real_turns_are_not() {
        for b in ["Mm-hmm.", "Right.", "Okay, great.", "Got it.", "Yeah, yeah.", "Sounds good."] {
            assert!(is_backchannel(b), "{b}");
        }
        for t in ["Okay. And why Agility?", "Great, tell me more.", "No.", "Interesting choice there."] {
            assert!(!is_backchannel(t), "{t}");
        }
    }

    /// Dual-track: the interviewer's "mm-hmm"s interleave with your segments and must not split
    /// your answer.
    #[test]
    fn answers_join_across_backchannels() {
        let segs = [
            seg(0.0, 4.0, INTERVIEWER, "Tell me about a project you're proud of."),
            seg(5.0, 15.0, YOU, "I rebuilt our checkout."),
            seg(12.0, 12.5, INTERVIEWER, "Mm-hmm."),
            seg(15.5, 30.0, YOU, "Conversion went from 2.1 to 2.9 percent."),
            seg(22.0, 22.4, INTERVIEWER, "Right."),
            seg(31.5, 35.0, INTERVIEWER, "That's great. How did you test it?"),
            seg(36.0, 50.0, YOU, "We ran an A/B test for four weeks."),
        ];
        let c = conversation(&segs);
        let kinds: Vec<Kind> = c.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [Kind::Substantive, Kind::Answer, Kind::Backchannel, Kind::Backchannel, Kind::Substantive, Kind::Answer]);
        assert_eq!(c[1].text, "I rebuilt our checkout. Conversion went from 2.1 to 2.9 percent.");
        assert_eq!((c[1].start, c[1].end), (5.0, 30.0));
        let inputs = turn_inputs(&c);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1].1.question, "Tell me about a project you're proud of.");
        assert!(inputs[1].1.candidate_said.ends_with("2.9 percent."));
        assert_eq!(inputs[1].1.interviewer_says, "That's great. How did you test it?");
    }

    #[test]
    fn latency_is_signed() {
        let segs = [
            seg(0.0, 4.0, INTERVIEWER, "Tell me about a project you're proud of."),
            seg(5.0, 15.0, YOU, "I rebuilt our checkout, and conversion rose."),
            seg(14.0, 18.0, INTERVIEWER, "Sorry to jump in, which quarter was that?"),
            seg(19.0, 25.0, YOU, "The second quarter, last year."),
            seg(27.5, 30.0, INTERVIEWER, "Got it, thanks. And how did you test it?"),
        ];
        let signals = build(&conversation(&segs), &BTreeMap::new(), None);
        let lat: Vec<Option<f64>> = signals.iter().filter(|s| s.kind == Kind::Substantive).map(|s| s.latency_s).collect();
        assert_eq!(lat, [None, Some(-1.0), Some(2.5)], "they interrupted at 14 s (−1 s), then waited 2.5 s");
    }

    fn verdicts(warm: f64, cool: f64, positive: f64, pushback: f64) -> BTreeMap<String, Verdict> {
        let yn = |p: f64| Verdict { pick: if p >= 0.5 { "yes" } else { "no" }.into(), value: p, confidence: None,
                                    probabilities: BTreeMap::from([("yes".into(), p), ("no".into(), 1.0 - p)]) };
        BTreeMap::from([
            ("tone".into(), Verdict { pick: "warm".into(), value: warm, confidence: None,
                                      probabilities: BTreeMap::from([("warm".into(), warm), ("neutral".into(), 1.0 - warm - cool),
                                                                     ("cool".into(), cool)]) }),
            ("positive_reaction".into(), yn(positive)),
            ("builds_on_answer".into(), yn(0.5)),
            ("pushback".into(), yn(pushback)),
        ])
    }

    fn signal(start: f64, checks: BTreeMap<String, Verdict>) -> Signal {
        Signal { turn_idx: 0, kind: Kind::Substantive, speaker: INTERVIEWER.into(), start, end: start + 5.0, text: String::new(),
                 checks, voice: None, z: BTreeMap::new(), backchannel_rate: None, latency_s: None, video: None,
                 temperature: None, smoothed: None }
    }

    #[test]
    fn warm_praise_scores_warm_and_pushback_scores_cool() {
        let mut s = vec![signal(0.0, verdicts(0.1, 0.1, 0.2, 0.2)), signal(60.0, verdicts(0.9, 0.0, 0.95, 0.05)),
                         signal(120.0, verdicts(0.0, 0.8, 0.05, 0.9)), signal(180.0, verdicts(0.1, 0.1, 0.2, 0.2))];
        score(&mut s);
        let t: Vec<f64> = s.iter().map(|s| s.temperature.unwrap()).collect();
        assert!(t[1] > 0.6, "{t:?}");
        assert!(t[2] < -0.6, "{t:?}");
        assert!(t[0].abs() < 0.2, "a plain turn sits near zero: {t:?}");
        // The line lags the dots: after the warm turn it's warmer than the first turn, but not as warm.
        let line: Vec<f64> = s.iter().map(|s| s.smoothed.unwrap()).collect();
        assert!(line[1] > line[0] && line[1] < t[1], "{line:?}");
    }

    #[test]
    fn markers_and_checks_that_failed_their_comparison_weigh_nothing() {
        let mut checks = verdicts(0.0, 0.0, 0.5, 0.5);
        checks.insert("selling".into(), Verdict { pick: "yes".into(), value: 1.0, confidence: None,
                                                  probabilities: BTreeMap::from([("yes".into(), 1.0)]) });
        let s = signal(0.0, checks);
        assert_eq!(temperature_with(&s, &["tone", "selling"]), 0.0, "selling is a marker, not a weight");
        let praise = signal(0.0, verdicts(0.0, 0.0, 0.95, 0.05));
        let counted = temperature_with(&praise, &["positive_reaction", "builds_on_answer", "pushback"]);
        assert!(counted > 0.3, "{counted}");
        assert_eq!(temperature_with(&praise, &["tone"]), 0.0, "failed checks don't count");
    }

    #[test]
    fn moments_and_cues() {
        // A warm greeting, your answer, then praise, pushback and next steps.
        let mut s = vec![signal(0.0, verdicts(0.9, 0.0, 0.2, 0.2)), signal(10.0, BTreeMap::new()),
                         signal(60.0, verdicts(0.9, 0.0, 0.95, 0.05)), signal(120.0, verdicts(0.0, 0.8, 0.05, 0.9)),
                         signal(180.0, verdicts(0.1, 0.1, 0.2, 0.2))];
        for (i, x) in s.iter_mut().enumerate() {
            x.turn_idx = i;
        }
        s[1].kind = Kind::Answer;
        s[3].checks.get_mut("tone").unwrap().pick = "cool".into();
        s[3].latency_s = Some(-1.0);
        s[4].checks.insert("next_steps".into(), Verdict { pick: "yes".into(), value: 0.9, confidence: None,
                                                          probabilities: BTreeMap::from([("yes".into(), 0.9)]) });
        score(&mut s);
        assert!(s[0].temperature.unwrap() > 0.6, "the greeting reads warm…");
        let m = moments(&s);
        assert_eq!(m.warmest.iter().map(|s| s.turn_idx).collect::<Vec<_>>(), [2], "…but isn't a moment");
        assert_eq!(m.coolest.iter().map(|s| s.turn_idx).collect::<Vec<_>>(), [3]);
        assert_eq!(m.next_steps.iter().map(|s| s.turn_idx).collect::<Vec<_>>(), [4]);
        assert_eq!(cues(&s[2]), ["warm tone", "reacted well to your answer"]);
        assert_eq!(cues(&s[3]), ["cool tone", "pushed back", "came in before you'd finished"]);
    }

    #[test]
    fn the_biggest_shift_needs_a_real_change() {
        let mk = |idx: usize, kind: Kind, temp: Option<f64>| Signal { turn_idx: idx, kind, temperature: temp, ..signal(idx as f64, BTreeMap::new()) };
        let flat: Vec<Signal> = (0..6).map(|i| mk(i * 2, Kind::Substantive, Some(0.1))).collect();
        assert_eq!(biggest_shift(&flat), None);
        let interview = |temps: [f64; 5]| -> Vec<Signal> {
            temps.iter().enumerate().flat_map(|(i, &t)| [mk(i * 2, Kind::Substantive, Some(t)), mk(i * 2 + 1, Kind::Answer, None)]).collect()
        };
        let shift = biggest_shift(&interview([0.6, 0.6, 0.5, -0.2, -0.3])).unwrap();
        assert_eq!(shift.after_answer, 5, "the answer between the warm pair and the cool pair");
        assert!((shift.delta + 0.8).abs() < 1e-9, "{shift:?}");
        assert_eq!(biggest_shift(&interview([0.9, -0.1, -0.1, -0.1, -0.1])), None, "a warm greeting isn't a shift");
    }

    #[test]
    fn your_voice_notes_name_the_clear_differences() {
        let mut s = signal(0.0, BTreeMap::new());
        s.kind = Kind::Answer;
        s.z = BTreeMap::from([("pace".into(), 1.4), ("pitch_var".into(), -1.2), ("energy".into(), 0.3)]);
        assert_eq!(voice_notes(&s), ["faster than your usual", "flatter than your usual"]);
    }
}
