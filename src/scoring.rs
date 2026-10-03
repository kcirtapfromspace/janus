//! Scoring one interview answer against specific checks: the fast, per-answer judgments the
//! feedback loop builds on. Jev and Claude both implement `Scorer`, so they can be compared on the
//! same answers (`ic eval scorers`) and swapped by a setting.
//!
//! Each answer is sent as a small state, `{question, answer, opening}`. `opening` is the answer's
//! first ~40 words, cut here because Jev is weak at counting and at positions in text. Anything
//! countable (fillers, length, "I" vs "we" ratios) is computed in Rust, never asked of a model.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::AnswerCheck;
use crate::llm::jev::{self, Question, SystemOne};
use crate::llm::{self, Effort, GenerateOptions, Llm};
use crate::models::{QuestionReview, Turn, YOU};

/// What a check asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckKind {
    YesNo { yes: String, no: String },
    /// (id, description), in display order.
    Choice { options: Vec<(String, String)> },
    /// Descriptions from lowest to highest; reported as levels 1..=n.
    Level { levels: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: &'static str,
    /// The question, referring to the state's fields in backticks.
    pub instructions: String,
    pub kind: CheckKind,
}

/// One answer to score, with the question it answers.
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerInput {
    pub question: String,
    pub answer: String,
}

pub const OPENING_WORDS: usize = 40;

impl AnswerInput {
    pub fn state(&self) -> Value {
        let opening: Vec<&str> = self.answer.split_whitespace().take(OPENING_WORDS).collect();
        json!({"question": self.question, "answer": self.answer, "opening": opening.join(" ")})
    }
}

/// One check's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// "yes"/"no", the chosen option's id, or the level ("1".."n").
    pub pick: String,
    /// P(yes) for yes/no; the pick's probability for a choice; the fractional level (1..=n) for a level.
    pub value: f64,
    /// Jev's confidence (yes/no: |2p − 1|). Claude reports none.
    pub confidence: Option<f64>,
    /// Probability per option or level, when the scorer gives them.
    pub probabilities: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    /// The model that answered, as it reports itself (e.g. `typesafe/jev-1.13.0`).
    pub scorer: String,
    pub verdicts: BTreeMap<String, Verdict>,
    pub latency_ms: u64,
    pub input_tokens: u64,
}

pub trait Scorer {
    /// The configured name, e.g. `typesafe/jev-latest` or `anthropic/claude-haiku-4-5-20251001`.
    fn name(&self) -> String;
    /// Score one answer. `rotation` shifts every choice's option order (to measure first-option bias).
    fn assess(&self, input: &AnswerInput, checks: &[Check], rotation: usize) -> Result<Assessment>;
}

fn rotated<T: Clone>(items: &[T], by: usize) -> Vec<T> {
    let n = items.len().max(1);
    (0..items.len()).map(|i| items[(i + by) % n].clone()).collect()
}

