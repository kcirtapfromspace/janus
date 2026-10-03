//! TypeSafe's Jev, through LiteLLM's `/typesafe/` pass-through.
//!
//! Jev doesn't write text. It answers typed questions about a piece of text (the "state"), and
//! every answer is one of the options it was given, with a probability for each:
//! - noul: a yes/no question, answered with the probability of yes (0–1);
//! - choice: pick one of up to 255 options; returns the pick, each option's probability, and a
//!   confidence (how concentrated the probabilities are);
//! - score: place the state on 2–10 described levels, numbered from 0; returns a fractional score
//!   (each level times its probability), each level's probability, and a confidence.
//!
//! The proxy swaps ic's virtual key for `TYPESAFE_API_KEY`, so the key never leaves the proxy.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{LlmError, error_from_response, with_retries};
use crate::proxy::LlmEndpoint;

pub const PATH: &str = "/typesafe/v1/systemone";
pub const DEFAULT_MODEL: &str = "jev-latest";
/// Jev 1.13's limit for the state plus the longest question, in tokens.
const MAX_STATE_TOKENS: usize = 32_000;
const MAX_CHOICES: usize = 255;

/// One question, as Jev's API takes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    Noul {
        instructions: String,
        yes: Option<String>,
        no: Option<String>,
    },
    /// Options in the order they're shown. Jev leans toward the first one, so callers can rotate.
    Choice {
        instructions: String,
        options: Vec<(String, String)>,
    },
    /// Levels from lowest to highest.
    Score {
        instructions: String,
        levels: Vec<String>,
    },
}

impl Question {
    pub fn instructions(&self) -> &str {
        match self {
            Question::Noul { instructions, .. }
            | Question::Choice { instructions, .. }
            | Question::Score { instructions, .. } => instructions,
        }
    }

    /// The API's JSON. Choice options get keys `o00`, `o01`, … so their order survives JSON object
    /// ordering (Jev never sees the keys, only the descriptions).
    fn to_json(&self) -> Value {
        match self {
            Question::Noul {
                instructions,
                yes,
                no,
            } => {
                let mut q = json!({"type": "noul", "instructions": instructions});
                if let (Some(yes), Some(no)) = (yes, no) {
                    q["criteria"] = json!({"true": yes, "false": no});
                }
                q
            }
            Question::Choice {
                instructions,
                options,
            } => {
                let criteria: serde_json::Map<String, Value> = options
                    .iter()
                    .enumerate()
                    .map(|(i, (_, description))| (choice_key(i), json!(description)))
                    .collect();
                json!({"type": "choice", "instructions": instructions, "criteria": criteria})
            }
            Question::Score {
                instructions,
                levels,
            } => json!({"type": "score", "instructions": instructions, "criteria": levels}),
        }
    }
}

