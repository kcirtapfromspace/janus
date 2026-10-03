//! Session processing in four stages — recording, transcript, after-action report, what to do
//! next — each a recorded run that can be repeated on its own (see `steps`).
//!
//! Session folders hold normalized audio (`audio.flac` for single-track uploads, `mic.flac` +
//! `system.flac` for dual-track), a mixed `listen.m4a`, and human-readable exports. The database
//! is the source of truth.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::analyze::{self, PROMPT_VERSION};
use crate::audio;
use crate::config::{ModelRef, Settings};
use crate::coverage;
use crate::db::{Db, NewSession, StoredAnalysis, StoredNextSteps, now_iso};
use crate::diarize;
use crate::llm::Llm;
use crate::merge::{assign_speakers, default_role_map, merge_tracks, relabel, swap_you_and_interviewer, to_turns};
use crate::metrics;
use crate::models::{INTERVIEWER, Mode, RunStatus, Segment, Session, SessionAnalysis, Source, Status, Step, YOU, fmt_ts,
                    speaker_label};
use crate::next_steps;
use crate::progress::Progress;
use crate::steps::{self, RunProgress};
use crate::transcribe::Transcriber;

/// Dual-track layout: which file belongs to which speaker.
pub const TRACKS: [(&str, &str); 2] = [("mic", YOU), ("system", INTERVIEWER)];

pub struct Transcribed {
    pub segments: Vec<Segment>,
    pub warnings: Vec<String>,
}

fn slug(text: &str) -> String {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(40).collect();
    if s.is_empty() { "session".into() } else { s.trim_end_matches('-').to_string() }
}

fn attach_dir(db: &Db, settings: &Settings, session: &mut Session) -> Result<PathBuf> {
    let dir = settings.sessions_dir().join(format!("{:04}-{}", session.id, slug(&session.title)));
    std::fs::create_dir_all(&dir)?;
    session.dir = dir.display().to_string();
    db.save_session(session)?;
    Ok(dir)
}

fn fail(db: &Db, id: i64, e: anyhow::Error) -> anyhow::Error {
    let _ = db.set_status(id, Status::Failed, Some(format!("{e:#}")));
    e
}

fn set_duration(db: &Db, session: &mut Session, files: &[PathBuf]) -> Result<()> {
    let mut longest = 0.0f64;
    for f in files {
        longest = longest.max(audio::duration_s(&audio::load(f)?));
    }
    session.duration_s = Some(longest);
    db.save_session(session)
}

/// Run one stage as a recorded run, built from the upstream stage's latest success. Progress is
/// mirrored into the run row for the app, and the result (or error) is saved however it ends.
/// `body` gets the run id and returns its value plus the id of the row it produced, if any.
fn run_stage<T>(db: &mut Db, session_id: i64, step: Step, params: serde_json::Value, progress: &mut dyn Progress,
                body: impl FnOnce(&mut Db, i64, &mut dyn Progress) -> Result<(T, Option<i64>)>) -> Result<T> {
    let input = steps::upstream_run(db, session_id, step)?;
    let run = db.start_run(session_id, step, &params, input)?;
    let mut live = RunProgress::new(db, run, progress)?;
    match body(db, run, &mut live) {
        Ok((value, output)) => {
            db.finish_run(run, output)?;
            Ok(value)
        }
        Err(e) => {
            let _ = db.fail_run(run, &format!("{e:#}"));
            Err(e)
        }
    }
}

// --- Stage 1: recording ------------------------------------------------------------------------

/// Single mixed track (Zoom export, voice memo, ...). Needs diarization to tell speakers apart.
pub fn ingest_file(db: &mut Db, settings: &Settings, src: &Path, title: &str, company: Option<String>,
                   num_speakers: Option<i64>, progress: &mut dyn Progress) -> Result<Session> {
    let src = std::fs::canonicalize(src)?;
    let mut session = db.create_session(NewSession {
        title: title.into(),
        company,
        source: Source::Upload,
        mode: Mode::Single,
        source_path: Some(src.display().to_string()),
        num_speakers,
        consent: None,
        status: Status::New,
    })?;
    attach_dir(db, settings, &mut session)?;
    record_stage(db, session.id, vec![src], progress)
}

/// Separate mic (you) and system (interviewer) tracks — speaker labels come for free.
pub fn ingest_tracks(db: &mut Db, settings: &Settings, mic: &Path, system: &Path, title: &str,
                     company: Option<String>, progress: &mut dyn Progress) -> Result<Session> {
    let (mic, system) = (std::fs::canonicalize(mic)?, std::fs::canonicalize(system)?);
    let mut session = db.create_session(NewSession {
        title: title.into(),
        company,
        source: Source::Upload,
        mode: Mode::Dual,
        source_path: mic.parent().map(|p| p.display().to_string()),
        num_speakers: None,
        consent: None,
        status: Status::New,
    })?;
    attach_dir(db, settings, &mut session)?;
    record_stage(db, session.id, vec![mic, system], progress)
}