/// The built-in checks every answer gets. The level descriptions are the session rubric's anchors.
pub fn builtin_checks() -> Vec<Check> {
    vec![
        Check {
            id: "leads_with_point",
            instructions: "Does the `answer` state its main point, or a direct answer to the `question`, at the very start \
                           (see `opening`) before any background?"
                .into(),
            kind: CheckKind::YesNo {
                yes: "The first sentence or two give the main point or direct answer.".into(),
                no: "It opens with background, context or filler and only gets to the point later, or never.".into(),
            },
        },
        Check {
            id: "has_quantified_result",
            instructions: "Does the `answer` give a concrete, quantified result or impact of the candidate's work?".into(),
            kind: CheckKind::YesNo {
                yes: "It states a measurable outcome: a number, percentage, amount, or a measured change such as \
                      'doubled' or 'cut it in half'."
                    .into(),
                no: "Any outcome is vague ('it went well') or missing. Numbers that describe the situation or the plan, \
                     not the result, don't count."
                    .into(),
            },
        },
        Check {
            id: "star_missing",
            instructions: "The `answer` should tell a story: the situation, what the candidate did, and the result. \
                           Which part is missing or too thin?"
                .into(),
            kind: CheckKind::Choice {
                options: vec![
                    ("none".into(), "Nothing: the situation, the candidate's own actions, and the result are all there.".into()),
                    ("situation".into(), "The situation: there's no context for why this mattered or what the problem was.".into()),
                    ("action".into(), "The actions: it doesn't say what the candidate personally did.".into()),
                    ("result".into(), "The result: it stops before saying how things turned out.".into()),
                ],
            },
        },
        Check {
            id: "ownership",
            instructions: "Whose actions does the `answer` describe?".into(),
            kind: CheckKind::Choice {
                options: vec![
                    ("i".into(), "Mostly the candidate's own actions ('I did …').".into()),
                    ("we".into(), "A team's, or nobody's in particular ('we did …', 'it was done'); the candidate's own part \
                                   is unclear."
                        .into()),
                    ("mixed".into(), "The team's work, with the candidate's own part clearly separated out.".into()),
                ],
            },
        },
        Check {
            id: "specificity",
            instructions: "How specific and concrete is the `answer`?".into(),
            kind: CheckKind::Level {
                levels: vec![
                    "Generic statements only: no real example, numbers, or personal ownership.".into(),
                    "A vague example with little detail; the outcome is unclear.".into(),
                    "A real example with some detail, but missing concrete results or the candidate's own role.".into(),
                    "A concrete example with the candidate's own actions and a clear outcome.".into(),
                    "A concrete, well-chosen example with the candidate's own actions and measured, quantified impact.".into(),
                ],
            },
        },
        Check {
            id: "structure",
            instructions: "How well structured is the `answer`?".into(),
            kind: CheckKind::Level {
                levels: vec![
                    "No discernible structure: it rambles or doesn't answer the question.".into(),
                    "Hard to follow; the point is buried or missing.".into(),
                    "Answers the question but meanders; some order.".into(),
                    "Clear order (situation, action, result) and the point is easy to find.".into(),
                    "Leads with the point, then a crisp situation, action and result with nothing extra.".into(),
                ],
            },
        },
    ]
}

// --- answers in a real interview ------------------------------------------------------------------

/// An answer shorter than this isn't worth checking ("Sure", "Thanks").
pub const MIN_ANSWER_WORDS: usize = 8;

/// One of the candidate's answers in an interview.
#[derive(Debug, Clone, PartialEq)]
pub struct InterviewAnswer {
    pub idx: usize,
    /// When the question was asked, in seconds.
    pub start: f64,
    pub input: AnswerInput,
}

fn seconds(timestamp: &str) -> Option<f64> {
    let parts: Vec<f64> = timestamp.split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p))
}

/// The candidate's answer to each question in the report: their turns from that question's timestamp
/// up to the next question's. Answers that are missing (e.g. a mic that stopped) or too short are skipped.
pub fn answers_from_report(turns: &[Turn], questions: &[QuestionReview]) -> Vec<InterviewAnswer> {
    let mut asked: Vec<(f64, &QuestionReview)> = questions.iter().filter_map(|q| Some((seconds(&q.timestamp)?, q))).collect();
    asked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = vec![];
    for (i, (start, q)) in asked.iter().enumerate() {
        let end = asked.get(i + 1).map_or(f64::INFINITY, |next| next.0);
        let text: Vec<&str> = turns
            .iter()
            .filter(|t| t.speaker == YOU && t.start >= *start - 1.0 && t.start < end)
            .map(|t| t.text.trim())
            .collect();
        let answer = text.join(" ");
        if answer.split_whitespace().count() >= MIN_ANSWER_WORDS {
            out.push(InterviewAnswer { idx: out.len(), start: *start,
                                       input: AnswerInput { question: q.question.clone(), answer } });
        }
    }
    out
}

/// Pass, fail, or unclear for one check's verdict. "Pass" is the coaching ideal: leads with the
/// point, gives a number, a complete story, the candidate's own part clear, and levels 4–5.
pub fn classify(check_id: &str, v: &Verdict) -> &'static str {
    let sure = |p: Option<f64>| p.is_none_or(|p| p >= 0.5);
    match check_id {
        "leads_with_point" | "has_quantified_result" => match v.value {
            p if p >= 0.7 => "pass",
            p if p <= 0.3 => "fail",
            _ => "unclear",
        },
        "star_missing" | "ownership" => {
            let good = if check_id == "star_missing" { v.pick == "none" } else { v.pick != "we" };
            match (good, sure(v.probabilities.get(&v.pick).copied())) {
                (_, false) => "unclear",
                (true, true) => "pass",
                (false, true) => "fail",
            }
        }
        _ => match v.value {
            l if l >= 3.5 => "pass",
            l if l <= 2.5 => "fail",
            _ => "unclear",
        },
    }
}

