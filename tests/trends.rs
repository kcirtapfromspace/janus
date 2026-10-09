//! Progress across interviews: each area's direction from the current reviews, what's left out, and
//! the window.

mod common;

use common::fake::*;
use interview_coach::db::{Db, NewSession};
use interview_coach::library;
use interview_coach::models::{Mode, Source, Status};
use interview_coach::pipeline::analyze_session;
use interview_coach::progress::Quiet;
use interview_coach::trends::{self, Direction, Trends};
use serde_json::Value;

/// A review scoring structure and composure as given (the rest of the rubric 4), with one coaching point.
fn review(structure: u8, composure: u8, coaching: &str) -> String {
    let mut v: Value = serde_json::from_str(&sample(false, GOOD_QUOTE)).unwrap();
    v["rubric"]["structure"]["score"] = structure.into();
    v["rubric"]["composure"]["score"] = composure.into();
    v["coaching"][0]["title"] = coaching.into();
    v.to_string()
}

fn interview(db: &mut Db, dir: &std::path::Path, title: &str) -> i64 {
    let mut s = db
        .create_session(NewSession {
            title: title.into(), company: None, source: Source::Upload, mode: Mode::Dual, source_path: None,
            num_speakers: Some(2), consent: None, status: Status::Transcribed,
        })
        .unwrap();
    s.dir = dir.display().to_string();
    db.save_session(&s).unwrap();
    db.replace_segments(s.id, &segments()).unwrap();
    s.id
}

fn reviewed(db: &mut Db, dir: &std::path::Path, title: &str, response: String) -> i64 {
    let id = interview(db, dir, title);
    analyze_session(db, &FakeLlm::new(vec![Ok(response)]), &claude(), id, &mut Quiet).unwrap();
    id
}

fn measure<'a>(t: &'a Trends, id: &str) -> &'a trends::Measure {
    t.measures.iter().find(|m| m.id == id).unwrap_or_else(|| panic!("no measure {id}"))
}

/// Structure climbs and composure falls across four reviews: one is getting better, the other is
/// slipping, and the overall score (their mean with the rest) holds steady. Practice, archived and
/// deleted interviews don't move anything, and a new review updates it straight away.
#[test]
fn directions_follow_the_current_reviews() {
    let (tmp, mut db, first) = setup(Mode::Dual);
    let dir = tmp.path().to_path_buf();
    analyze_session(&mut db, &FakeLlm::new(vec![Ok(review(2, 4, "Lead with the result"))]), &claude(), first, &mut Quiet).unwrap();
    reviewed(&mut db, &dir, "Second", review(2, 4, "Lead with results first"));
    reviewed(&mut db, &dir, "Third", review(4, 2, "Slow down"));
    reviewed(&mut db, &dir, "Fourth", review(4, 2, "Lead with the result"));

    let practice = reviewed(&mut db, &dir, "Mock", review(1, 5, "Lead with the result"));
    db.set_practice(practice).unwrap();
    let archived = reviewed(&mut db, &dir, "Archived", review(1, 5, "Lead with the result"));
    library::archive(&db, archived, true).unwrap();
    let deleted = reviewed(&mut db, &dir, "Deleted", review(1, 5, "Lead with the result"));
    library::delete(&db, deleted).unwrap();
    interview(&mut db, &dir, "Not reviewed yet");

    let t = trends::build(&db, None).unwrap();
    assert_eq!(t.interviews.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["HM screen", "Second", "Third", "Fourth"]);
    let structure = measure(&t, "structure");
    assert_eq!((structure.direction, structure.recent, structure.earlier), (Direction::Improving, Some(4.0), Some(2.0)));
    assert_eq!((structure.recent_count, structure.earlier_count), (2, 2));
    assert_eq!(measure(&t, "composure").direction, Direction::Slipping);
    assert_eq!(measure(&t, "overall").direction, Direction::Steady);
    assert_eq!(measure(&t, "clarity").direction, Direction::Steady);
    // Not probed in any of them: no points, so no direction.
    let depth = measure(&t, "technical_depth");
    assert_eq!((depth.points.len(), depth.direction, depth.needed), (0, Direction::TooFew, 3));
    assert!(t.measures.iter().all(|m| m.id != "leads_with_point"), "habits wait for a scorer");

    assert_eq!(t.summary.headline, "Your overall score is holding steady at about 3.7 out of 5.");
    assert_eq!(t.summary.detail.as_deref(), Some("Getting better: structure. Slipping: composure."));
    assert_eq!((t.summary.improving.clone(), t.summary.slipping.clone()), (vec!["Structure"], vec!["Composure"]));
    assert_eq!(t.summary.recurring.len(), 1);
    let theme = &t.summary.recurring[0];
    assert_eq!((theme.title.as_str(), theme.count, theme.in_latest), ("Lead with the result", 3, true));
    assert_eq!(t.models, ["anthropic/claude-opus-5-5"]);

    // A fifth review, strong all round, lifts the overall score at once.
    reviewed(&mut db, &dir, "Fifth", review(5, 5, "Ask about the team"));
    let t = trends::build(&db, None).unwrap();
    assert_eq!(t.interviews.len(), 5);
    let overall = measure(&t, "overall");
    assert_eq!((overall.recent_count, overall.earlier_count), (2, 3));
    assert!(!t.summary.recurring[0].in_latest);
}

