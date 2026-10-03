//! The four stages as recorded runs: what each is built from, what goes out of date when an
//! earlier stage re-runs, failures, history, older-session backfill, and the app's JSON view.

mod common;

use common::fake::*;
use interview_coach::config::ModelRef;
use interview_coach::db::Db;
use interview_coach::llm::LlmError;
use interview_coach::models::{Mode, RunStatus, Step};
use interview_coach::pipeline::{analyze_session, plan_next_steps, report_model, swap_speakers};
use interview_coach::progress::Quiet;
use interview_coach::session_view;
use interview_coach::steps::{self, StageStatus};
use serde_json::json;

const INTERVIEWER_QUOTE: &str = "Our recruiter will reach out tomorrow";

/// A session whose recording and transcript stages already ran (no audio work in a unit test).
fn transcribed(mode: Mode) -> (tempfile::TempDir, Db, i64) {
    let (dir, db, id) = setup(mode);
    let recording = db
        .insert_finished_run(id, Step::Recording, RunStatus::Succeeded, "2026-10-02T10:00:00+00:00",
                             &json!({"sources": []}), None, None, None)
        .unwrap();
    db.insert_finished_run(id, Step::Transcript, RunStatus::Succeeded, "2026-10-02T10:01:00+00:00", &json!({}),
                           Some(recording), None, None)
        .unwrap();
    (dir, db, id)
}

fn statuses(db: &Db, id: i64) -> Vec<StageStatus> {
    steps::flow(db, id).unwrap().into_iter().map(|s| s.status).collect()
}

use StageStatus::{Done, NotRun, OutOfDate};

#[test]
fn each_stage_records_what_it_was_built_from() {
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample_next(INTERVIEWER_QUOTE))]);
    let report = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(statuses(&db, id), [Done, Done, Done, NotRun]);

    let next = plan_next_steps(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(statuses(&db, id), [Done, Done, Done, Done]);
    assert_eq!(next.analysis_id, Some(report.id));
    assert!(next.unverified_quotes.is_empty());

    let flow = steps::flow(&db, id).unwrap();
    let run = |step: Step| flow.iter().find(|s| s.step == step).unwrap().current.clone().unwrap();
    assert_eq!(run(Step::Report).input_run_id, Some(run(Step::Transcript).id));
    assert_eq!(run(Step::Report).output_id, Some(report.id));
    assert_eq!(run(Step::Next).input_run_id, Some(run(Step::Report).id));
    assert_eq!(run(Step::Next).params["model"], "anthropic/claude-opus-5-5");
    // The plan was built from the report and the transcript.
    let prompt = &llm.requests.borrow()[1].0;
    assert!(prompt.contains("<report>") && prompt.contains("Cut the fillers") && prompt.contains("[00:00:31] Interviewer"));
}

