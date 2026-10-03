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
use crate::scoring::{self, Scorer};
use crate::progress::Progress;
use crate::prosody;
use crate::steps::{self, RunProgress};
use crate::temperature;
use crate::versions::{self, CachedScorer, Manifest};
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
        db.save_transcript_revision(run, id, &result.segments)?;
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
    run_stage(db, id, Step::Transcript, json!({"kind": "swap"}), progress, |db, run, _| {
        let segments = swap_you_and_interviewer(&db.get_segments(id)?);
        db.replace_segments(id, &segments)?;
        db.save_transcript_revision(run, id, &segments)?;
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
    analyze_session_with(db, llm, model, id, ReportExtras::default(), progress)
}

/// What the Report stage adds after Claude's report.
#[derive(Clone, Copy, Default)]
pub struct ReportExtras<'a> {
    /// Checks each of your answers (off unless the `scorer` setting picks one).
    pub checker: Option<&'a dyn Scorer>,
    /// Judges what the interviewer says, turn by turn, for the temperature timeline (Jev, when its
    /// key exists). The timeline is always built; without this it's voice-only.
    pub timeline: Option<&'a dyn Scorer>,
}

/// The report, then (with a `checker`) the built-in checks on each of the candidate's answers,
/// then the room's temperature timeline. Failures after the report only leave warnings on the run.
pub fn analyze_session_with(db: &mut Db, llm: &dyn Llm, model: &ModelRef, id: i64, extras: ReportExtras,
                            progress: &mut dyn Progress) -> Result<StoredAnalysis> {
    let session = db.get_session(id)?;
    if db.get_segments(id)?.is_empty() {
        bail!("Session {id} has no transcript yet — run: ic run transcript {id}");
    }
    db.set_status(id, Status::Analyzing, None)?;
    let mut params = json!({"model": model.to_string()});
    if let Some(checker) = extras.checker {
        params["checks"] = json!(checker.name());
    }
    if let Some(timeline) = extras.timeline {
        params["timeline"] = json!(timeline.name());
    }
    run_stage(db, id, Step::Report, params, progress, |db, run, progress| {
        let parent = current_report(db, id)?.map(|r| r.id);
        let mut warnings = vec![];
        let (metrics, message) = report_message(&session, &db.get_segments(id)?);
        let key = versions::report_key(model, &message);
        let (stored, reused) = match db.analysis_by_key(id, &key)? {
            Some(existing) => {
                // Same inputs as an earlier version: show it again rather than asking for a new answer.
                let number = db.analyses(id)?.iter().filter(|a| a.id <= existing.id).count();
                progress.stage(&format!("Nothing has changed since version {number}, so it's shown again"));
                let mut s = db.get_session(id)?;
                (s.status, s.error) = (Status::Analyzed, None);
                db.save_session(&s)?;
                (existing, true)
            }
            None => {
                let (stored, notes) = analyze_inner(db, llm, model, session, run, parent, (metrics, message), progress)?;
                warnings.extend(notes);
                (stored, false)
            }
        };
        let checker = extras.checker.map(|inner| CachedScorer { inner, db: &*db });
        let timeline = extras.timeline.map(|inner| CachedScorer { inner, db: &*db });
        if let Some(checker) = &checker
            && stored.answer_checks.is_empty()
        {
            let answers = scoring::answers_from_report(&to_turns(&db.get_segments(id)?), &stored.analysis.questions);
            if !answers.is_empty() {
                progress.stage(&format!("Checking your {} answers one by one…", answers.len()));
                match scoring::check_answers(checker, &answers) {
                    Ok(rows) => db.add_answer_checks(id, stored.id, &rows)?,
                    Err(e) => warnings.push(format!("The answer-by-answer checks were skipped: {e:#}")),
                }
            }
        }
        // Built from the final transcript: a single-track report may just have swapped the speakers.
        // A reused version keeps its timeline; one made before timelines existed gets one now.
        if stored.turn_signals.is_empty() {
            let session = db.get_session(id)?;
            warnings.extend(add_timeline(db, &session, stored.id, timeline.as_ref().map(|t| t as &dyn Scorer), progress)?);
        }
        db.set_run_warnings(run, &warnings)?;
        let stored = db.analysis_by_id(stored.id)?.unwrap_or(stored);
        if !reused || stored.inputs.is_none() {
            let inputs = manifest(db, &db.get_session(id)?, model, stored.timeline_scorer.clone(),
                                  stored.answer_checks.first().map(|c| c.scorer.clone()))?;
            db.set_analysis_inputs(stored.id, &inputs)?;
        }
        let stored = db.analysis_by_id(stored.id)?.unwrap_or(stored);
        let analysis_id = stored.id;
        Ok((stored, Some(analysis_id)))
    })
    .map_err(|e| fail(db, id, e))
}