/// Run the built-in checks on every answer, as rows to store with the analysis.
pub fn check_answers(scorer: &dyn Scorer, answers: &[InterviewAnswer]) -> Result<Vec<AnswerCheck>> {
    let checks = builtin_checks();
    let mut rows = vec![];
    for a in answers {
        let assessment = scorer.assess(&a.input, &checks, 0)?;
        for c in &checks {
            let Some(v) = assessment.verdicts.get(c.id) else { continue };
            rows.push(AnswerCheck {
                answer_idx: a.idx as i64,
                answer_start: a.start,
                question: a.input.question.clone(),
                check_id: c.id.to_string(),
                scorer: assessment.scorer.clone(),
                pick: v.pick.clone(),
                value: v.value,
                confidence: v.confidence,
                verdict: classify(c.id, v).to_string(),
            });
        }
    }
    Ok(rows)
}

// --- Jev ---------------------------------------------------------------------------------------

pub struct JevScorer<'a> {
    pub client: &'a dyn SystemOne,
    pub model: String,
}

impl Scorer for JevScorer<'_> {
    fn name(&self) -> String {
        format!("typesafe/{}", self.model)
    }

    fn assess(&self, input: &AnswerInput, checks: &[Check], rotation: usize) -> Result<Assessment> {
        let questions = checks
            .iter()
            .map(|c| {
                let q = match &c.kind {
                    CheckKind::YesNo { yes, no } => Question::Noul { instructions: c.instructions.clone(),
                                                                     yes: Some(yes.clone()), no: Some(no.clone()) },
                    CheckKind::Choice { options } => Question::Choice { instructions: c.instructions.clone(),
                                                                        options: rotated(options, rotation) },
                    CheckKind::Level { levels } => Question::Score { instructions: c.instructions.clone(), levels: levels.clone() },
                };
                (c.id.to_string(), q)
            })
            .collect();
        let req = jev::Request { state: input.state(), model: self.model.clone(), questions };
        let resp = self.client.ask(&req)?;
        let mut verdicts = BTreeMap::new();
        for c in checks {
            let answer = resp.answers.get(c.id).with_context(|| format!("Jev didn't answer {}", c.id))?;
            let verdict = match answer {
                jev::Answer::Noul { p_yes } => Verdict {
                    pick: if *p_yes >= 0.5 { "yes" } else { "no" }.into(),
                    value: *p_yes,
                    confidence: Some((2.0 * p_yes - 1.0).abs()),
                    probabilities: BTreeMap::from([("yes".into(), *p_yes), ("no".into(), 1.0 - p_yes)]),
                },
                jev::Answer::Choice { pick, probabilities, confidence } => Verdict {
                    pick: pick.clone(),
                    value: probabilities.get(pick).copied().unwrap_or(0.0),
                    confidence: Some(*confidence),
                    probabilities: probabilities.clone(),
                },
                jev::Answer::Score { score, level, probabilities, confidence } => Verdict {
                    pick: (level + 1).to_string(),
                    value: score + 1.0,
                    confidence: Some(*confidence),
                    probabilities: probabilities.iter().enumerate().map(|(i, p)| ((i + 1).to_string(), *p)).collect(),
                },
            };
            verdicts.insert(c.id.to_string(), verdict);
        }
        let scorer = if resp.model.contains('/') { resp.model.clone() } else { format!("typesafe/{}", resp.model) };
        Ok(Assessment { scorer, verdicts, latency_ms: resp.latency_ms, input_tokens: resp.input_tokens })
    }
}

// --- Claude --------------------------------------------------------------------------------------

pub const CLAUDE_PROMPT: &str = include_str!("../prompts/scorer_v1.md");

pub struct ClaudeScorer<'a> {
    pub llm: &'a dyn Llm,
    pub model: String,
    pub effort: Effort,
}