#[test]
fn rerunning_an_earlier_stage_puts_later_ones_out_of_date_until_updated() {
    let (_tmp, mut db, id) = transcribed(Mode::Single);
    let llm = FakeLlm::new(vec![
        Ok(sample(false, GOOD_QUOTE)), Ok(sample_next(INTERVIEWER_QUOTE)),
        Ok(sample(false, GOOD_QUOTE)), Ok(sample_next(INTERVIEWER_QUOTE)),
    ]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    plan_next_steps(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();

    // Swapping speakers is a new transcript revision.
    swap_speakers(&mut db, id, &mut Quiet).unwrap();
    assert_eq!(statuses(&db, id), [Done, Done, OutOfDate, OutOfDate]);

    // A new report is current again; next steps still follow the old report.
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(statuses(&db, id), [Done, Done, Done, OutOfDate]);

    plan_next_steps(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(statuses(&db, id), [Done, Done, Done, Done]);
    assert_eq!(db.analyses(id).unwrap().len(), 2, "every report run is kept");
}

#[test]
fn a_failed_rerun_is_recorded_and_the_last_good_result_stays() {
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Err(LlmError::Overloaded)]);
    let first = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    // With another model (the same one would reuse the first report without a call).
    let haiku: ModelRef = "anthropic/claude-haiku-4-5".parse().unwrap();
    assert!(analyze_session(&mut db, &llm, &haiku, id, &mut Quiet).is_err());

    let report = steps::flow(&db, id).unwrap().into_iter().find(|s| s.step == Step::Report).unwrap();
    assert_eq!(report.status, StageStatus::Failed);
    assert!(report.error.unwrap().contains("overloaded"));
    assert_eq!(report.current.unwrap().output_id, Some(first.id));
}

#[test]
fn next_steps_default_to_the_model_that_wrote_the_report() {
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let gpt: ModelRef = "openai/gpt-5.6".parse().unwrap();
    analyze_session(&mut db, &FakeLlm::openai(vec![Ok(sample(false, GOOD_QUOTE))]), &gpt, id, &mut Quiet).unwrap();
    assert_eq!(report_model(&db, id).unwrap(), Some(gpt));
}

#[test]
fn older_sessions_get_their_stages_from_what_they_already_have() {
    let (_tmp, db, id) = setup(Mode::Dual); // segments, but no runs: like a session from before stages
    let session = db.get_session(id).unwrap();
    std::fs::write(std::path::Path::new(&session.dir).join("mic.flac"), b"").unwrap();
    std::fs::write(std::path::Path::new(&session.dir).join("system.flac"), b"").unwrap();
    steps::backfill(&db, &session).unwrap();
    assert_eq!(statuses(&db, id), [Done, Done, NotRun, NotRun]);
    steps::backfill(&db, &session).unwrap(); // only ever once
    assert_eq!(db.runs(id).unwrap().len(), 2);
}

#[test]
fn the_app_view_has_every_stage_with_summaries_and_rerun_rules() {
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample_next(INTERVIEWER_QUOTE))]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    plan_next_steps(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();

    let view = serde_json::to_value(session_view::build(&db, id).unwrap()).unwrap();
    let stages = view["stages"].as_array().unwrap();
    let names: Vec<_> = stages.iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(names, ["recording", "transcript", "report", "next"]);
    assert_eq!(stages[2]["summary"], "Leaning positive · medium confidence");
    assert_eq!(stages[3]["summary"], "1 prep item · 1 drill");
    // This recording kept no sources, so it can't be rebuilt — and the app is told why.
    assert_eq!(stages[0]["can_rerun"], false);
    assert!(stages[0]["rerun_blocked"].as_str().unwrap().contains("weren't kept"));
    assert_eq!(stages[2]["can_rerun"], true);
    assert_eq!(view["turns"].as_array().unwrap().len(), 3);
    assert_eq!(view["reports"][0]["is_current"], true);
    assert!(std::path::Path::new(view["reports"][0]["html_path"].as_str().unwrap()).exists());
    assert_eq!(view["next_steps"]["plan"]["practice_plan"][0]["priority"], "high");
}

/// Writes a track of faint noise and normalizes it the way the recording stage does.
fn track(dir: &std::path::Path, name: &str, seconds: f64) {
    let wav = dir.join(format!("{name}.wav"));
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..(seconds * 16_000.0) as usize {
        w.write_sample(((i % 7) as i16 - 3) * 20).unwrap();
    }
    w.finalize().unwrap();
    interview_coach::audio::normalize(&wav, &dir.join(format!("{name}.flac"))).unwrap();
}