/// The headline counts down to the first directions, and says plainly when nothing's reviewed.
#[test]
fn early_on_it_says_how_many_more_it_needs() {
    let (tmp, mut db, first) = setup(Mode::Dual);
    let t = trends::build(&db, None).unwrap();
    assert_eq!(t.summary.headline, "Your progress shows here once an interview has been reviewed.");
    assert!(t.interviews.is_empty() && t.summary.detail.is_none());

    analyze_session(&mut db, &FakeLlm::new(vec![Ok(review(3, 3, "Slow down"))]), &claude(), first, &mut Quiet).unwrap();
    let t = trends::build(&db, None).unwrap();
    assert_eq!(t.summary.headline, "One reviewed interview so far. Two more and Janus can show which areas are getting better or worse.");
    assert!(t.measures.iter().all(|m| m.direction == Direction::TooFew));
    assert_eq!(measure(&t, "structure").recent, Some(3.0));

    reviewed(&mut db, tmp.path(), "Second", review(3, 3, "Slow down"));
    let t = trends::build(&db, None).unwrap();
    assert!(t.summary.headline.starts_with("Two reviewed interviews so far. One more"), "{}", t.summary.headline);
    assert_eq!(t.summary.recurring[0].count, 2);
}

/// A review that found none of your answers (the mic wasn't recorded) has no scores to trend, and
/// the headline says why rather than counting it as a scored interview.
#[test]
fn a_review_without_your_answers_says_why() {
    let (tmp, mut db, first) = setup(Mode::Dual);
    let mut unscored: Value = serde_json::from_str(&review(3, 3, "Check your mic")).unwrap();
    for key in ["clarity", "structure", "specificity_and_impact", "role_fit", "curiosity", "composure"] {
        unscored["rubric"][key] = serde_json::json!({"score": null, "rationale": "Your answers weren't recorded.", "evidence": []});
    }
    unscored["questions"][0]["score"] = Value::Null;
    analyze_session(&mut db, &FakeLlm::new(vec![Ok(unscored.to_string())]), &claude(), first, &mut Quiet).unwrap();
    let t = trends::build(&db, None).unwrap();
    assert_eq!(t.summary.headline, "One reviewed interview, but its review couldn't score your answers; they may not have been recorded.");
    assert!(measure(&t, "overall").points.is_empty() && measure(&t, "answer_score").points.is_empty());
    // Too few of your words to measure how you spoke, so no talk share either.
    assert!(measure(&t, "talk_share").points.is_empty() && measure(&t, "fillers").points.is_empty());

    reviewed(&mut db, tmp.path(), "Second", review(3, 3, "Slow down"));
    let t = trends::build(&db, None).unwrap();
    assert_eq!(t.summary.headline,
               "Two reviewed interviews so far, 1 with scores. Two more and Janus can show which areas are getting better or worse.");
}

/// `--days` keeps only the interviews in the window, today included.
#[test]
fn the_window_leaves_out_older_interviews() {
    let (tmp, mut db, first) = setup(Mode::Dual);
    let dir = tmp.path().to_path_buf();
    analyze_session(&mut db, &FakeLlm::new(vec![Ok(review(2, 4, "Slow down"))]), &claude(), first, &mut Quiet).unwrap();
    reviewed(&mut db, &dir, "Second", review(2, 4, "Slow down"));
    reviewed(&mut db, &dir, "Third", review(4, 4, "Slow down"));
    let old = (chrono::Utc::now() - chrono::Duration::days(100)).format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
    rusqlite::Connection::open(tmp.path().join("coach.db"))
        .unwrap()
        .execute("UPDATE sessions SET created_at = ?1 WHERE id = ?2", rusqlite::params![old, first])
        .unwrap();

    assert_eq!(trends::build(&db, None).unwrap().interviews.len(), 3);
    let t = trends::build(&db, Some(90)).unwrap();
    assert_eq!((t.days, t.interviews.len()), (Some(90), 2));
    assert_eq!(measure(&t, "structure").direction, Direction::TooFew);
}

// --- the app's fixture --------------------------------------------------------------------------

