//! Analysis flow with a fake Claude: parsing, quote checks, storage, swapped-label repair, reports.

use std::cell::RefCell;

use interview_coach::analyze::{analyze, build_user_message, unverified_quotes};
use interview_coach::config::{ModelRef, Settings};
use interview_coach::llm::{Llm, LlmError, StructuredRequest};
use interview_coach::db::{Db, NewSession};
use interview_coach::merge::to_turns;
use interview_coach::metrics::compute;
use interview_coach::models::{INTERVIEWER, Mode, OutcomeResult, Segment, SessionAnalysis, Source, Stage, Status, YOU};
use interview_coach::pipeline::analyze_session;
use interview_coach::progress::Quiet;
use interview_coach::report::render_html;
use serde_json::{Value, json};

fn segments() -> Vec<Segment> {
    vec![
        Segment::new(0.0, 4.0, "Tell me about a tough prioritization call.", INTERVIEWER),
        Segment::new(5.0, 20.0, "Um, so, like, we shipped the onboarding redesign in eight weeks.", YOU),
        Segment::new(21.0, 30.0, "Activation went from sixty to seventy eight percent. What does success look like?", YOU),
        Segment::new(31.0, 36.0, "Great. Our recruiter will reach out tomorrow to set up the onsite.", INTERVIEWER),
    ]
}

fn evidence(quote: &str, ts: &str) -> Value {
    json!({"quote": quote, "timestamp": ts})
}

fn sample(labels_swapped: bool, quote: &str) -> String {
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

const GOOD_QUOTE: &str = "we shipped the onboarding redesign in eight weeks";

/// Returns queued responses in order and records every request body it was sent.
struct FakeLlm {
    responses: RefCell<Vec<Result<String, LlmError>>>,
    provider_body: fn(&StructuredRequest) -> Value,
    requests: RefCell<Vec<(String, Value)>>,
}

impl FakeLlm {
    /// Records requests as the Claude adapter would send them.
    fn new(responses: Vec<Result<String, LlmError>>) -> Self {
        FakeLlm { responses: RefCell::new(responses), requests: RefCell::new(vec![]),
                  provider_body: interview_coach::llm::anthropic::request_body }
    }

    /// Records requests as the OpenAI adapter would send them.
    fn openai(responses: Vec<Result<String, LlmError>>) -> Self {
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

fn claude() -> ModelRef {
    "anthropic/claude-opus-5-5".parse().unwrap()
}

fn setup(mode: Mode) -> (tempfile::TempDir, Db, i64) {
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

#[test]
fn user_message_contains_transcript_and_metrics() {
    let segs = segments();
    let msg = build_user_message(&to_turns(&segs), &compute(&segs, Mode::Dual), "HM screen", Some("Northwind"), Mode::Dual, None);
    assert!(msg.contains("[00:00:05] You: Um, so, like"), "{msg}");
    assert!(msg.contains("separate mic/system tracks") && msg.contains("\"your_share\""));
}

#[test]
fn analyze_parses_validates_and_sends_the_schema() {
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE))]);
    let a = analyze(&llm, "claude-opus-5-5", "transcript", &mut |_| {}).unwrap();
    assert_eq!(a.outlook.verdict.as_str(), "leaning_positive");
    assert_eq!(a.rubric.technical_depth.score, None);
    let body = &llm.requests.borrow()[0].1;
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"]["format"]["schema"]["additionalProperties"], false);
}

#[test]
fn out_of_range_scores_are_rejected() {
    let bad = sample(false, GOOD_QUOTE).replace("\"score\":4", "\"score\":9");
    let llm = FakeLlm::new(vec![Ok(bad)]);
    let err = analyze(&llm, "m", "t", &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("failed validation"), "{err}");
}

#[test]
fn the_same_typed_analysis_works_through_the_openai_adapter() {
    let (_tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::openai(vec![Ok(sample(false, GOOD_QUOTE))]);
    let gpt: ModelRef = "openai/gpt-5.6".parse().unwrap();
    let stored = analyze_session(&mut db, &llm, &gpt, id, &mut Quiet).unwrap();
    assert_eq!(stored.model, "openai/gpt-5.6", "reports must show which model judged the interview");
    let body = &llm.requests.borrow()[0].1;
    assert_eq!(body["model"], "gpt-5.6", "the provider prefix is stripped before the API call");
    assert_eq!(body["text"]["format"]["name"], "session_analysis");
    assert_eq!(body["text"]["format"]["schema"]["additionalProperties"], false);
    assert_eq!(body["store"], false);
}

#[test]
fn unverified_quotes_flags_paraphrases() {
    let turns = to_turns(&segments());
    let ok: SessionAnalysis = serde_json::from_str(&sample(false, GOOD_QUOTE)).unwrap();
    assert!(unverified_quotes(&ok, &turns).is_empty());
    let bad: SessionAnalysis = serde_json::from_str(&sample(false, "we delivered the new onboarding quickly")).unwrap();
    assert!(unverified_quotes(&bad, &turns).contains(&"we delivered the new onboarding quickly".to_string()));
}

#[test]
fn analyze_session_stores_result_and_fills_session_context() {
    let (_tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE))]);
    let stored = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(db.latest_analysis(id).unwrap().unwrap().id, stored.id);
    let s = db.get_session(id).unwrap();
    assert_eq!((s.status, s.stage, s.company.as_deref()), (Status::Analyzed, Some(Stage::HiringManager), Some("Northwind")));
    assert_eq!(db.latest_verdicts().unwrap()[&id].as_str(), "leaning_positive");
    assert!(std::path::Path::new(&s.dir).join("analysis.json").exists());
}

#[test]
fn swapped_labels_are_fixed_and_reanalysed_for_single_track() {
    let (_tmp, mut db, id) = setup(Mode::Single);
    let llm = FakeLlm::new(vec![Ok(sample(true, GOOD_QUOTE)), Ok(sample(false, GOOD_QUOTE))]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(llm.requests.borrow().len(), 2);
    assert_eq!(db.get_segments(id).unwrap()[0].speaker, YOU); // first speaker was the interviewer; now swapped
    assert!(llm.requests.borrow()[1].0.contains("[00:00:00] You: Tell me about"));
}

#[test]
fn failed_analysis_marks_the_session_failed() {
    let (_tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![Err(LlmError::Refusal("category: cyber".into()))]);
    let err = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap_err();
    assert!(err.to_string().contains("declined"), "{err}");
    let s = db.get_session(id).unwrap();
    assert_eq!(s.status, Status::Failed);
    assert!(s.error.unwrap().contains("declined"));
}

#[test]
fn html_report_renders_with_outcome() {
    let (_tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE))]);
    let stored = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    let outcome = db.set_outcome(id, OutcomeResult::Offer, None).unwrap();
    let html = render_html(&db.get_session(id).unwrap(), &stored, Some(&outcome));
    assert!(html.contains("<b>Offer</b>") && html.contains("Cut the fillers") && html.contains("prefers-color-scheme"));
    assert!(html.contains("Daniel") || html.contains("Northwind"));
}
