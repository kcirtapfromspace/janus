//! Everything about one session in one payload (`ic session N --json`), shaped for the app's
//! pipeline view: each stage's state, the audio, the transcript, report history, next steps.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::capture;
use crate::coverage;
use crate::db::{Db, StoredNextSteps};
use crate::merge::to_turns;
use crate::models::{Mode, Session, Step, fmt_ts, speaker_label};
use crate::pipeline;
use crate::report;
use crate::steps::{self, StageState, StageStatus};
use crate::video;

#[derive(Serialize)]
pub struct SessionView {
    pub session: SessionInfo,
    pub stages: Vec<StageView>,
    pub audio: AudioView,
    pub turns: Vec<TurnView>,
    /// Newest first; `is_current` marks the one the current report stage points at.
    pub reports: Vec<ReportSummary>,
    pub next_steps: Option<StoredNextSteps>,
    pub outcome: Option<OutcomeView>,
    /// Your answers with what the call's video showed (from the current report), for correcting.
    pub video_answers: Vec<VideoAnswer>,
}

#[derive(Serialize)]
pub struct VideoAnswer {
    pub start: f64,
    pub end: f64,
    pub timestamp: String,
    /// The interviewer's question before it.
    pub question: Option<String>,
    pub cues: crate::video::VideoCues,
    /// What the review says about it.
    pub notes: Vec<String>,
    /// Your correction, if you made one (`ic eval correct`).
    pub corrected: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct SessionInfo {
    pub id: i64,
    pub title: String,
    pub company: Option<String>,
    pub stage: Option<&'static str>,
    pub created_at: String,
    pub duration_s: Option<f64>,
    pub mode: &'static str,
    pub source: &'static str,
    pub status: &'static str,
    pub dir: String,
}

#[derive(Serialize)]
pub struct StageView {
    pub step: Step,
    pub label: &'static str,
    pub status: StageStatus,
    pub summary: Option<String>,
    pub last_run_at: Option<String>,
    pub duration_s: Option<f64>,
    pub error: Option<String>,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub model: Option<String>,
    pub can_rerun: bool,
    pub rerun_blocked: Option<String>,
    /// Problems with this stage's current result.
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct AudioView {
    /// Both tracks mixed into one file to listen to.
    pub listen_path: Option<String>,
    /// The call's window, recorded alongside (silent; it starts when the audio does).
    pub video_path: Option<String>,
    pub tracks: Vec<TrackView>,
    /// Problems the recorder noticed (e.g. a silent track).
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct TrackView {
    pub name: &'static str,
    pub label: &'static str,
    pub path: String,
}

#[derive(Serialize)]
pub struct TurnView {
    pub speaker: String,
    pub speaker_label: String,
    pub start: f64,
    pub end: f64,
    pub timestamp: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct ReportSummary {
    pub analysis_id: i64,
    pub created_at: String,
    pub model: String,
    pub verdict: &'static str,
    pub verdict_label: &'static str,
    pub confidence: &'static str,
    pub html_path: String,
    pub is_current: bool,
    pub unverified_quotes: usize,
}

#[derive(Serialize)]
pub struct OutcomeView {
    pub result: &'static str,
    pub label: &'static str,
    pub notes: Option<String>,
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn seconds_between(start: &str, end: Option<&str>) -> Option<f64> {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    Some((parse(end?)? - parse(start)?).num_milliseconds() as f64 / 1000.0)
}

pub fn build(db: &Db, id: i64) -> Result<SessionView> {
    let session = db.get_session(id)?;
    let dir = Path::new(&session.dir);
    let flow = steps::flow(db, id)?;
    let segments = db.get_segments(id)?;
    let turns = to_turns(&segments);
    let outcome = db.get_outcome(id)?;
    let current_report = pipeline::current_report(db, id)?;
    let next_steps = db.latest_next_steps(id)?;
    let busy = flow.iter().any(|s| s.status == StageStatus::Running);

    // Kept current: e.g. a recording-gap notice added after a page was first written, or a new version.
    report::write_pages(db, &session, outcome.as_ref())?;
    let mut reports = vec![];
    for a in db.analyses(id)? {
        let path = report::analysis_html_path(&session, a.id);
        reports.push(ReportSummary {
            analysis_id: a.id,
            is_current: current_report.as_ref().is_some_and(|c| c.id == a.id),
            created_at: a.created_at,
            model: a.model,
            verdict: a.analysis.outlook.verdict.as_str(),
            verdict_label: a.analysis.outlook.verdict.label(),
            confidence: a.analysis.outlook.confidence.as_str(),
            html_path: path.display().to_string(),
            unverified_quotes: a.unverified_quotes.len(),
        });
    }

    let coverage_notes = coverage::recording_notes(dir, session.mode).for_you;
    let video_file = dir.join(video::VIDEO_FILE);
    let mut recorder_warnings = coverage_notes.clone();
    recorder_warnings.extend(capture::read_report(dir).map(|r| capture::report_warnings(&r)).unwrap_or_default());
    // A track that stopped early shows on the transcript too: that's where its missing lines are noticed.
    let stage_warnings = |state: &StageState| -> Vec<String> {
        match state.step {
            Step::Recording => {
                let mut w = recorder_warnings.clone();
                w.extend(state.current.iter().flat_map(|r| r.warnings.clone()));
                w
            }
            Step::Transcript => {
                let mut w = coverage_notes.clone();
                w.extend(state.current.iter().flat_map(|r| r.warnings.clone()));
                w
            }
            _ => state.current.as_ref().map(|r| r.warnings.clone()).unwrap_or_default(),
        }
    };
    let summary = |state: &StageState| -> Option<String> {
        state.current.as_ref()?;
        Some(match state.step {
            Step::Recording => {
                let tracks = if session.mode == Mode::Dual { "2 tracks" } else { "1 track" };
                let video = if video_file.exists() { " + video" } else { "" };
                let warn = match stage_warnings(state).len() {
                    0 => String::new(),
                    n => format!(" · {n} warning{}", if n == 1 { "" } else { "s" }),
                };
                format!("{} · {tracks}{video}{warn}", fmt_ts(session.duration_s.unwrap_or(0.0)))
            }
            Step::Transcript => {
                let swapped = state.current.as_ref().is_some_and(|r| r.params["kind"] == "swap");
                let warnings = stage_warnings(state).len();
                format!("{}{}{}", plural(turns.len(), "turn"), if swapped { " · speakers swapped" } else { "" },
                        if warnings > 0 { format!(" · {}", plural(warnings, "warning")) } else { String::new() })
            }
            Step::Report => {
                let a = current_report.as_ref()?;
                format!("{} · {} confidence", a.analysis.outlook.verdict.label(), a.analysis.outlook.confidence)
            }
            Step::Next => {
                let n = next_steps.as_ref()?;
                format!("{} · {}", plural(n.plan.next_round_prep.len(), "prep item"), plural(n.plan.practice_plan.len(), "drill"))
            }
        })
    };

    let mut stages = vec![];
    for state in &flow {
        let upstream_done = state.step.upstream().is_none_or(|up| flow.iter().any(|s| s.step == up && s.current.is_some()));
        let blocked = if busy {
            Some("Another step is running.".to_string())
        } else if state.step == Step::Recording {
            pipeline::reprocess_blocker(db, id)?
        } else if !upstream_done {
            Some(format!("Needs the {} first.", state.step.upstream().map(|u| u.label().to_lowercase()).unwrap_or_default()))
        } else {
            None
        };
        let shown = state.latest.as_ref();
        stages.push(StageView {
            step: state.step,
            label: state.step.label(),
            status: state.status,
            summary: summary(state),
            last_run_at: shown.map(|r| r.finished_at.clone().unwrap_or_else(|| r.started_at.clone())),
            duration_s: shown.and_then(|r| seconds_between(&r.started_at, r.finished_at.as_deref())),
            error: state.error.clone(),
            progress: shown.filter(|_| state.status == StageStatus::Running).and_then(|r| r.progress),
            message: shown.filter(|_| state.status == StageStatus::Running).and_then(|r| r.message.clone()),
            model: state.current.as_ref().or(shown).and_then(|r| r.params["model"].as_str().map(String::from)),
            can_rerun: blocked.is_none(),
            rerun_blocked: blocked,
            warnings: stage_warnings(state),
        });
    }

    let tracks: Vec<TrackView> = match session.mode {
        Mode::Single => vec![TrackView { name: "audio", label: "Recording", path: dir.join("audio.flac").display().to_string() }],
        Mode::Dual => vec![
            TrackView { name: "mic", label: "You (mic)", path: dir.join("mic.flac").display().to_string() },
            TrackView { name: "system", label: "Interviewer (call audio)", path: dir.join("system.flac").display().to_string() },
        ],
    };
    let listen = dir.join("listen.m4a");

    Ok(SessionView {
        session: SessionInfo {
            // You entered none: the company the current report inferred.
            company: session.company.clone().or_else(|| current_report.as_ref().and_then(|r| r.analysis.context.company.clone())),
            ..info(&session)
        },
        stages,
        audio: AudioView {
            listen_path: listen.exists().then(|| listen.display().to_string()),
            video_path: video_file.exists().then(|| video_file.display().to_string()),
            tracks,
            warnings: recorder_warnings,
        },
        turns: turns
            .iter()
            .map(|t| TurnView {
                speaker: t.speaker.clone(),
                speaker_label: speaker_label(&t.speaker),
                start: t.start,
                end: t.end,
                timestamp: fmt_ts(t.start),
                text: t.text.clone(),
            })
            .collect(),
        reports,
        next_steps,
        outcome: outcome.map(|o| OutcomeView { result: o.result.as_str(), label: o.result.label(), notes: o.notes }),
        video_answers: current_report.as_ref().map(|r| video_answers(&r.turn_signals)).unwrap_or_default(),
    })
}

fn video_answers(signals: &[crate::temperature::Signal]) -> Vec<VideoAnswer> {
    use crate::temperature::Kind;
    let mut question: Option<String> = None;
    let mut out = vec![];
    for s in signals {
        match s.kind {
            Kind::Substantive => question = Some(s.text.clone()),
            Kind::Answer => {
                if let Some(cues) = s.video {
                    out.push(VideoAnswer {
                        start: s.start,
                        end: s.end,
                        timestamp: fmt_ts(s.start),
                        question: question.clone(),
                        cues,
                        notes: crate::video::notes(&cues, &crate::video::GATE),
                        corrected: None,
                    });
                }
            }
            Kind::Backchannel => {}
        }
    }
    out
}

fn info(s: &Session) -> SessionInfo {
    SessionInfo {
        id: s.id,
        title: s.title.clone(),
        company: s.company.clone(),
        stage: s.stage.map(|st| st.label()),
        created_at: s.created_at.clone(),
        duration_s: s.duration_s,
        mode: s.mode.as_str(),
        source: s.source.as_str(),
        status: s.status.as_str(),
        dir: s.dir.clone(),
    }
}