use interview_coach::models::{INTERVIEWER, Segment, YOU};
use interview_coach::pipeline::{ReportExtras, analyze_session_with};
use interview_coach::scoring::{Assessment, CheckKind, CheckSet, ScoreInput, Scorer, Verdict};

/// Judges answers by the phrases `fixture_answer` puts in them, and the interviewer's tone as set.
struct FixtureScorer {
    warmth: f64,
}

impl Scorer for FixtureScorer {
    fn name(&self) -> String {
        "typesafe/jev-fixture".into()
    }

    fn assess(&self, input: &dyn ScoreInput, set: &CheckSet, _rotation: usize) -> anyhow::Result<Assessment> {
        let state = input.state();
        let answer = state["answer"].as_str().unwrap_or_default().to_lowercase();
        let yes = |p: bool| if p { 0.9 } else { 0.1 };
        let mut verdicts = std::collections::BTreeMap::new();
        for check in &set.checks {
            let (pick, value, probabilities): (String, f64, Vec<(&str, f64)>) = match (&check.kind, check.id) {
                (CheckKind::YesNo { .. }, id) => {
                    let p = match id {
                        "leads_with_point" => yes(answer.starts_with("the short answer")),
                        "has_quantified_result" => yes(answer.contains("percent")),
                        "positive_reaction" | "builds_on_answer" => self.warmth,
                        _ => 0.1,
                    };
                    (if p >= 0.5 { "yes" } else { "no" }.into(), p, vec![("yes", p), ("no", 1.0 - p)])
                }
                (CheckKind::Choice { .. }, "star_missing") => {
                    let pick = if answer.contains("the result was") { "none" } else { "result" };
                    (pick.into(), 0.8, vec![(pick, 0.8)])
                }
                (CheckKind::Choice { .. }, "ownership") => {
                    let pick = if answer.contains(" i ") { "i" } else { "we" };
                    (pick.into(), 0.8, vec![(pick, 0.8)])
                }
                (CheckKind::Choice { .. }, _) => {
                    let pick = if self.warmth >= 0.5 { "warm" } else { "neutral" };
                    (pick.into(), 0.8, vec![("warm", self.warmth), ("neutral", 1.0 - self.warmth), ("cool", 0.0)])
                }
                (CheckKind::Level { .. }, _) => ("3".into(), 3.0, vec![]),
            };
            let probabilities = probabilities.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
            verdicts.insert(check.id.to_string(), Verdict { pick, value, confidence: Some(0.8), probabilities });
        }
        Ok(Assessment { scorer: self.name(), verdicts, latency_ms: 5, input_tokens: 0 })
    }
}

/// Answer `j` of interview `i`: later interviews lead with the point, give numbers, finish the
/// story and own it more often, with fewer fillers.
fn fixture_answer(i: usize, j: usize) -> String {
    let good = |threshold: usize| (i + j) >= threshold;
    let mut parts = vec![];
    parts.push(if good(4) { "The short answer is that we moved the launch." } else { "So, um, there's some background first." });
    parts.push(if i < 2 { "Um, so, like, the team had, like, a lot going on, you know, and" } else if i < 4 { "So, um, the team had a lot going on and" } else { "The team had a lot going on and" });
    parts.push(if good(3) { "I rewrote the plan myself and walked each lead through it," } else { "we sort of reworked the plan together over a few weeks," });
    parts.push(if good(5) { "and activation rose twelve percent in a month." } else { "and things got better after that." });
    parts.push(if good(4) { "The result was a launch on the new date with every team on board." } else { "I guess it worked out in the end, maybe." });
    parts.join(" ")
}

