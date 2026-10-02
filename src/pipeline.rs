//! Session processing: files in, labelled transcript and analysis out.
//!
//! Session folders hold normalized audio (`audio.flac` for single-track uploads, `mic.flac` +
//! `system.flac` for dual-track) plus human-readable exports. The database is the source of truth.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::analyze::{self, PROMPT_VERSION};
use crate::audio;
use crate::config::{ModelRef, Settings};
use crate::llm::Llm;
use crate::db::{Db, NewSession, StoredAnalysis};
use crate::diarize;
use crate::merge::{assign_speakers, default_role_map, merge_tracks, relabel, swap_you_and_interviewer, to_turns};
use crate::metrics;
use crate::models::{INTERVIEWER, Mode, Segment, Session, SessionAnalysis, Source, Status, YOU, fmt_ts, speaker_label};
use crate::progress::Progress;
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

/// Single mixed track (Zoom export, voice memo, ...). Needs diarization to tell speakers apart.
pub fn ingest_file(db: &Db, settings: &Settings, src: &Path, title: &str, company: Option<String>,
                   num_speakers: Option<i64>) -> Result<Session> {
    let mut session = db.create_session(NewSession {
        title: title.into(),
        company,
        source: Source::Upload,
        mode: Mode::Single,
        source_path: Some(std::fs::canonicalize(src)?.display().to_string()),
        num_speakers,
        consent: None,
        status: Status::New,
    })?;
    let dir = attach_dir(db, settings, &mut session)?;
    let flac = dir.join("audio.flac");
    audio::normalize(src, &flac).map_err(|e| fail(db, session.id, e))?;
    set_duration(db, &mut session, &[flac])?;
    Ok(session)
}

/// Separate mic (you) and system (interviewer) tracks — speaker labels come for free.
pub fn ingest_tracks(db: &Db, settings: &Settings, mic: &Path, system: &Path, title: &str,
                     company: Option<String>) -> Result<Session> {
    let mut session = db.create_session(NewSession {
        title: title.into(),
        company,
        source: Source::Upload,
        mode: Mode::Dual,
        source_path: std::fs::canonicalize(mic)?.parent().map(|p| p.display().to_string()),
        num_speakers: None,
        consent: None,
        status: Status::New,
    })?;
    attach_dir(db, settings, &mut session)?;
    normalize_tracks(db, &mut session, mic, system)?;
    Ok(session)
}

/// A dual-track session whose folder ICRecorder.app records straight into.
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

/// Convert the recorder's (or an upload's) tracks to 16 kHz FLAC in the session folder.
pub fn normalize_tracks(db: &Db, session: &mut Session, mic: &Path, system: &Path) -> Result<()> {
    let dir = PathBuf::from(&session.dir);
    let mut outs = vec![];
    for ((name, _), src) in TRACKS.iter().zip([mic, system]) {
        let dst = dir.join(format!("{name}.flac"));
        audio::normalize(src, &dst).map_err(|e| fail(db, session.id, e))?;
        outs.push(dst);
    }
    set_duration(db, session, &outs)?;
    db.set_status(session.id, Status::New, None)?;
    session.status = Status::New;
    Ok(())
}

pub fn transcribe_session(db: &mut Db, settings: &Settings, id: i64, progress: &mut dyn Progress) -> Result<Transcribed> {
    let session = db.get_session(id)?;
    db.set_status(id, Status::Transcribing, None)?;
    let dir = PathBuf::from(&session.dir);
    let result = match session.mode {
        Mode::Single => transcribe_single(settings, &dir, &session, progress),
        Mode::Dual => transcribe_dual(settings, &dir, progress),
    };
    let result = result.map_err(|e| fail(db, id, e))?;
    db.replace_segments(id, &result.segments)?;
    write_transcript_files(&dir, &result.segments)?;
    db.set_status(id, Status::Transcribed, None)?;
    Ok(result)
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
                "The interviewer track (system) is silent — check System Audio Recording permission for ICRecorder.".to_string()
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

/// Metrics + Claude analysis for a transcribed session. Stores a new analysis row each run.
/// `model` picks the provider/model; `llm` must be that provider's adapter (see `llm::client`).
pub fn analyze_session(db: &mut Db, llm: &dyn Llm, model: &ModelRef, id: i64, progress: &mut dyn Progress)
    -> Result<StoredAnalysis> {
    let session = db.get_session(id)?;
    if db.get_segments(id)?.is_empty() {
        bail!("Session {id} has no transcript yet — run: ic transcribe {id}");
    }
    db.set_status(id, Status::Analyzing, None)?;
    analyze_inner(db, llm, model, session, progress).map_err(|e| fail(db, id, e))
}

fn analyze_inner(db: &mut Db, llm: &dyn Llm, model: &ModelRef, mut session: Session, progress: &mut dyn Progress)
    -> Result<StoredAnalysis> {
    let mut segments = db.get_segments(session.id)?;
    let (mut metrics, mut analysis) = run_analysis(llm, model, &session, &segments, progress)?;
    if analysis.context.labels_swapped && session.mode == Mode::Single {
        // Speaker detection guessed backwards: fix the transcript, then re-analyse so the metrics
        // describe the right person.
        progress.stage("Speakers were swapped — fixing labels and re-analysing");
        segments = swap_you_and_interviewer(&segments);
        db.replace_segments(session.id, &segments)?;
        write_transcript_files(Path::new(&session.dir), &segments)?;
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
    let message = analyze::build_user_message(&to_turns(segments), &metrics, &session.title, session.company.as_deref(),
                                              session.mode, None);
    progress.stage(&format!("{model} is reading the transcript…"));
    let analysis = analyze::analyze(llm, &model.name, &message, &mut |chars| {
        if chars > 0 {
            progress.stage(&format!("{model} is writing the analysis… ({} KB)", chars / 1024));
        }
    })?;
    Ok((metrics, analysis))
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