/// The real failure: the mic stopped seconds in while the call kept recording. The model must be
/// told (so missing answers aren't read as silence), and the report and app must say so.
#[test]
fn a_mic_that_stopped_early_is_reported_to_the_model_the_report_and_the_app() {
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let dir = std::path::PathBuf::from(db.get_session(id).unwrap().dir);
    track(&dir, "mic", 25.0);
    track(&dir, "system", 120.0);
    assert!((interview_coach::coverage::flac_duration_s(&dir.join("mic.flac")).unwrap() - 25.0).abs() < 0.01);

    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample_next(INTERVIEWER_QUOTE))]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    plan_next_steps(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    let requests = llm.requests.borrow();
    for (user, _) in requests.iter() {
        assert!(user.contains("<recording_notes>\nThe candidate's microphone track stopped at 00:00:25 but the interview ran to 00:02:00"),
                "{user}");
    }
    assert!(requests[0].0.contains("<metrics>\nLeft out"), "talk-time metrics would be wrong, so they aren't sent");

    let view = serde_json::to_value(session_view::build(&db, id).unwrap()).unwrap();
    let warning = view["audio"]["warnings"][0].as_str().unwrap();
    assert!(warning.starts_with("Your microphone stopped recording at 0:25 of 2:00"), "{warning}");
    assert!(view["stages"][0]["summary"].as_str().unwrap().ends_with("1 warning"));

    let html = std::fs::read_to_string(view["reports"][0]["html_path"].as_str().unwrap()).unwrap();
    assert!(html.contains("Part of this interview wasn't recorded"));
    assert!(html.contains("Left out: part of the conversation wasn't recorded"));
    assert!(!html.contains("your share of talk time"));
}

/// Gives every answer the same verdicts (or fails), recording what it was asked.
struct FakeScorer {
    fail: bool,
    asked: std::cell::RefCell<Vec<String>>,
}

impl interview_coach::scoring::Scorer for FakeScorer {
    fn name(&self) -> String {
        "typesafe/jev-latest".into()
    }

    fn assess(&self, input: &dyn interview_coach::scoring::ScoreInput, set: &interview_coach::scoring::CheckSet, _: usize)
        -> anyhow::Result<interview_coach::scoring::Assessment> {
        use interview_coach::scoring::{Assessment, Verdict};
        self.asked.borrow_mut().push(input.state()["answer"].as_str().unwrap_or_default().to_string());
        if self.fail {
            anyhow::bail!("TypeSafe is down");
        }
        let verdict = |pick: &str, value: f64| Verdict { pick: pick.into(), value, confidence: Some(0.9),
                                                         probabilities: [(pick.to_string(), 0.9)].into() };
        let verdicts = set
            .checks
            .iter()
            .map(|c| {
                let v = match c.id {
                    "leads_with_point" => Verdict { probabilities: [("yes".into(), 0.1), ("no".into(), 0.9)].into(),
                                                    ..verdict("no", 0.1) },
                    "has_quantified_result" => Verdict { probabilities: [("yes".into(), 0.95)].into(), ..verdict("yes", 0.95) },
                    "star_missing" => verdict("none", 0.9),
                    "ownership" => verdict("we", 0.9),
                    _ => verdict("4", 4.1),
                };
                (c.id.to_string(), v)
            })
            .collect();
        Ok(Assessment { scorer: "typesafe/jev-1.13.0".into(), verdicts, latency_ms: 120, input_tokens: 300 })
    }
}

fn checks(scorer: &dyn interview_coach::scoring::Scorer) -> interview_coach::pipeline::ReportExtras<'_> {
    interview_coach::pipeline::ReportExtras { checker: Some(scorer), timeline: None }
}