/// A dual-track session whose folder a recorder (ICRecorder.app or the app itself) records into.
pub fn create_recording_session(db: &Db, settings: &Settings, title: &str, company: Option<String>) -> Result<Session> {
    let mut session = db.create_session(NewSession {
        title: title.into(),
        company,
        source: Source::Recording,
        mode: Mode::Dual,
        source_path: None,
        num_speakers: None,
        consent: Some(true),
        status: Status::Recording,
    })?;
    attach_dir(db, settings, &mut session)?;
    Ok(session)
}

/// The recorder finished: its raw WAVs become the recording stage's sources. They're kept, so
/// the stage can be re-processed later.
pub fn process_recording(db: &mut Db, session_id: i64, mic: &Path, system: &Path, progress: &mut dyn Progress)
    -> Result<Session> {
    record_stage(db, session_id, vec![mic.to_path_buf(), system.to_path_buf()], progress)
}

/// Why the recording stage can't be re-processed, if it can't.
pub fn reprocess_blocker(db: &Db, session_id: i64) -> Result<Option<String>> {
    let Some(run) = db.runs(session_id)?.into_iter().rfind(|r| r.step == Step::Recording) else {
        return Ok(Some("Nothing has been recorded or imported yet.".into()));
    };
    let sources = steps::recording_sources(&run);
    if sources.is_empty() {
        return Ok(Some("The original tracks of this import weren't kept, so the audio can't be rebuilt.".into()));
    }
    Ok(sources.iter().find(|p| !p.exists()).map(|p| format!("The original audio is gone: {}", p.display())))
}

/// Rebuild the normalized audio and listening copy from the original sources.
pub fn reprocess_audio(db: &mut Db, session_id: i64, progress: &mut dyn Progress) -> Result<Session> {
    if let Some(reason) = reprocess_blocker(db, session_id)? {
        bail!("{reason}");
    }
    let run = db.runs(session_id)?.into_iter().rfind(|r| r.step == Step::Recording).expect("checked above");
    record_stage(db, session_id, steps::recording_sources(&run), progress)
}

fn record_stage(db: &mut Db, session_id: i64, sources: Vec<PathBuf>, progress: &mut dyn Progress) -> Result<Session> {
    let params = json!({"sources": sources.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()});
    run_stage(db, session_id, Step::Recording, params, progress, |db, _, progress| {
        let mut session = db.get_session(session_id)?;
        let dir = PathBuf::from(&session.dir);
        let names: &[&str] = match session.mode {
            Mode::Single => &["audio"],
            Mode::Dual => &["mic", "system"],
        };
        if sources.len() != names.len() {
            bail!("expected {} audio source(s), got {}", names.len(), sources.len());
        }
        progress.stage("Converting audio");
        let mut outs = vec![];
        for (name, src) in names.iter().zip(&sources) {
            let dst = dir.join(format!("{name}.flac"));
            audio::normalize(src, &dst)?;
            outs.push(dst);
        }
        set_duration(db, &mut session, &outs)?;
        progress.stage("Making a copy to listen to");
        audio::make_listen_copy(&outs, &dir.join("listen.m4a"))?;
        db.set_status(session_id, Status::New, None)?;
        Ok((db.get_session(session_id)?, None))
    })
    .map_err(|e| fail(db, session_id, e))
}

// --- Stage 2: transcript -----------------------------------------------------------------------

pub fn transcribe_session(db: &mut Db, settings: &Settings, id: i64, progress: &mut dyn Progress) -> Result<Transcribed> {
    let session = db.get_session(id)?;
    db.set_status(id, Status::Transcribing, None)?;
    let params = json!({"speakers": session.num_speakers, "whisper_model": settings.whisper_model});
    run_stage(db, id, Step::Transcript, params, progress, |db, run, progress| {
        let dir = PathBuf::from(&session.dir);
        let result = match session.mode {
            Mode::Single => transcribe_single(settings, &dir, &session, progress),
            Mode::Dual => transcribe_dual(settings, &dir, progress),
        }?;
        db.set_run_warnings(run, &result.warnings)?;
        db.replace_segments(id, &result.segments)?;
        write_transcript_files(&dir, &result.segments)?;
        db.set_status(id, Status::Transcribed, None)?;
        Ok((result, None))
    })
    .map_err(|e| fail(db, id, e))
}