fn choice_key(index: usize) -> String {
    format!("o{index:02}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// What the questions are about. Instructions can refer to its fields in backticks.
    pub state: Value,
    pub model: String,
    /// Question id → question.
    pub questions: Vec<(String, Question)>,
}

impl Request {
    pub fn to_json(&self) -> Value {
        let questions: serde_json::Map<String, Value> = self
            .questions
            .iter()
            .map(|(id, q)| (id.clone(), q.to_json()))
            .collect();
        json!({"state": self.state, "model": self.model, "questions": questions})
    }

    /// Jev's documented limits, checked before sending. Tokens are estimated at 3.5 characters each.
    pub fn validate(&self) -> Result<(), String> {
        let state_chars = self.state.to_string().len();
        let longest = self
            .questions
            .iter()
            .map(|(_, q)| q.to_json().to_string().len())
            .max()
            .unwrap_or(0);
        if (state_chars + longest) * 10 / 35 > MAX_STATE_TOKENS {
            return Err(format!(
                "the state is too long for Jev (about {} tokens; the limit is {MAX_STATE_TOKENS})",
                (state_chars + longest) * 10 / 35
            ));
        }
        for (id, q) in &self.questions {
            match q {
                Question::Choice { options, .. }
                    if options.len() < 2 || options.len() > MAX_CHOICES =>
                {
                    return Err(format!("{id}: a choice needs 2 to {MAX_CHOICES} options"));
                }
                Question::Score { levels, .. } if !(2..=10).contains(&levels.len()) => {
                    return Err(format!("{id}: a score needs 2 to 10 levels"));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// One answer, typed and checked against its question.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// Probability of yes.
    Noul { p_yes: f64 },
    /// The picked option's id and each option's probability (by option id).
    Choice {
        pick: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// Fractional score over levels 0..n-1, the most likely level, and each level's probability.
    Score {
        score: f64,
        level: usize,
        probabilities: Vec<f64>,
        confidence: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Response {
    /// The model that answered, e.g. `jev-1.13.0`.
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    pub input_tokens: u64,
    pub latency_ms: u64,
}

#[derive(Deserialize)]
struct RawResponse {
    model: String,
    answers: BTreeMap<String, Value>,
    #[serde(default)]
    usage: Value,
}

/// Parse Jev's reply and check every answer fits its question: right type, known options,
/// probabilities that add up, a score within its levels.
pub fn parse_response(req: &Request, body: &str, latency_ms: u64) -> Result<Response, String> {
    let raw: RawResponse = serde_json::from_str(body)
        .map_err(|e| format!("Jev's reply wasn't the expected JSON: {e}"))?;
    let mut answers = BTreeMap::new();
    for (id, question) in &req.questions {
        let a = raw
            .answers
            .get(id)
            .ok_or_else(|| format!("Jev didn't answer {id}"))?;
        let probs = |v: &Value| -> Result<BTreeMap<String, f64>, String> {
            v.as_object()
                .ok_or_else(|| format!("{id}: no probabilities"))?
                .iter()
                .map(|(k, p)| {
                    p.as_f64()
                        .map(|p| (k.clone(), p))
                        .ok_or_else(|| format!("{id}: probability isn't a number"))
                })
                .collect()
        };
        let sums_to_one = |p: &BTreeMap<String, f64>| (p.values().sum::<f64>() - 1.0).abs() <= 0.02;
        let answer = match (question, a["type"].as_str()) {
            (Question::Noul { .. }, Some("noul")) => {
                let p = a["noul"]
                    .as_f64()
                    .filter(|p| (0.0..=1.0).contains(p))
                    .ok_or_else(|| format!("{id}: noul out of range"))?;
                Answer::Noul { p_yes: p }
            }
            (Question::Choice { options, .. }, Some("choice")) => {
                let by_key = probs(&a["probabilities"])?;
                if !sums_to_one(&by_key) {
                    return Err(format!("{id}: choice probabilities don't add up to 1"));
                }
                let mut probabilities = BTreeMap::new();
                for (i, (option_id, _)) in options.iter().enumerate() {
                    let p = by_key
                        .get(&choice_key(i))
                        .ok_or_else(|| format!("{id}: no probability for {option_id}"))?;
                    probabilities.insert(option_id.clone(), *p);
                }
                let picked_key = a["choice"]
                    .as_str()
                    .ok_or_else(|| format!("{id}: no choice"))?;
                let pick = options
                    .iter()
                    .enumerate()
                    .find(|(i, _)| choice_key(*i) == picked_key)
                    .map(|(_, (option_id, _))| option_id.clone())
                    .ok_or_else(|| format!("{id}: Jev picked an unknown option {picked_key}"))?;
                Answer::Choice {
                    pick,
                    probabilities,
                    confidence: a["confidence"].as_f64().unwrap_or(0.0),
                }
            }
            (Question::Score { levels, .. }, Some("score")) => {
                let by_level = probs(&a["probabilities"])?;
                if !sums_to_one(&by_level) {
                    return Err(format!("{id}: score probabilities don't add up to 1"));
                }
                let probabilities: Vec<f64> = (0..levels.len())
                    .map(|i| by_level.get(&i.to_string()).copied().unwrap_or(0.0))
                    .collect();
                let score = a["score"]
                    .as_f64()
                    .ok_or_else(|| format!("{id}: no score"))?;
                if score < -0.01 || score > (levels.len() - 1) as f64 + 0.01 {
                    return Err(format!("{id}: score {score} is outside its levels"));
                }
                let level = probabilities
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                Answer::Score {
                    score,
                    level,
                    probabilities,
                    confidence: a["confidence"].as_f64().unwrap_or(0.0),
                }
            }
            (_, other) => {
                return Err(format!(
                    "{id}: expected a {} answer, got {other:?}",
                    kind(question)
                ));
            }
        };
        answers.insert(id.clone(), answer);
    }
    Ok(Response {
        model: raw.model,
        answers,
        input_tokens: raw.usage["input_tokens"].as_u64().unwrap_or(0),
        latency_ms,
    })
}

fn kind(q: &Question) -> &'static str {
    match q {
        Question::Noul { .. } => "noul",
        Question::Choice { .. } => "choice",
        Question::Score { .. } => "score",
    }
}

/// Anything that answers Jev requests: the real client, or a fake in tests.
pub trait SystemOne {
    fn ask(&self, req: &Request) -> Result<Response, LlmError>;
}

pub struct Client {
    http: reqwest::blocking::Client,
    endpoint: LlmEndpoint,
    direct: bool,
}

impl Client {
    pub fn new(endpoint: LlmEndpoint) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("building HTTP client");
        Client {
            http,
            endpoint,
            direct: false,
        }
    }
    pub fn direct(api_key: String) -> Self {
        let mut client = Self::new(LlmEndpoint {
            base_url: "https://api.typesafe.ai".into(),
            api_key,
        });
        client.direct = true;
        client
    }

    pub fn configured(settings: &crate::config::Settings) -> anyhow::Result<Self> {
        use anyhow::Context;
        if crate::proxy::using_external_proxy() {
            Ok(Self::new(
                LlmEndpoint::load(settings).context("IC_LLM_URL requires IC_LLM_KEY")?,
            ))
        } else {
            Ok(Self::direct(crate::jev_auth::key(settings)?.context("Jev evaluation requires a TypeSafe key. Add it in Setup (or: ic proxy key typesafe).")?))
        }
    }
}

impl SystemOne for Client {
    fn ask(&self, req: &Request) -> Result<Response, LlmError> {
        req.validate().map_err(LlmError::Protocol)?;
        let body = req.to_json().to_string();
        let path = if self.direct { "/v1/systemone" } else { PATH };
        let url = format!("{}{path}", self.endpoint.base_url.trim_end_matches('/'));
        with_retries(|| {
            let started = Instant::now();
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.endpoint.api_key)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .map_err(|e| LlmError::Network(e.to_string()))?;
            if resp.status().as_u16() != 200 {
                return Err(error_from_response(resp));
            }
            let text = resp.text().map_err(|e| LlmError::Network(e.to_string()))?;
            parse_response(req, &text, started.elapsed().as_millis() as u64)
                .map_err(LlmError::Protocol)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            state: json!({"question": "Why us?", "answer": "I use your product daily."}),
            model: DEFAULT_MODEL.into(),
            questions: vec![
                (
                    "leads".into(),
                    Question::Noul {
                        instructions: "Does `answer` lead with its point?".into(),
                        yes: Some("States it first".into()),
                        no: Some("Builds up to it".into()),
                    },
                ),
                (
                    "owner".into(),
                    Question::Choice {
                        instructions: "Whose actions?".into(),
                        options: vec![
                            ("i".into(), "The candidate's own".into()),
                            ("we".into(), "The team's".into()),
                        ],
                    },
                ),
                (
                    "specific".into(),
                    Question::Score {
                        instructions: "How specific?".into(),
                        levels: vec!["Vague".into(), "Some detail".into(), "Concrete".into()],
                    },
                ),
            ],
        }
    }

    #[test]
    fn requests_use_jevs_shapes_and_keep_choice_order() {
        let body = request().to_json();
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(
            body["questions"]["leads"],
            json!({"type": "noul", "instructions": "Does `answer` lead with its point?",
                                                      "criteria": {"true": "States it first", "false": "Builds up to it"}})
        );
        assert_eq!(
            body["questions"]["owner"]["criteria"],
            json!({"o00": "The candidate's own", "o01": "The team's"})
        );
        assert_eq!(
            body["questions"]["specific"]["criteria"],
            json!(["Vague", "Some detail", "Concrete"])
        );
    }

    /// Shapes from TypeSafe's docs (noul 0.95; choice with probabilities by key; score 1.43).
    #[test]
    fn responses_are_typed_and_mapped_back_to_option_ids() {
        let body = json!({
            "model": "jev-1.13.0",
            "answers": {
                "leads": {"type": "noul", "noul": 0.95},
                "owner": {"type": "choice", "choice": "o01", "confidence": 0.6, "probabilities": {"o00": 0.2, "o01": 0.8}},
                "specific": {"type": "score", "score": 1.43, "confidence": 0.35,
                             "legend": {"0": "Vague", "1": "Some detail", "2": "Concrete"},
                             "probabilities": {"0": 0.0, "1": 0.57, "2": 0.43}},
            },
            "usage": {"input_tokens": 296, "output_tokens": 20}
        });
        let r = parse_response(&request(), &body.to_string(), 120).unwrap();
        assert_eq!(r.model, "jev-1.13.0");
        assert_eq!(r.answers["leads"], Answer::Noul { p_yes: 0.95 });
        assert_eq!(
            r.answers["owner"],
            Answer::Choice {
                pick: "we".into(),
                probabilities: BTreeMap::from([("i".into(), 0.2), ("we".into(), 0.8)]),
                confidence: 0.6
            }
        );
        assert_eq!(
            r.answers["specific"],
            Answer::Score {
                score: 1.43,
                level: 1,
                probabilities: vec![0.0, 0.57, 0.43],
                confidence: 0.35
            }
        );
        assert_eq!(r.input_tokens, 296);
    }

    #[test]
    fn answers_that_dont_fit_their_question_are_rejected() {
        let bad = |answers: Value| {
            parse_response(
                &request(),
                &json!({"model": "jev", "answers": answers}).to_string(),
                0,
            )
            .unwrap_err()
        };
        let ok_owner = json!({"type": "choice", "choice": "o00", "confidence": 1.0, "probabilities": {"o00": 1.0, "o01": 0.0}});
        let ok_specific = json!({"type": "score", "score": 1.0, "confidence": 1.0, "probabilities": {"0": 0.0, "1": 1.0, "2": 0.0}});
        assert!(
            bad(json!({"owner": ok_owner, "specific": ok_specific}))
                .contains("didn't answer leads")
        );
        assert!(bad(json!({"leads": {"type": "noul", "noul": 1.7}, "owner": ok_owner, "specific": ok_specific})).contains("out of range"));
        assert!(bad(json!({"leads": {"type": "noul", "noul": 0.5}, "specific": ok_specific,
                           "owner": {"type": "choice", "choice": "o07", "confidence": 1.0, "probabilities": {"o00": 0.5, "o01": 0.5}}}))
            .contains("unknown option"));
        assert!(bad(json!({"leads": {"type": "noul", "noul": 0.5}, "owner": ok_owner,
                           "specific": {"type": "score", "score": 1.0, "confidence": 1.0, "probabilities": {"0": 0.9, "1": 0.9}}}))
            .contains("don't add up"));
        assert!(
            bad(json!({"leads": {"type": "choice"}, "owner": ok_owner, "specific": ok_specific}))
                .contains("expected a noul")
        );
    }

    #[test]
    fn limits_are_checked_before_sending() {
        let mut r = request();
        assert!(r.validate().is_ok());
        r.state = json!({"answer": "word ".repeat(30_000)});
        assert!(r.validate().unwrap_err().contains("too long"));
        let mut r = request();
        r.questions[2].1 = Question::Score {
            instructions: "x".into(),
            levels: vec!["only one".into()],
        };
        assert!(r.validate().unwrap_err().contains("2 to 10 levels"));
    }
}