/// Jev (or Claude) checks each answer after the report; the results are stored with the analysis
/// and shown in the report, and a scorer failure only leaves a warning.
#[test]
fn each_answer_is_checked_after_the_report_and_a_failure_only_warns() {
    use interview_coach::pipeline::analyze_session_with;
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample(false, GOOD_QUOTE))]);
    let scorer = FakeScorer { fail: false, asked: Default::default() };
    let stored = analyze_session_with(&mut db, &llm, &claude(), id, checks(&scorer), &mut Quiet).unwrap();
    assert_eq!(scorer.asked.borrow().len(), 1, "one question in the report, so one answer");
    assert!(scorer.asked.borrow()[0].starts_with("Um, so, like, we shipped"), "the answer is the candidate's turns after it");
    assert_eq!(stored.answer_checks.len(), 6);
    let get = |id: &str| stored.answer_checks.iter().find(|c| c.check_id == id).unwrap();
    assert_eq!((get("leads_with_point").verdict.as_str(), get("has_quantified_result").verdict.as_str()), ("fail", "pass"));
    assert_eq!(get("ownership").verdict, "fail");
    assert_eq!(get("specificity").scorer, "typesafe/jev-1.13.0");

    let session = db.get_session(id).unwrap();
    let html = interview_coach::report::render_html(&session, &stored, None, None);
    assert!(html.contains("Answer by answer"));
    assert!(html.contains("Led with the point 0 of 1 · Gave a number 1 of 1"));
    assert!(html.contains("✓ 4/5") && html.contains("✗ we"));
    assert!(html.contains("Checked answer by answer by typesafe/jev-1.13.0"));

    // A scorer failure (on another interview: this one's verdicts are stored, so they'd be reused).
    let (_tmp2, mut db, id) = transcribed(Mode::Dual);
    let broken = FakeScorer { fail: true, asked: Default::default() };
    let again = analyze_session_with(&mut db, &llm, &claude(), id, checks(&broken), &mut Quiet).unwrap();
    assert!(again.answer_checks.is_empty());
    let run = steps::current_run(&db, id, Step::Report).unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Succeeded);
    assert!(run.warnings[0].contains("answer-by-answer checks were skipped: TypeSafe is down"), "{:?}", run.warnings);
    assert_eq!(run.params["checks"], "typesafe/jev-latest");
}

/// Reads interviewer turns like Jev would: "Great" is warm praise, a recruiter is next steps.
struct RoomScorer;

impl interview_coach::scoring::Scorer for RoomScorer {
    fn name(&self) -> String {
        "typesafe/jev-latest".into()
    }

    fn assess(&self, input: &dyn interview_coach::scoring::ScoreInput, set: &interview_coach::scoring::CheckSet, _: usize)
        -> anyhow::Result<interview_coach::scoring::Assessment> {
        use interview_coach::scoring::{Assessment, Verdict};
        let says = input.state()["interviewer_says"].as_str().unwrap_or_default().to_string();
        let warm = says.starts_with("Great");
        let yes_no = |yes: bool| {
            let p = if yes { 0.9 } else { 0.1 };
            Verdict { pick: if yes { "yes" } else { "no" }.into(), value: p, confidence: Some(0.9),
                      probabilities: [("yes".to_string(), p), ("no".to_string(), 1.0 - p)].into() }
        };
        let verdicts = set
            .checks
            .iter()
            .map(|c| {
                let v = match c.id {
                    "tone" => {
                        let (w, n) = if warm { (0.8, 0.15) } else { (0.1, 0.8) };
                        Verdict { pick: if warm { "warm" } else { "neutral" }.into(), value: w, confidence: Some(0.8),
                                  probabilities: [("warm".to_string(), w), ("neutral".to_string(), n),
                                                  ("cool".to_string(), 1.0 - w - n)].into() }
                    }
                    "positive_reaction" => yes_no(warm),
                    "next_steps" => yes_no(says.contains("recruiter")),
                    _ => yes_no(false),
                };
                (c.id.to_string(), v)
            })
            .collect();
        Ok(Assessment { scorer: "typesafe/jev-1.13.0".into(), verdicts, latency_ms: 90, input_tokens: 200 })
    }
}

