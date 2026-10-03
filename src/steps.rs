//! Where each of an interview's four stages stands.
//!
//! Every attempt at a stage is a `step_runs` row recording the upstream run it was built from.
//! A stage is *out of date* when that upstream run is no longer the upstream stage's latest
//! success — or when the upstream stage is itself out of date. Nothing re-runs automatically.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use serde_json::json;

use crate::db::{Db, StepRun};
use crate::models::{Mode, RunStatus, Session, Source, Status, Step};
use crate::progress::Progress;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    NotRun,
    Running,
    Done,
    OutOfDate,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageState {
    pub step: Step,
    pub status: StageStatus,
    /// The most recent attempt, whatever its outcome.
    pub latest: Option<StepRun>,
    /// The most recent success: what the stage shows.
    pub current: Option<StepRun>,
    pub out_of_date: bool,
    /// Why the latest attempt failed (including a run whose process died).
    pub error: Option<String>,
}

fn pid_alive(pid: i64) -> bool {
    // Signal 0 checks existence; EPERM still means the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Stage states from a session's runs (oldest first). `alive` says whether a running run's process
/// still exists; a "running" run whose process is gone was interrupted.
pub fn compute(runs: &[StepRun], alive: impl Fn(i64) -> bool) -> Vec<StageState> {
    let mut states: Vec<StageState> = vec![];
    for step in Step::ALL.iter().copied() {
        let of_step = || runs.iter().filter(move |r| r.step == step);
        let latest = of_step().next_back().cloned();
        let current = of_step().rfind(|r| r.status == RunStatus::Succeeded).cloned();
        let out_of_date = match (&current, step.upstream()) {
            (Some(cur), Some(up)) => {
                let upstream = states.iter().find(|s| s.step == up).expect("upstream computed first");
                upstream.out_of_date || cur.input_run_id != upstream.current.as_ref().map(|r| r.id)
            }
            _ => false,
        };
        let (status, error) = match &latest {
            Some(r) if r.status == RunStatus::Running && r.pid.is_none_or(&alive) => (StageStatus::Running, None),
            Some(r) if r.status == RunStatus::Running => {
                (StageStatus::Failed, Some("Interrupted: the process running this stage stopped.".to_string()))
            }
            Some(r) if r.status == RunStatus::Failed => (StageStatus::Failed, r.error.clone()),
            _ if current.is_some() && out_of_date => (StageStatus::OutOfDate, None),
            _ if current.is_some() => (StageStatus::Done, None),
            _ => (StageStatus::NotRun, None),
        };
        states.push(StageState { step, status, latest, current, out_of_date, error });
    }
    states
}

pub fn flow(db: &Db, session_id: i64) -> Result<Vec<StageState>> {
    Ok(compute(&db.runs(session_id)?, pid_alive))
}

/// The run a new run of `step` would be built from: the upstream stage's latest success.
pub fn upstream_run(db: &Db, session_id: i64, step: Step) -> Result<Option<i64>> {
    let Some(up) = step.upstream() else { return Ok(None) };
    Ok(current_run(db, session_id, up)?.map(|r| r.id))
}

pub fn current_run(db: &Db, session_id: i64, step: Step) -> Result<Option<StepRun>> {
    Ok(db.runs(session_id)?.into_iter().rfind(|r| r.step == step && r.status == RunStatus::Succeeded))
}

/// Audio files a recording stage would rebuild from, recorded in its params.
pub fn recording_sources(run: &StepRun) -> Vec<PathBuf> {
    run.params["sources"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(PathBuf::from)).collect()
}

/// Synthesize runs for a session from before stage runs existed, from what it already has.
pub fn backfill(db: &Db, session: &Session) -> Result<()> {
    if !db.runs(session.id)?.is_empty() || session.status == Status::Recording {
        return Ok(());
    }
    let dir = Path::new(&session.dir);
    let at = session.created_at.as_str();
    let has_audio = match session.mode {
        Mode::Single => dir.join("audio.flac").exists(),
        Mode::Dual => dir.join("mic.flac").exists() && dir.join("system.flac").exists(),
    };
    let sources: Vec<String> = match (session.source, session.mode) {
        (Source::Recording, _) => ["mic.wav", "system.wav"].iter().map(|f| dir.join(f).display().to_string()).collect(),
        (Source::Upload, Mode::Single) => session.source_path.iter().cloned().collect(),
        (Source::Upload, Mode::Dual) => vec![], // the original track paths weren't kept
    };
    let failed = |step: Step, input: Option<i64>| -> Result<()> {
        if session.status == Status::Failed {
            db.insert_finished_run(session.id, step, RunStatus::Failed, at, &json!({}), input, None, session.error.as_deref())?;
        }
        Ok(())
    };
    if !has_audio {
        return failed(Step::Recording, None);
    }
    let recording = db.insert_finished_run(session.id, Step::Recording, RunStatus::Succeeded, at,
                                           &json!({"sources": sources}), None, None, None)?;
    if db.get_segments(session.id)?.is_empty() {
        return failed(Step::Transcript, Some(recording));
    }
    let transcript = db.insert_finished_run(session.id, Step::Transcript, RunStatus::Succeeded, at,
                                            &json!({"speakers": session.num_speakers}), Some(recording), None, None)?;
    let mut analyses = db.analyses(session.id)?;
    if analyses.is_empty() {
        return failed(Step::Report, Some(transcript));
    }
    analyses.reverse(); // oldest first, so the newest is the current run
    for a in analyses {
        db.insert_finished_run(session.id, Step::Report, RunStatus::Succeeded, &a.created_at,
                               &json!({"model": a.model}), Some(transcript), Some(a.id), None)?;
    }
    Ok(())
}

/// Forwards progress to the terminal UI and mirrors it into the run row (at most about once a
/// second) through its own connection, so the app can show live progress while ic works.
pub struct RunProgress<'a> {
    db: Db,
    run: i64,
    inner: &'a mut dyn Progress,
    message: String,
    last_write: Instant,
}