/// The room's temperature timeline for a stored report: Jev's checks on each substantive
/// interviewer turn (with a `scorer`) and voices measured from the session's audio. Returns
/// warnings; only a database error fails it.
fn add_timeline(db: &Db, session: &Session, analysis_id: i64, scorer: Option<&dyn Scorer>, progress: &mut dyn Progress)
    -> Result<Vec<String>> {
    let convo = temperature::conversation(&db.get_segments(session.id)?);
    let turns = convo.iter().filter(|t| t.kind == temperature::Kind::Substantive).count();
    if turns == 0 {
        return Ok(vec![]);
    }
    let mut warnings = vec![];
    let assessments = match scorer {
        Some(scorer) => {
            progress.stage(&format!("Reading the room: {turns} things the interviewer said…"));
            let (assessments, errors) = temperature::assess_turns(scorer, &convo);
            if let Some(first) = errors.first() {
                warnings.push(format!("The room's timeline is missing the words of {} of the interviewer's {turns} turns: {first}",
                                      turns - assessments.len()));
            }
            assessments
        }
        None => Default::default(),
    };
    progress.stage("Measuring how everyone sounded…");
    let audio = match timeline_audio(session) {
        Ok(audio) => Some(audio),
        Err(e) => {
            warnings.push(format!("The room's timeline couldn't read the audio, so it leaves out how people sounded: {e:#}"));
            None
        }
    };
    let signals = temperature::build(&convo, &assessments, audio.as_ref());
    let scorer_name = assessments.values().next().map(|a| a.scorer.clone());
    db.set_turn_signals(session.id, analysis_id, &signals, scorer_name.as_deref())?;
    Ok(warnings)
}

/// Rebuild the room's timeline for the current report without asking Claude again: for reports
/// made before the timeline existed, or after adding a TypeSafe key. Returns the report and warnings.
pub fn refresh_timeline(db: &mut Db, id: i64, scorer: Option<&dyn Scorer>, progress: &mut dyn Progress)
    -> Result<(StoredAnalysis, Vec<String>)> {
    let report = current_report(db, id)?
        .with_context(|| format!("Session {id} has no after-action report yet — run: ic run report {id}"))?;
    // Through the stored verdicts, like the Report stage: the same turn always gets the same verdict.
    let cached = scorer.map(|inner| CachedScorer { inner, db: &*db });
    let warnings = add_timeline(db, &db.get_session(id)?, report.id, cached.as_ref().map(|c| c as &dyn Scorer), progress)?;
    Ok((db.analysis_by_id(report.id)?.unwrap_or(report), warnings))
}

/// Pitch and loudness frames for each voice: the interviewer's track and yours, or the one mixed
/// track for both.
fn timeline_audio(session: &Session) -> Result<temperature::Audio> {
    let dir = Path::new(&session.dir);
    let track = |name: &str| -> Result<Vec<prosody::Frame>> {
        let samples = audio::load(&dir.join(format!("{name}.flac")))?;
        Ok(prosody::frames(&samples, prosody::noise_floor_db(&samples)))
    };
    Ok(match session.mode {
        Mode::Dual => temperature::Audio { interviewer: track("system")?, you: track("mic")?, separate: true },
        Mode::Single => {
            let frames = track("audio")?;
            temperature::Audio { interviewer: frames.clone(), you: frames, separate: false }
        }
    })
}

/// What a report built now, from the live transcript, is built from.
fn manifest(db: &Db, session: &Session, model: &ModelRef, timeline_scorer: Option<String>, answer_checker: Option<String>)
    -> Result<Manifest> {
    let segments = db.get_segments(session.id)?;
    Ok(Manifest {
        key: versions::report_key(model, &report_message(session, &segments).1),
        transcript_run_id: steps::current_run(db, session.id, Step::Transcript)?.map(|r| r.id).unwrap_or_default(),
        transcript_sha: versions::transcript_sha(&segments),
        title: session.title.clone(),
        model: model.to_string(),
        prompt_version: PROMPT_VERSION.into(),
        timeline_method: temperature::METHOD.into(),
        timeline_scorer,
        answer_checker,
        app_version: env!("CARGO_PKG_VERSION").into(),
    })
}