/// Swap "You" and "Interviewer" on a single-track transcript. That's a new transcript revision,
/// so the report and next steps built on the old one become out of date.
pub fn swap_speakers(db: &mut Db, id: i64, progress: &mut dyn Progress) -> Result<()> {
    let session = db.get_session(id)?;
    if session.mode == Mode::Dual {
        bail!("This session has separate tracks, so its labels come from the tracks themselves.");
    }
    run_stage(db, id, Step::Transcript, json!({"kind": "swap"}), progress, |db, _, _| {
        let segments = swap_you_and_interviewer(&db.get_segments(id)?);
        db.replace_segments(id, &segments)?;
        write_transcript_files(Path::new(&session.dir), &segments)?;
        Ok(((), None))
    })
}

fn transcribe_single(settings: &Settings, dir: &Path, session: &Session, progress: &mut dyn Progress) -> Result<Transcribed> {
    let samples = audio::load(&dir.join("audio.flac"))?;
    if audio::is_silent(&samples) {
        bail!("The recording is silent — nothing to transcribe.");
    }
    progress.stage("Detecting speakers");
    let spans = diarize::diarize(&samples, session.num_speakers.map(|n| n as usize), settings, progress)?;
    let transcriber = Transcriber::load(settings, progress)?;
    progress.stage("Transcribing");
    let segments = assign_speakers(&transcriber.transcribe(&samples, progress)?, &spans);
    let mapping = default_role_map(&segments);
    let mut warnings = vec![];
    let expected = session.num_speakers.unwrap_or(2) as usize;
    if mapping.len() < expected {
        warnings.push(format!(
            "Speaker detection found {} voice(s) but {expected} were expected, so some lines may be labelled with \
             the wrong person. Recordings made with `ic record` avoid this (separate tracks).",
            mapping.len()
        ));
    }
    Ok(Transcribed { segments: relabel(&segments, &mapping), warnings })
}

fn transcribe_dual(settings: &Settings, dir: &Path, progress: &mut dyn Progress) -> Result<Transcribed> {
    let transcriber = Transcriber::load(settings, progress)?;
    let mut tracks = vec![];
    let mut warnings = vec![];
    for (name, speaker) in TRACKS {
        let samples = audio::load(&dir.join(format!("{name}.flac")))?;
        if audio::is_silent(&samples) {
            warnings.push(if name == "system" {
                "The interviewer track (system) is silent — check System Audio Recording permission for the recording app."
                    .to_string()
            } else {
                "Your track (mic) is silent — was your mic muted or the wrong input selected?".to_string()
            });
            continue;
        }
        progress.stage(&format!("Transcribing ({})", speaker_label(speaker)));
        tracks.push((speaker, transcriber.transcribe(&samples, progress)?));
    }
    if tracks.is_empty() {
        bail!("Both tracks are silent — nothing to transcribe. {}", warnings.join(" "));
    }
    Ok(Transcribed { segments: merge_tracks(tracks), warnings })
}

pub fn write_transcript_files(dir: &Path, segments: &[Segment]) -> Result<()> {
    std::fs::write(dir.join("transcript.json"), serde_json::to_string_pretty(segments)?)?;
    let md: Vec<String> = to_turns(segments)
        .iter()
        .map(|t| format!("**[{}] {}:** {}\n", fmt_ts(t.start), speaker_label(&t.speaker), t.text))
        .collect();
    std::fs::write(dir.join("transcript.md"), md.join("\n"))?;
    Ok(())
}

// --- Stage 3: after-action report --------------------------------------------------------------

/// Metrics + analysis for a transcribed session. Every run is kept.
/// `model` picks the provider/model; `llm` must be that provider's adapter (see `llm::client`).
pub fn analyze_session(db: &mut Db, llm: &dyn Llm, model: &ModelRef, id: i64, progress: &mut dyn Progress)
    -> Result<StoredAnalysis> {
    let session = db.get_session(id)?;
    if db.get_segments(id)?.is_empty() {
        bail!("Session {id} has no transcript yet — run: ic run transcript {id}");
    }
    db.set_status(id, Status::Analyzing, None)?;
    run_stage(db, id, Step::Report, json!({"model": model.to_string()}), progress, |db, run, progress| {
        let stored = analyze_inner(db, llm, model, session, run, progress)?;
        let analysis_id = stored.id;
        Ok((stored, Some(analysis_id)))
    })
    .map_err(|e| fail(db, id, e))
}