/// The report stage reads the room: each interviewer turn and your answer are stored with the
/// report, the page shows the timeline with seek links, and it renders the same every time.
#[test]
fn the_report_reads_the_room_and_renders_it_the_same_every_time() {
    use interview_coach::pipeline::{ReportExtras, analyze_session_with, refresh_timeline};
    use interview_coach::temperature::Kind;
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE))]);
    let extras = ReportExtras { checker: None, timeline: Some(&RoomScorer) };
    let stored = analyze_session_with(&mut db, &llm, &claude(), id, extras, &mut Quiet).unwrap();

    let kinds: Vec<Kind> = stored.turn_signals.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [Kind::Substantive, Kind::Answer, Kind::Substantive], "your two segments are one answer");
    assert_eq!(stored.timeline_scorer.as_deref(), Some("typesafe/jev-1.13.0"));
    let (first, last) = (&stored.turn_signals[0], &stored.turn_signals[2]);
    assert!(last.temperature.unwrap() > first.temperature.unwrap() + 0.3, "praise reads warmer than a plain question");
    assert_eq!(last.latency_s, Some(1.0));
    let run = steps::current_run(&db, id, Step::Report).unwrap().unwrap();
    assert_eq!(run.params["timeline"], "typesafe/jev-latest");
    assert!(run.warnings.iter().any(|w| w.contains("couldn't read the audio")), "no audio in this test: {:?}", run.warnings);

    let session = db.get_session(id).unwrap();
    let html = interview_coach::report::render_html(&session, &stored, None, None);
    assert!(html.contains("<h2>How the room felt</h2>"));
    assert!(html.contains("<svg class='room'") && html.contains("href='#t=31.0'"), "the dots seek");
    assert!(html.contains("<a class='ts' href='#t=31.0'>00:00:31</a>"), "quoted timestamps seek too");
    assert!(html.contains("Next steps mentioned") && html.contains("★ next steps"));
    assert!(html.contains("checked turn by turn by typesafe/jev-1.13.0"));
    let reloaded = db.analysis_by_id(stored.id).unwrap().unwrap();
    assert_eq!(interview_coach::report::render_html(&session, &reloaded, None, None), html, "same rows, same page");

    // Without a TypeSafe key the timeline is rebuilt from voices only, and says so.
    let (voice_only, warnings) = refresh_timeline(&mut db, id, None, &mut Quiet).unwrap();
    assert_eq!(voice_only.id, stored.id, "the same report, no new Claude call");
    assert_eq!((voice_only.turn_signals.len(), voice_only.timeline_scorer.clone()), (3, None));
    assert!(voice_only.turn_signals.iter().all(|s| s.checks.is_empty()));
    assert!(warnings.iter().any(|w| w.contains("couldn't read the audio")));
    let html = interview_coach::report::render_html(&session, &voice_only, None, None);
    assert!(html.contains("add a TypeSafe key in Setup"));
}