/// The JSON schema for a set of checks: yes/no as {yes, p_yes}, a choice as {pick}, a level as {level}.
pub fn claude_schema(checks: &[Check], rotation: usize) -> Value {
    let mut properties = serde_json::Map::new();
    for c in checks {
        let schema = match &c.kind {
            CheckKind::YesNo { .. } => json!({"type": "object", "additionalProperties": false, "required": ["yes", "p_yes"],
                "properties": {"yes": {"type": "boolean"},
                               "p_yes": {"type": "number", "description": "Your probability that the answer is yes, 0 to 1."}}}),
            CheckKind::Choice { options } => {
                let ids: Vec<String> = rotated(options, rotation).into_iter().map(|(id, _)| id).collect();
                json!({"type": "object", "additionalProperties": false, "required": ["pick"],
                       "properties": {"pick": {"type": "string", "enum": ids}}})
            }
            CheckKind::Level { levels } => json!({"type": "object", "additionalProperties": false, "required": ["level"],
                "properties": {"level": {"type": "integer", "enum": (1..=levels.len()).collect::<Vec<_>>()}}}),
        };
        properties.insert(c.id.to_string(), schema);
    }
    let required: Vec<&str> = checks.iter().map(|c| c.id).collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

/// The checks written out for Claude, in the same order and wording Jev gets.
pub fn claude_message(input: &AnswerInput, checks: &[Check], rotation: usize) -> String {
    let mut out = format!("<question>\n{}\n</question>\n\n<answer>\n{}\n</answer>\n\n<checks>\n", input.question, input.answer);
    for c in checks {
        out.push_str(&format!("\n{}: {}\n", c.id, c.instructions.replace('`', "")));
        match &c.kind {
            CheckKind::YesNo { yes, no } => out.push_str(&format!("  yes: {yes}\n  no: {no}\n")),
            CheckKind::Choice { options } => {
                for (id, description) in rotated(options, rotation) {
                    out.push_str(&format!("  {id}: {description}\n"));
                }
            }
            CheckKind::Level { levels } => {
                for (i, description) in levels.iter().enumerate() {
                    out.push_str(&format!("  {}: {description}\n", i + 1));
                }
            }
        }
    }
    out.push_str("</checks>");
    out
}

impl Scorer for ClaudeScorer<'_> {
    fn name(&self) -> String {
        format!("anthropic/{}", self.model)
    }

    fn assess(&self, input: &AnswerInput, checks: &[Check], rotation: usize) -> Result<Assessment> {
        let schema = claude_schema(checks, rotation);
        let started = Instant::now();
        let value = llm::structured_value(self.llm, &self.model, CLAUDE_PROMPT, &claude_message(input, checks, rotation),
                                          &schema, "answer_checks", GenerateOptions { effort: self.effort, max_tokens: 4_000 })?;
        let latency_ms = started.elapsed().as_millis() as u64;
        let mut verdicts = BTreeMap::new();
        for c in checks {
            let v = &value[c.id];
            let verdict = match &c.kind {
                CheckKind::YesNo { .. } => {
                    let yes = v["yes"].as_bool().with_context(|| format!("{}: no yes/no", c.id))?;
                    let p = v["p_yes"].as_f64().map(|p| p.clamp(0.0, 1.0)).unwrap_or(if yes { 1.0 } else { 0.0 });
                    Verdict { pick: if yes { "yes" } else { "no" }.into(), value: p, confidence: None,
                              probabilities: BTreeMap::from([("yes".into(), p), ("no".into(), 1.0 - p)]) }
                }
                CheckKind::Choice { options } => {
                    let pick = v["pick"].as_str().with_context(|| format!("{}: no pick", c.id))?.to_string();
                    if !options.iter().any(|(id, _)| *id == pick) {
                        bail!("{}: Claude picked an unknown option {pick}", c.id);
                    }
                    Verdict { pick, value: 1.0, confidence: None, probabilities: BTreeMap::new() }
                }
                CheckKind::Level { levels } => {
                    let level = v["level"].as_u64().filter(|l| (1..=levels.len() as u64).contains(l))
                        .with_context(|| format!("{}: level out of range", c.id))?;
                    Verdict { pick: level.to_string(), value: level as f64, confidence: None, probabilities: BTreeMap::new() }
                }
            };
            verdicts.insert(c.id.to_string(), verdict);
        }
        Ok(Assessment { scorer: self.name(), verdicts, latency_ms, input_tokens: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmError;
    use std::cell::RefCell;

    fn input() -> AnswerInput {
        AnswerInput { question: "Tell me about a time you failed.".into(),
                      answer: format!("The clearest failure was a pricing change. {}", "word ".repeat(60)) }
    }

    fn turn(speaker: &str, start: f64, text: &str) -> Turn {
        Turn { speaker: speaker.into(), start, end: start + 5.0, text: text.into() }
    }

    fn review(ts: &str, question: &str) -> QuestionReview {
        serde_json::from_value(json!({"timestamp": ts, "question": question, "type": "behavioral", "answer_summary": "",
                                      "score": 3, "what_worked": "", "what_was_missing": "", "stronger_answer": ""})).unwrap()
    }

    #[test]
    fn answers_are_the_candidates_turns_between_questions() {
        let turns = [
            turn("interviewer", 10.0, "Tell me about a time you failed."),
            turn("you", 14.0, "The clearest failure was a pricing change that cut sign-ups by eighteen percent."),
            turn("you", 30.0, "I rolled it back within two days and wrote it up."),
            turn("interviewer", 60.0, "Why us?"),
            turn("you", 63.0, "Sure."),
            turn("interviewer", 80.0, "How do you prioritise?"),
            turn("you", 84.0, "I size each request by revenue at risk and effort, then fix the biggest first."),
        ];
        let qs = [review("00:01:20", "How do you prioritise?"), review("00:00:10", "Tell me about a time you failed."),
                  review("00:01:00", "Why us?")];
        let answers = answers_from_report(&turns, &qs);
        assert_eq!(answers.len(), 2, "'Sure.' is too short to check");
        assert_eq!(answers[0].input.question, "Tell me about a time you failed.");
        assert!(answers[0].input.answer.ends_with("wrote it up."));
        assert_eq!((answers[1].idx, answers[1].start), (1, 80.0));
    }

    #[test]
    fn verdicts_are_classified_with_an_unclear_band() {
        let yes = |p: f64| Verdict { pick: if p >= 0.5 { "yes" } else { "no" }.into(), value: p, confidence: None,
                                     probabilities: BTreeMap::from([("yes".into(), p), ("no".into(), 1.0 - p)]) };
        assert_eq!(classify("leads_with_point", &yes(0.9)), "pass");
        assert_eq!(classify("leads_with_point", &yes(0.5)), "unclear");
        assert_eq!(classify("has_quantified_result", &yes(0.1)), "fail");
        let pick = |p: &str, prob: f64| Verdict { pick: p.into(), value: prob, confidence: None,
                                                  probabilities: BTreeMap::from([(p.to_string(), prob)]) };
        assert_eq!(classify("star_missing", &pick("none", 0.8)), "pass");
        assert_eq!(classify("star_missing", &pick("result", 0.8)), "fail");
        assert_eq!(classify("ownership", &pick("mixed", 0.9)), "pass");
        assert_eq!(classify("ownership", &pick("we", 0.4)), "unclear");
        let level = |l: f64| Verdict { pick: format!("{}", l.round()), value: l, confidence: None, probabilities: BTreeMap::new() };
        assert_eq!(classify("specificity", &level(4.2)), "pass");
        assert_eq!(classify("structure", &level(3.0)), "unclear");
        assert_eq!(classify("structure", &level(1.6)), "fail");
    }

    #[test]
    fn the_state_carries_a_word_limited_opening() {
        let state = input().state();
        assert_eq!(state["opening"].as_str().unwrap().split_whitespace().count(), OPENING_WORDS);
        assert!(state["opening"].as_str().unwrap().starts_with("The clearest failure"));
        assert_eq!(state["question"], "Tell me about a time you failed.");
    }

    struct FakeJev(RefCell<Vec<jev::Request>>);

    impl SystemOne for FakeJev {
        fn ask(&self, req: &jev::Request) -> Result<jev::Response, LlmError> {
            self.0.borrow_mut().push(req.clone());
            let mut answers = BTreeMap::new();
            for (id, q) in &req.questions {
                let a = match q {
                    Question::Noul { .. } => jev::Answer::Noul { p_yes: 0.8 },
                    Question::Choice { options, .. } => jev::Answer::Choice {
                        pick: options[0].0.clone(),
                        probabilities: options.iter().enumerate().map(|(i, (o, _))| (o.clone(), if i == 0 { 0.7 } else { 0.1 })).collect(),
                        confidence: 0.55,
                    },
                    Question::Score { levels, .. } => jev::Answer::Score {
                        score: 3.2, level: 3, probabilities: (0..levels.len()).map(|i| if i == 3 { 0.8 } else { 0.05 }).collect(),
                        confidence: 0.6,
                    },
                };
                answers.insert(id.clone(), a);
            }
            Ok(jev::Response { model: "jev-1.13.0".into(), answers, input_tokens: 400, latency_ms: 90 })
        }
    }

    #[test]
    fn jev_verdicts_use_rubric_levels_and_rotation_reorders_choices() {
        let fake = FakeJev(RefCell::new(vec![]));
        let scorer = JevScorer { client: &fake, model: "jev-latest".into() };
        let a = scorer.assess(&input(), &builtin_checks(), 0).unwrap();
        assert_eq!(a.scorer, "typesafe/jev-1.13.0");
        assert_eq!(a.verdicts["leads_with_point"].pick, "yes");
        assert!((a.verdicts["leads_with_point"].confidence.unwrap() - 0.6).abs() < 1e-9);
        assert_eq!(a.verdicts["star_missing"].pick, "none", "unrotated: the first option is 'none'");
        assert_eq!((a.verdicts["specificity"].pick.as_str(), a.verdicts["specificity"].value), ("4", 4.2), "levels report as 1..=5");
        let rotated = scorer.assess(&input(), &builtin_checks(), 1).unwrap();
        assert_eq!(rotated.verdicts["star_missing"].pick, "situation", "rotated by one, 'situation' is shown first");
        let sent = fake.0.borrow();
        assert!(matches!(&sent[1].questions[2].1, Question::Choice { options, .. } if options[0].0 == "situation"));
    }

    #[test]
    fn claude_gets_the_same_checks_and_a_strict_schema() {
        let checks = builtin_checks();
        let schema = claude_schema(&checks, 1);
        assert_eq!(schema["required"].as_array().unwrap().len(), checks.len());
        assert_eq!(schema["properties"]["ownership"]["properties"]["pick"]["enum"], json!(["we", "mixed", "i"]));
        assert_eq!(schema["properties"]["specificity"]["properties"]["level"]["enum"], json!([1, 2, 3, 4, 5]));
        let message = claude_message(&input(), &checks, 0);
        assert!(message.contains("leads_with_point: Does the answer state its main point"));
        assert!(message.contains("  5: A concrete, well-chosen example"));
    }

    struct FakeClaude(String);

    impl Llm for FakeClaude {
        fn structured(&self, _: &llm::StructuredRequest, _: &mut dyn FnMut(usize)) -> Result<String, LlmError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn claude_verdicts_are_parsed_and_checked() {
        let reply = json!({"leads_with_point": {"yes": false, "p_yes": 0.2}, "has_quantified_result": {"yes": true, "p_yes": 0.9},
                           "star_missing": {"pick": "result"}, "ownership": {"pick": "we"},
                           "specificity": {"level": 3}, "structure": {"level": 2}});
        let fake = FakeClaude(reply.to_string());
        let scorer = ClaudeScorer { llm: &fake, model: "claude-haiku-4-5-20251001".into(), effort: Effort::Low };
        let a = scorer.assess(&input(), &builtin_checks(), 0).unwrap();
        assert_eq!(a.verdicts["leads_with_point"].pick, "no");
        assert_eq!(a.verdicts["star_missing"].pick, "result");
        assert_eq!(a.verdicts["structure"].value, 2.0);
        let bad = FakeClaude(json!({"leads_with_point": {"yes": true, "p_yes": 1}, "has_quantified_result": {"yes": true, "p_yes": 1},
                                    "star_missing": {"pick": "everything"}, "ownership": {"pick": "i"},
                                    "specificity": {"level": 3}, "structure": {"level": 2}}).to_string());
        let scorer = ClaudeScorer { llm: &bad, model: "m".into(), effort: Effort::Low };
        assert!(scorer.assess(&input(), &builtin_checks(), 0).unwrap_err().to_string().contains("unknown option"));
    }
}