fn fixture_review(i: usize) -> String {
    let rubric = [(2, 4, 2, 3, None, 3, 4), (3, 4, 2, 4, None, 3, 4), (3, 4, 3, 3, Some(3), 3, 4),
                  (4, 4, 3, 4, None, 3, 3), (4, 4, 4, 3, Some(3), 3, 3), (4, 4, 4, 4, Some(3), 2, 3)][i];
    let (structure, clarity, specificity, role_fit, depth, curiosity, composure) = rubric;
    let stages = ["recruiter_screen", "hiring_manager", "technical", "behavioral", "panel", "final"];
    let companies = ["Northwind", "Fabrikam", "Contoso", "Northwind", "Litware", "Fabrikam"];
    let verdicts = ["mixed", "mixed", "leaning_positive", "mixed", "leaning_positive", "strong"];
    let coaching: [&[&str]; 6] = [
        &["Lead with the result", "Quantify your impact", "Slow down"],
        &["Lead with results first", "Quantify impact with numbers", "Ask about the team"],
        &["Lead with the result", "Name your own part", "Pause before answering"],
        &["Quantify your impact", "Name your own part", "Keep answers under two minutes"],
        &["Name your own part", "Pause before answering", "Keep answers under two minutes"],
        &["Pause before answering", "Keep answers shorter", "Ask about the roadmap"],
    ];
    let mut v: Value = serde_json::from_str(&sample(false, GOOD_QUOTE)).unwrap();
    let score = |s: Option<u8>| match s {
        Some(s) => serde_json::json!({"score": s, "rationale": "From the answers.", "evidence": []}),
        None => serde_json::json!({"score": null, "rationale": "Not probed.", "evidence": []}),
    };
    v["context"]["company"] = companies[i].into();
    v["context"]["stage"] = stages[i].into();
    v["outlook"]["verdict"] = verdicts[i].into();
    for (key, s) in [("structure", Some(structure)), ("clarity", Some(clarity)), ("specificity_and_impact", Some(specificity)),
                     ("role_fit", Some(role_fit)), ("technical_depth", depth), ("curiosity", Some(curiosity)),
                     ("composure", Some(composure))] {
        v["rubric"][key] = score(s);
    }
    let question = v["questions"][0].clone();
    v["questions"] = (0..4)
        .map(|j| {
            let mut q = question.clone();
            q["timestamp"] = format!("00:0{j}:00").into();
            q["question"] = ["Walk me through a launch you ran.", "Tell me about a hard tradeoff.",
                             "How did you handle a slipping deadline?", "What would you change about it?"][j].into();
            q["score"] = (2 + (i + j) / 3).min(5).into();
            q
        })
        .collect();
    let point = v["coaching"][0].clone();
    v["coaching"] = coaching[i].iter().map(|t| { let mut c = point.clone(); c["title"] = (*t).into(); c }).collect();
    v.to_string()
}

/// Regenerates the Mac app's dashboard fixture from six made-up interviews run through the real
/// report stage: `cargo test --test trends -- --ignored`. Set IC_FIXTURE_LIBRARY to a path to also
/// write the matching `ic list --json --all` (for IC_SNAPSHOT_LIBRARY).
#[test]
#[ignore]
fn write_the_dashboard_fixture() {
    let (tmp, mut db, _) = setup(Mode::Dual);
    let dir = tmp.path().to_path_buf();
    db.erase_session(1).unwrap();
    let mut ids = vec![];
    for i in 0..6 {
        let id = interview(&mut db, &dir, ["Recruiter screen", "Hiring manager", "Technical deep dive", "Behavioral loop",
                                           "Panel", "Final round"][i]);
        let segments: Vec<Segment> = (0..4)
            .flat_map(|j| {
                let at = 60.0 * j as f64;
                [Segment::new(at, at + 5.0, ["Walk me through a launch you ran.", "Tell me about a hard tradeoff.",
                                             "How did you handle a slipping deadline?", "What would you change about it?"][j], INTERVIEWER),
                 Segment::new(at + 6.0, at + 50.0, &fixture_answer(i, j), YOU)]
            })
            .collect();
        db.replace_segments(id, &segments).unwrap();
        let scorer = FixtureScorer { warmth: 0.25 + 0.1 * i as f64 };
        let model = if i == 2 { "anthropic/claude-sonnet-5-5".parse().unwrap() } else { claude() };
        let extras = ReportExtras { checker: Some(&scorer), timeline: Some(&scorer), require_evaluation: false };
        analyze_session_with(&mut db, &FakeLlm::new(vec![Ok(fixture_review(i))]), &model, id, extras, &mut Quiet).unwrap();
        ids.push(id);
    }
    let conn = rusqlite::Connection::open(tmp.path().join("coach.db")).unwrap();
    for (i, id) in ids.iter().enumerate() {
        let at = chrono::NaiveDate::from_ymd_opt(2026, 8, 28).unwrap() + chrono::Duration::days(7 * i as i64);
        conn.execute("UPDATE sessions SET created_at = ?1, duration_s = ?2 WHERE id = ?3",
                     rusqlite::params![format!("{at}T15:00:00+00:00"), 1800.0 + 300.0 * i as f64, id]).unwrap();
    }
    // The notebook's default window, on a fixed day, so the fixture doesn't age out.
    let today = chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 10, 9, 12, 0, 0).unwrap();
    let t = trends::build_at(&db, Some(90), today).unwrap();
    assert_eq!(t.interviews.len(), 6);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(root.join("mac/Tests/InterviewCoachKitTests/Fixtures/trends.json"), serde_json::to_string_pretty(&t).unwrap()).unwrap();
    if let Ok(path) = std::env::var("IC_FIXTURE_LIBRARY") {
        std::fs::write(path, serde_json::to_string(&library::library(&db).unwrap()).unwrap()).unwrap();
    }
}