/// The analysis message for a transcript, and the call stats in it: everything a report is built from.
fn report_message(session: &Session, segments: &[Segment]) -> (metrics::TalkMetrics, String) {
    let metrics = metrics::compute(segments, session.mode);
    let notes = coverage::recording_notes(Path::new(&session.dir), session.mode);
    let message = analyze::build_user_message(&to_turns(segments), &metrics, &session.title, session.company.as_deref(),
                                              session.mode, None, &notes);
    (metrics, message)
}

#[allow(clippy::too_many_arguments)]
fn analyze_inner(db: &mut Db, llm: &dyn Llm, model: &ModelRef, mut session: Session, report_run: i64, parent: Option<i64>,
                 prepared: (metrics::TalkMetrics, String), progress: &mut dyn Progress)
    -> Result<(StoredAnalysis, Vec<String>)> {
    let mut segments = db.get_segments(session.id)?;
    let (mut metrics, message) = prepared;
    let mut analysis = run_analysis(llm, model, &message, progress)?;
    let mut warnings = vec![];
    // Speakers are swapped automatically only on a fresh transcript. Once a swap has been made (by
    // a report or by you), another model disagreeing doesn't flip the transcript back: every version
    // would then be built on a different one.
    let already_swapped = steps::current_run(db, session.id, Step::Transcript)?.is_some_and(|r| r.params["kind"] == "swap");
    if analysis.context.labels_swapped && session.mode == Mode::Single && already_swapped {
        warnings.push(format!("{model} thinks You and Interviewer are the wrong way round in this transcript. It was \
                               already swapped once, so it was left as it is: if the labels look wrong, use Swap Speakers."));
    } else if analysis.context.labels_swapped && session.mode == Mode::Single {
        // Speaker detection guessed backwards: fix the transcript — a new transcript revision this
        // report is built on — then re-analyse so the metrics describe the right person.
        progress.stage("Speakers were swapped — fixing labels and re-analysing");
        segments = swap_you_and_interviewer(&segments);
        db.replace_segments(session.id, &segments)?;
        write_transcript_files(Path::new(&session.dir), &segments)?;
        let recording = steps::upstream_run(db, session.id, Step::Transcript)?;
        let revision = db.insert_finished_run(session.id, Step::Transcript, RunStatus::Succeeded, &now_iso(),
                                              &json!({"kind": "swap", "by": "analysis"}), recording, None, None)?;
        db.save_transcript_revision(revision, session.id, &segments)?;
        db.set_run_input(report_run, revision)?;
        let (swapped_metrics, swapped_message) = report_message(&session, &segments);
        metrics = swapped_metrics;
        analysis = run_analysis(llm, model, &swapped_message, progress)?;
    }
    let unverified = analyze::unverified_quotes(&analysis, &to_turns(&segments));
    // The full provider/model is stored so reports (and calibration) show which model judged each interview.
    // Its inputs go in with it, so even a run interrupted later can be reused; the scorers are added after.
    let inputs = manifest(db, &session, model, None, None)?;
    let stored = db.add_analysis(session.id, &analysis, &metrics, &model.to_string(), PROMPT_VERSION, &unverified,
                                 Some(&inputs), parent)?;
    session.status = Status::Analyzed;
    session.error = None;
    session.stage = Some(analysis.context.stage);
    db.save_session(&session)?;
    std::fs::write(Path::new(&session.dir).join("analysis.json"), serde_json::to_string_pretty(&stored)?)?;
    Ok((stored, warnings))
}

fn run_analysis(llm: &dyn Llm, model: &ModelRef, message: &str, progress: &mut dyn Progress) -> Result<SessionAnalysis> {
    progress.stage(&format!("{model} is reading the transcript…"));
    analyze::analyze(llm, &model.name, message, &mut |chars| {
        if chars > 0 {
            progress.stage(&format!("{model} is writing the analysis… ({} KB)", chars / 1024));
        }
    })
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
