//! A fake model and sample data shared by the integration tests.

use std::cell::RefCell;

use interview_coach::config::{ModelRef, Settings};
use interview_coach::db::{Db, NewSession};
use interview_coach::llm::{Llm, LlmError, StructuredRequest};
use interview_coach::models::{INTERVIEWER, Mode, Segment, Source, Status, YOU};
use serde_json::{Value, json};

pub fn segments() -> Vec<Segment> {
    vec![
        Segment::new(0.0, 4.0, "Tell me about a tough prioritization call.", INTERVIEWER),
        Segment::new(5.0, 20.0, "Um, so, like, we shipped the onboarding redesign in eight weeks.", YOU),
        Segment::new(21.0, 30.0, "Activation went from sixty to seventy eight percent. What does success look like?", YOU),
        Segment::new(31.0, 36.0, "Great. Our recruiter will reach out tomorrow to set up the onsite.", INTERVIEWER),
    ]
}

pub fn evidence(quote: &str, ts: &str) -> Value {
    json!({"quote": quote, "timestamp": ts})
}

pub fn sample(labels_swapped: bool, quote: &str) -> String {
    let score = json!({"score": 4, "rationale": "Solid.", "evidence": [evidence(quote, "00:00:05")]});
    json!({
        "context": {"company": "Northwind", "role_title": "Senior PM", "seniority": "senior", "stage": "hiring_manager",
                    "interviewers": ["Daniel — head of product"], "labels_swapped": labels_swapped},
        "summary": "Went well.",
        "outlook": {"verdict": "leaning_positive", "confidence": "medium", "reasoning": "Next steps discussed.",
                    "signals": [{"direction": "positive", "signal": "Scheduled onsite.",
                                 "evidence": evidence("Our recruiter will reach out tomorrow", "00:00:31")}]},
        "rubric": {"clarity": score, "structure": score, "specificity_and_impact": score, "role_fit": score,
                   "technical_depth": {"score": null, "rationale": "Not probed.", "evidence": []},
                   "curiosity": score, "composure": score},
        "questions": [{"timestamp": "00:00:00", "question": "Tough prioritization call?", "type": "behavioral",
                       "answer_summary": "Onboarding redesign.", "score": 4, "what_worked": "Numbers.",
                       "what_was_missing": "Tradeoff.", "stronger_answer": "Lead with the result."}],
        "strengths": [{"point": "Quantified impact", "evidence": evidence(quote, "00:00:05")}],
        "red_flags": [],
        "coaching": [{"title": "Cut the fillers", "why_it_matters": "Sounds unsure.", "evidence": evidence(quote, "00:00:05"),
                      "fix": "Pause instead.", "drill": "Record a 2-minute answer."}],
    })
    .to_string()
}

pub const GOOD_QUOTE: &str = "we shipped the onboarding redesign in eight weeks";

/// Returns queued responses in order and records every request body it was sent.
pub struct FakeLlm {
    pub responses: RefCell<Vec<Result<String, LlmError>>>,
    pub provider_body: fn(&StructuredRequest) -> Value,
    pub requests: RefCell<Vec<(String, Value)>>,
}

impl FakeLlm {
    /// Records requests as the Claude adapter would send them.
    pub fn new(responses: Vec<Result<String, LlmError>>) -> Self {
        FakeLlm { responses: RefCell::new(responses), requests: RefCell::new(vec![]),
                  provider_body: interview_coach::llm::anthropic::request_body }
    }

    /// Records requests as the OpenAI adapter would send them.
    pub fn openai(responses: Vec<Result<String, LlmError>>) -> Self {
        FakeLlm { provider_body: interview_coach::llm::openai::request_body, ..FakeLlm::new(responses) }
    }
}

impl Llm for FakeLlm {
    fn structured(&self, req: &StructuredRequest, on_progress: &mut dyn FnMut(usize)) -> Result<String, LlmError> {
        self.requests.borrow_mut().push((req.user.to_string(), (self.provider_body)(req)));
        on_progress(10);
        self.responses.borrow_mut().remove(0)
    }
}

pub fn claude() -> ModelRef {
    "anthropic/claude-opus-5-5".parse().unwrap()
}

pub fn setup(mode: Mode) -> (tempfile::TempDir, Db, i64) {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings { data_dir: dir.path().to_path_buf(), ..Settings::load().unwrap() };
    let mut db = Db::open(&settings.db_path()).unwrap();
    let mut s = db
        .create_session(NewSession {
            title: "HM screen".into(), company: None, source: Source::Upload, mode, source_path: None,
            num_speakers: Some(2), consent: None, status: Status::Transcribed,
        })
        .unwrap();
    s.dir = dir.path().display().to_string();
    db.save_session(&s).unwrap();
    db.replace_segments(s.id, &segments()).unwrap();
    (dir, db, s.id)
}


/// A valid "what to do next" plan quoting `quote` (which should be from the interviewer).
pub fn sample_next(quote: &str) -> String {
    json!({
        "headline": "Strong screen: prepare for the onsite.",
        "next_round_prep": [{
            "topic": "The onsite", "why": "They set it up.", "evidence": evidence(quote, "00:00:31"),
            "how_to_prepare": "Rehearse the activation story.", "likely_questions": ["Walk us through activation?"],
        }],
        "practice_plan": [{"skill": "Pausing", "from_coaching": "Cut the fillers", "drill": "Record answers.",
                           "minutes": 10, "priority": "high"}],
    })
    .to_string()
}