impl<'a> RunProgress<'a> {
    pub fn new(db: &Db, run: i64, inner: &'a mut dyn Progress) -> Result<Self> {
        Ok(RunProgress { db: db.reopen()?, run, inner, message: String::new(), last_write: Instant::now() })
    }
}

impl Progress for RunProgress<'_> {
    fn stage(&mut self, message: &str) {
        self.inner.stage(message);
        self.message = message.to_string();
        let _ = self.db.set_run_progress(self.run, None, message);
        self.last_write = Instant::now();
    }

    fn step(&mut self, done: u64, total: u64) {
        self.inner.step(done, total);
        if total > 0 && (done == total || self.last_write.elapsed().as_millis() >= 1000) {
            let pct = (done as f64 / total as f64 * 100.0).min(100.0);
            let _ = self.db.set_run_progress(self.run, Some(pct), &self.message);
            self.last_write = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, step: Step, status: RunStatus, input: Option<i64>) -> StepRun {
        StepRun {
            id, session_id: 1, step, status, started_at: "t".into(), finished_at: None, params: json!({}),
            input_run_id: input, output_id: None, error: (status == RunStatus::Failed).then(|| "boom".to_string()),
            progress: None, message: None, pid: (status == RunStatus::Running).then_some(42), warnings: vec![],
        }
    }

    fn statuses(runs: &[StepRun]) -> Vec<StageStatus> {
        compute(runs, |_| true).into_iter().map(|s| s.status).collect()
    }

    use RunStatus::{Failed, Running, Succeeded};
    use StageStatus::{Done, NotRun, OutOfDate};

    #[test]
    fn a_complete_pipeline_is_all_done() {
        let runs = [run(1, Step::Recording, Succeeded, None), run(2, Step::Transcript, Succeeded, Some(1)),
                    run(3, Step::Report, Succeeded, Some(2)), run(4, Step::Next, Succeeded, Some(3))];
        assert_eq!(statuses(&runs), [Done, Done, Done, Done]);
    }

    #[test]
    fn rerunning_the_transcript_puts_everything_after_it_out_of_date() {
        let runs = [run(1, Step::Recording, Succeeded, None), run(2, Step::Transcript, Succeeded, Some(1)),
                    run(3, Step::Report, Succeeded, Some(2)), run(4, Step::Next, Succeeded, Some(3)),
                    run(5, Step::Transcript, Succeeded, Some(1))];
        assert_eq!(statuses(&runs), [Done, Done, OutOfDate, OutOfDate]);
    }

    #[test]
    fn a_new_report_only_puts_next_steps_out_of_date() {
        let runs = [run(1, Step::Recording, Succeeded, None), run(2, Step::Transcript, Succeeded, Some(1)),
                    run(3, Step::Report, Succeeded, Some(2)), run(4, Step::Next, Succeeded, Some(3)),
                    run(5, Step::Report, Succeeded, Some(2))];
        assert_eq!(statuses(&runs), [Done, Done, Done, OutOfDate]);
    }

    #[test]
    fn a_failed_rerun_keeps_the_previous_result_and_shows_the_error() {
        let runs = [run(1, Step::Recording, Succeeded, None), run(2, Step::Transcript, Succeeded, Some(1)),
                    run(3, Step::Report, Succeeded, Some(2)), run(4, Step::Report, Failed, Some(2))];
        let states = compute(&runs, |_| true);
        assert_eq!(states[2].status, StageStatus::Failed);
        assert_eq!(states[2].error.as_deref(), Some("boom"));
        assert_eq!(states[2].current.as_ref().map(|r| r.id), Some(3), "the last good report is still shown");
        assert_eq!(states[3].status, NotRun);
    }

    #[test]
    fn a_running_run_whose_process_died_reads_as_interrupted() {
        let runs = [run(1, Step::Recording, Succeeded, None), run(2, Step::Transcript, Running, Some(1))];
        assert_eq!(compute(&runs, |_| true)[1].status, StageStatus::Running);
        let dead = compute(&runs, |_| false);
        assert_eq!(dead[1].status, StageStatus::Failed);
        assert!(dead[1].error.as_deref().unwrap().contains("Interrupted"));
    }
}