/// The same inputs give the same report back: a rerun with nothing changed makes no model call
/// and no new version. Another model makes a new version whose parent is the one it was rerun
/// from, and going back to the first model brings the first version back, still without a call.
#[test]
fn unchanged_reruns_give_the_same_report_back_and_new_models_branch() {
    use interview_coach::config::{ModelRef, Provider};
    let (_tmp, mut db, id) = transcribed(Mode::Dual);
    let haiku = ModelRef { provider: Provider::Anthropic, name: "claude-haiku-4-5".into() };
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample(false, GOOD_QUOTE))]);

    let v1 = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    let again = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!(llm.requests.borrow().len(), 1, "nothing changed, so Claude wasn't asked again");
    assert_eq!(again.id, v1.id);
    assert_eq!(serde_json::to_value(&again.analysis).unwrap(), serde_json::to_value(&v1.analysis).unwrap());
    let run = steps::current_run(&db, id, Step::Report).unwrap().unwrap();
    assert_eq!(run.output_id, Some(v1.id), "the rerun is recorded and points at the same version");
    assert_eq!(db.analyses(id).unwrap().len(), 1);

    let inputs = v1.inputs.clone().unwrap_or_else(|| db.analysis_by_id(v1.id).unwrap().unwrap().inputs.unwrap());
    assert_eq!(inputs.model, "anthropic/claude-opus-5-5");
    assert_eq!(inputs.transcript_sha.len(), 64);

    let v2 = analyze_session(&mut db, &llm, &haiku, id, &mut Quiet).unwrap();
    assert_ne!(v2.id, v1.id);
    assert_eq!(v2.parent_id, Some(v1.id));
    assert_eq!(v2.inputs.as_ref().unwrap().model, "anthropic/claude-haiku-4-5");
    assert_eq!(llm.requests.borrow().len(), 2);

    let back = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_eq!((back.id, llm.requests.borrow().len()), (v1.id, 2), "the first version is back, without a call");
    assert_eq!(pipeline_current(&db, id), v1.id);

    // The history: one transcript, v1 (shown again twice) and v2 re-run from it with another model.
    let history = interview_coach::history::build(&db, id).unwrap();
    let versions: Vec<_> = history.revisions.iter().flat_map(|r| &r.versions).collect();
    assert_eq!(versions.len(), 2);
    assert_eq!((versions[0].number, versions[0].reruns_reused), (1, 2));
    assert_eq!((versions[1].parent, versions[1].changes.clone()), (Some(1), vec!["model claude-opus-5-5 → claude-haiku-4-5".to_string()]));
    assert_eq!(history.current, Some(v1.id));
    let session = db.get_session(id).unwrap();
    let page = interview_coach::report::render_html(&session, &back, None, Some(&history));
    assert!(page.contains("Version 1 of 2 · claude-opus-5-5"), "{page}");
    assert!(page.contains(&format!("<a href='{}.html'>v2</a>", v2.id)));
    assert!(page.contains("From v1: model claude-opus-5-5 → claude-haiku-4-5"));
    assert!(page.contains("Shown again by 2 unchanged re-runs."));
    assert_eq!(page, interview_coach::report::render_html(&session, &back, None, Some(&history)), "deterministic");
    let older = interview_coach::report::render_html(&session, &v2, None, Some(&history));
    assert!(older.contains(&format!("not current (<a href='{}.html'>v1</a> is)", v1.id)));

    // report.html opens the current version's page.
    let index = interview_coach::report::write_pages(&db, &session, None).unwrap().unwrap();
    assert!(std::fs::read_to_string(index).unwrap().contains(&format!("url=reports/{}.html", v1.id)));
}

fn pipeline_current(db: &Db, id: i64) -> i64 {
    interview_coach::pipeline::current_report(db, id).unwrap().unwrap().id
}

/// Swapping speakers keeps the old transcript as a revision, so every version's transcript is known.
#[test]
fn every_transcript_revision_is_kept() {
    let (_tmp, mut db, id) = transcribed(Mode::Single);
    let before = db.get_segments(id).unwrap();
    // The fixture's transcript predates revisions; a speaker swap records the new one.
    swap_speakers(&mut db, id, &mut Quiet).unwrap();
    swap_speakers(&mut db, id, &mut Quiet).unwrap();
    let revisions = db.transcript_revisions(id).unwrap();
    assert_eq!(revisions.len(), 2);
    assert_ne!(revisions[0].1, revisions[1].1);
    assert_eq!(revisions[1].1, interview_coach::versions::transcript_sha(&before), "swapping back is the same transcript");
}

/// Scorer verdicts are stored under their exact inputs: the same answer is judged once, however
/// many times the report is rebuilt.
#[test]
fn the_same_input_is_judged_once() {
    use interview_coach::scoring::Scorer;
    use interview_coach::temperature::TurnInput;
    let (_tmp, db, _) = transcribed(Mode::Dual);
    let counting = FakeScorer { fail: false, asked: Default::default() };
    let cached = interview_coach::versions::CachedScorer { inner: &counting, db: &db };
    let set = interview_coach::temperature::interviewer_set();
    let turn = |says: &str| TurnInput { question: "q".into(), candidate_said: "a".into(), interviewer_says: says.into() };
    let first = cached.assess(&turn("Great answer."), &set, 0).unwrap();
    let again = cached.assess(&turn("Great answer."), &set, 0).unwrap();
    assert_eq!(first, again);
    cached.assess(&turn("Tell me more."), &set, 0).unwrap();
    assert_eq!(counting.asked.borrow().len(), 2, "the repeated turn wasn't sent again");
}