fn analyze_inner(db: &mut Db, llm: &dyn Llm, model: &ModelRef, mut session: Session, report_run: i64,
                 progress: &mut dyn Progress) -> Result<StoredAnalysis> {
    let mut segments = db.get_segments(session.id)?;
    let (mut metrics, mut analysis) = run_analysis(llm, model, &session, &segments, progress)?;
    if analysis.context.labels_swapped && session.mode == Mode::Single {
        // Speaker detection guessed backwards: fix the transcript — a new transcript revision this
        // report is built on — then re-analyse so the metrics describe the right person.
        progress.stage("Speakers were swapped — fixing labels and re-analysing");
        segments = swap_you_and_interviewer(&segments);
        db.replace_segments(session.id, &segments)?;
        write_transcript_files(Path::new(&session.dir), &segments)?;
        let recording = steps::upstream_run(db, session.id, Step::Transcript)?;
        let revision = db.insert_finished_run(session.id, Step::Transcript, RunStatus::Succeeded, &now_iso(),
                                              &json!({"kind": "swap", "by": "analysis"}), recording, None, None)?;
        db.set_run_input(report_run, revision)?;
        (metrics, analysis) = run_analysis(llm, model, &session, &segments, progress)?;
    }
    let unverified = analyze::unverified_quotes(&analysis, &to_turns(&segments));
    // The full provider/model is stored so reports (and calibration) show which model judged each interview.
    let stored = db.add_analysis(session.id, &analysis, &metrics, &model.to_string(), PROMPT_VERSION, &unverified)?;
    session.status = Status::Analyzed;
    session.error = None;
    session.stage = Some(analysis.context.stage);
    session.company = session.company.take().or(analysis.context.company.clone());
    db.save_session(&session)?;
    std::fs::write(Path::new(&session.dir).join("analysis.json"), serde_json::to_string_pretty(&stored)?)?;
    Ok(stored)
}

fn run_analysis(llm: &dyn Llm, model: &ModelRef, session: &Session, segments: &[Segment], progress: &mut dyn Progress)
    -> Result<(metrics::TalkMetrics, SessionAnalysis)> {
    let metrics = metrics::compute(segments, session.mode);
    let notes = coverage::recording_notes(Path::new(&session.dir), session.mode);
    let message = analyze::build_user_message(&to_turns(segments), &metrics, &session.title, session.company.as_deref(),
                                              session.mode, None, &notes);
    progress.stage(&format!("{model} is reading the transcript…"));
    let analysis = analyze::analyze(llm, &model.name, &message, &mut |chars| {
        if chars > 0 {
            progress.stage(&format!("{model} is writing the analysis… ({} KB)", chars / 1024));
        }
    })?;
    Ok((metrics, analysis))
}

// --- Stage 4: what to do next ------------------------------------------------------------------

/// The analysis behind the current report, if there is one.
pub fn current_report(db: &Db, id: i64) -> Result<Option<StoredAnalysis>> {
    let Some(run) = steps::current_run(db, id, Step::Report)? else { return Ok(None) };
    Ok(match run.output_id {
        Some(analysis_id) => db.analysis_by_id(analysis_id)?,
        None => db.latest_analysis(id)?,
    })
}

/// Next steps default to the model that produced the report they build on.
pub fn report_model(db: &Db, id: i64) -> Result<Option<ModelRef>> {
    Ok(current_report(db, id)?.and_then(|a| a.model.parse().ok()))
}

/// Next-round prep and a practice plan, built from the current report. Every run is kept.
pub fn plan_next_steps(db: &mut Db, llm: &dyn Llm, model: &ModelRef, id: i64, progress: &mut dyn Progress)
    -> Result<StoredNextSteps> {
    let session = db.get_session(id)?;
    let report = current_report(db, id)?
        .with_context(|| format!("Session {id} has no after-action report yet — run: ic run report {id}"))?;
    let turns = to_turns(&db.get_segments(id)?);
    let outcome = db.get_outcome(id)?;
    run_stage(db, id, Step::Next, json!({"model": model.to_string()}), progress, |db, _, progress| {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let notes = coverage::recording_notes(Path::new(&session.dir), session.mode);
        let message = next_steps::build_user_message(&session, &report, outcome.as_ref(), &turns, &today, &notes);
        progress.stage(&format!("{model} is planning what to do next…"));
        let plan = next_steps::generate(llm, &model.name, &message, &mut |chars| {
            if chars > 0 {
                progress.stage(&format!("{model} is writing the plan… ({} KB)", chars / 1024));
            }
        })?;
        let unverified = next_steps::unverified_quotes(&plan, &turns);
        let stored = db.add_next_steps(id, &plan, &model.to_string(), next_steps::PROMPT_VERSION, Some(report.id),
                                       &unverified)?;
        std::fs::write(Path::new(&session.dir).join("next-steps.json"), serde_json::to_string_pretty(&stored)?)?;
        let next_id = stored.id;
        Ok((stored, Some(next_id)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slug("Northwind PM (strong)"), "northwind-pm-strong");
        assert_eq!(slug("!!!"), "session");
    }
}
