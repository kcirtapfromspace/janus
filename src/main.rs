//! `ic` — the Interview Coach command line.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use interview_coach::auth;
use interview_coach::{errln, out, outln};
use interview_coach::capture;
use interview_coach::config::{FileConfig, ModelRef, Provider, ScorerRef, Settings};
use interview_coach::db::Db;
use interview_coach::events::JsonEvents;
use interview_coach::llm;
use interview_coach::merge::to_turns;
use interview_coach::models::{OutcomeResult, Status, Step, YOU, fmt_ts, speaker_label};
use interview_coach::pipeline;
use interview_coach::progress::Progress;
use interview_coach::proxy::{self, KeyTarget, LlmEndpoint};
use interview_coach::report;
use interview_coach::scoring::{ClaudeScorer, JevScorer, Scorer};
use interview_coach::session_view;
use interview_coach::setup;
use interview_coach::steps::{self, StageStatus};
use interview_coach::tools::{Origin, Tool};

#[derive(Parser)]
#[command(name = "ic", about = "Interview Coach — record, transcribe, and get coached on your interviews.", version)]
struct Cli {
    /// Model for analysis, e.g. anthropic/claude-opus-5-5 or openai/gpt-5.6 (overrides the configured default).
    #[arg(long, global = true, value_name = "PROVIDER/MODEL")]
    model: Option<ModelRef>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record an interview from any app (your mic + the call's audio) until you press Ctrl+C.
    Record {
        /// Name for this session.
        #[arg(long)]
        title: Option<String>,
        /// Company you're interviewing with.
        #[arg(long)]
        company: Option<String>,
        /// Echo cancellation on your mic (if you're not using headphones).
        #[arg(long)]
        aec: bool,
        /// Stop automatically after this many seconds.
        #[arg(long)]
        duration: Option<u32>,
        /// Skip the consent question.
        #[arg(long, short)]
        yes: bool,
        /// Don't analyse when the recording ends.
        #[arg(long)]
        no_analyze: bool,
    },
    /// Stop a recording that's still running (e.g. if its terminal was closed).
    Stop {
        #[arg(long)]
        no_analyze: bool,
    },
    /// Used by the Interview Coach app, which records in-process and hands the result to ic.
    #[command(hide = true)]
    Recording {
        #[command(subcommand)]
        action: RecordingCmd,
    },
    /// Import an interview recording (audio or video) and transcribe it.
    Import {
        /// File with the whole interview.
        path: Option<PathBuf>,
        /// Your mic track (for separate-track recordings).
        #[arg(long, requires = "system", conflicts_with = "path")]
        mic: Option<PathBuf>,
        /// The interviewer's track (for separate-track recordings).
        #[arg(long, requires = "mic")]
        system: Option<PathBuf>,
        /// Name for this session (defaults to the file name).
        #[arg(long)]
        title: Option<String>,
        /// Company you interviewed with.
        #[arg(long)]
        company: Option<String>,
        /// People on the call, including you (single-file imports).
        #[arg(long, default_value_t = 2)]
        speakers: i64,
        /// Only import; don't transcribe or analyse yet.
        #[arg(long)]
        no_transcribe: bool,
        /// Transcribe but don't analyse.
        #[arg(long)]
        no_analyze: bool,
    },
    /// (Re)run transcription for a session.
    Transcribe { id: i64 },
    /// List your interview sessions.
    List {
        /// Machine-readable output (used by the Interview Coach app).
        #[arg(long)]
        json: bool,
    },
    /// Print a session's transcript.
    Transcript {
        id: i64,
        /// Print segments as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show an interview's four stages (recording, transcript, report, next) and where each stands.
    Steps {
        id: i64,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Re-run one stage: recording, transcript, report, or next. Later stages built on it go out of date.
    Run {
        #[arg(value_parser = parse_step)]
        step: Step,
        id: i64,
        /// People on the call, including you (transcript of a single-track recording).
        #[arg(long)]
        speakers: Option<i64>,
        /// Also update every later stage that's out of date.
        #[arg(long)]
        then_later: bool,
    },
    /// Show what to do next: next-round prep and a practice plan.
    Next { id: i64 },
    /// Everything about one session as JSON (used by the Interview Coach app).
    #[command(hide = true)]
    Session { id: i64 },
    /// Swap 'You' and 'Interviewer' labels if speaker detection guessed wrong.
    Swap { id: i64 },
    /// Analyse a transcribed session with Claude (re-running keeps earlier analyses).
    Analyze {
        id: i64,
        /// Open the HTML report in your browser.
        #[arg(long)]
        open: bool,
        /// Show every question's breakdown.
        #[arg(long)]
        full: bool,
    },
    /// Show the latest analysis for a session.
    Report {
        id: i64,
        #[arg(long)]
        open: bool,
        #[arg(long)]
        full: bool,
    },
    /// Rebuild how the room felt for an analysed interview (no new Claude analysis; uses Jev when
    /// its key is set).
    Timeline { id: i64 },
    /// Record how an interview actually turned out — coaching learns from real results.
    Outcome {
        id: i64,
        /// pending, advanced, offer, rejected, withdrew, or no_response.
        #[arg(value_parser = parse_outcome)]
        result: OutcomeResult,
        /// Anything the recruiter or interviewer told you.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Sign in to Claude through your browser (no API key needed).
    Login {
        /// JSON-lines events for the app (the sign-in URL, a code prompt); a code is read from stdin.
        #[arg(long, hide = true)]
        events: bool,
    },
    /// What this Mac still needs before Interview Coach works, and the steps that set it up.
    Setup {
        #[command(subcommand)]
        action: SetupCmd,
    },
    /// Run and manage the local LiteLLM proxy that brokers every LLM request.
    Proxy {
        #[command(subcommand)]
        action: ProxyCmd,
    },
    /// Show or change settings (~/InterviewCoach/config.toml).
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Check that everything the pipeline needs is installed and set up.
    Doctor,
    /// Compare answer scorers (Jev and Claude) on the labelled answers.
    #[command(hide = true)]
    Eval {
        #[command(subcommand)]
        action: EvalCmd,
    },
    /// TypeSafe's Jev, through the AI proxy.
    #[command(hide = true)]
    Jev {
        #[command(subcommand)]
        action: JevCmd,
    },
}

#[derive(Subcommand)]
enum EvalCmd {
    /// Score every labelled item with each arm, then apply the set's rule in docs/eval/.
    Scorers {
        /// What to score: answers (your answers' checks) or interviewer (the timeline's turn checks).
        #[arg(long, value_enum, default_value_t = EvalSet::Answers)]
        set: EvalSet,
        /// The labelled items (default: tests/fixtures/answers.jsonl or interviewer_turns.jsonl).
        #[arg(long)]
        items: Option<PathBuf>,
        /// Any of: jev, haiku, sonnet, opus.
        #[arg(long, value_delimiter = ',', default_value = "jev,haiku,sonnet,opus")]
        arms: Vec<String>,
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Where results go (default: dist/eval for answers, dist/eval/interviewer otherwise).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Only the first N items (for a quick check).
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EvalSet {
    Answers,
    Interviewer,
}

impl EvalSet {
    fn check_set(self) -> interview_coach::scoring::CheckSet {
        match self {
            EvalSet::Answers => interview_coach::scoring::answer_set(),
            EvalSet::Interviewer => interview_coach::temperature::interviewer_set(),
        }
    }

    fn default_items(self) -> PathBuf {
        PathBuf::from(match self {
            EvalSet::Answers => "tests/fixtures/answers.jsonl",
            EvalSet::Interviewer => "tests/fixtures/interviewer_turns.jsonl",
        })
    }

    fn default_out(self) -> PathBuf {
        PathBuf::from(match self {
            EvalSet::Answers => "dist/eval",
            EvalSet::Interviewer => "dist/eval/interviewer",
        })
    }
}

#[derive(Subcommand)]
enum JevCmd {
    /// Send one tiny question, to check the key and the proxy route.
    Ping,
}

#[derive(Subcommand)]
enum SetupCmd {
    /// What's done and what's left.
    Status {
        /// Machine-readable (used by the app's Setup window).
        #[arg(long)]
        json: bool,
    },
    /// Do one setup step: docker, proxy, models, or all of them.
    Run {
        step: setup::Step,
        /// JSON-lines progress events for the app.
        #[arg(long)]
        events: bool,
    },
}

#[derive(Subcommand)]
enum RecordingCmd {
    /// Create a recording session (consent is confirmed in the app); prints {"id", "dir"} as JSON.
    Begin {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        company: Option<String>,
    },
    /// Process a recording the app has finished: normalize, transcribe, and analyse.
    Finish {
        id: i64,
        #[arg(long)]
        no_analyze: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show the settings in effect and where the config file lives.
    Show,
    /// Set a value in the config file (validated before it's saved).
    Set { key: ConfigKey, value: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum ConfigKey {
    /// Default model for analysis, e.g. openai/gpt-5.6.
    Model,
    /// Interview language for Whisper ("en", "de", …) or "auto".
    Language,
    /// whisper.cpp model, e.g. large-v3-turbo or large-v3-turbo-q5_0.
    WhisperModel,
    /// Who checks each answer: typesafe/jev-latest, anthropic/<model>, or off.
    Scorer,
}

#[derive(Subcommand)]
enum ProxyCmd {
    /// Set up (or repair) the proxy: write its config, start it, and give ic its own key.
    Setup,
    /// Add or replace an API key the proxy holds: openai, or typesafe (for Jev). Claude uses `ic login`.
    Key {
        target: KeyTarget,
        /// Read the key from stdin instead of prompting (the app pipes it; it never appears in arguments).
        #[arg(long)]
        stdin: bool,
    },
    /// Start the proxy containers.
    Start,
    /// Stop the proxy containers (your keys and spend history are kept).
    Stop,
    /// Show whether the proxy is up and what ic has spent through it.
    Status,
}

fn parse_step(s: &str) -> Result<Step, String> {
    s.parse().map_err(|_| "expected one of: recording, transcript, report, next".to_string())
}

fn parse_outcome(s: &str) -> Result<OutcomeResult, String> {
    s.parse().map_err(|_| {
        let all: Vec<_> = OutcomeResult::ALL.iter().map(|o| o.as_str()).collect();
        format!("expected one of: {}", all.join(", "))
    })
}

/// Spinner / progress bar for long steps.
struct Ui {
    bar: ProgressBar,
    last_step: u64,
}

impl Ui {
    fn new() -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        bar.enable_steady_tick(Duration::from_millis(100));
        Ui { bar, last_step: 0 }
    }

    fn finish(self) {
        self.bar.finish_and_clear();
    }
}

impl Progress for Ui {
    fn stage(&mut self, message: &str) {
        self.bar.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        self.bar.set_message(message.to_string());
        self.last_step = 0;
    }

    fn step(&mut self, done: u64, total: u64) {
        if total == 0 || (done < total && done.saturating_sub(self.last_step) < total / 200) {
            return;
        }
        self.last_step = done;
        let template = if total > 10_000 {
            "{spinner:.cyan} {msg} [{bar:30.cyan/dim}] {bytes}/{total_bytes}"
        } else {
            "{spinner:.cyan} {msg} [{bar:30.cyan/dim}] {percent}%"
        };
        self.bar.set_style(ProgressStyle::with_template(template).unwrap().progress_chars("━╸─"));
        self.bar.set_length(total);
        self.bar.set_position(done);
    }
}

fn warn(msg: &str) {
    errln!("{} {msg}", style("Warning:").red().bold());
}

fn open_db(settings: &Settings) -> Result<Db> {
    Db::open(&settings.db_path())
}

fn run_transcription(db: &mut Db, settings: &Settings, id: i64) -> Result<()> {
    let mut ui = Ui::new();
    let result = pipeline::transcribe_session(db, settings, id, &mut ui);
    ui.finish();
    let result = result?;
    for w in &result.warnings {
        warn(w);
    }
    outln!("{} {} turns. View it with {}", style(format!("Transcribed session {id}:")).green(),
             to_turns(&result.segments).len(), style(format!("ic transcript {id}")).bold());
    Ok(())
}

/// Why the default model can't be used with the current setup, if it can't.
fn analysis_blocker(settings: &Settings) -> Option<String> {
    blocker_for(settings, &settings.model)
}

/// Why `model` can't be used with the current setup, if it can't.
fn blocker_for(settings: &Settings, model: &ModelRef) -> Option<String> {
    if LlmEndpoint::load(settings).is_none() {
        return Some("the AI proxy isn't set up yet — open Setup in the app (or run: ic proxy setup)".into());
    }
    match model.provider {
        Provider::Anthropic => (!auth::has_login())
            .then(|| "you're not signed in to Claude — sign in from Setup in the app (or run: ic login)".to_string()),
        Provider::OpenAi => (!proxy::using_external_proxy() && !proxy::has_key(settings, KeyTarget::OpenAi))
            .then(|| format!("the proxy has no OpenAI key, which {model} needs — add one in Setup (or: ic proxy key openai)")),
    }
}

fn run_analysis(db: &mut Db, settings: &Settings, model: &ModelRef, id: i64, open: bool, full: bool) -> Result<()> {
    if let Some(reason) = blocker_for(settings, model) {
        bail!("Can't analyse yet: {reason}\nThen run: ic run report {id}");
    }
    let mut ui = Ui::new();
    if let Err(e) = proxy::ensure_running(settings, &mut ui) {
        ui.finish();
        return Err(e);
    }
    let endpoint = LlmEndpoint::load(settings).expect("checked above");
    let client = llm::client(model, endpoint.clone());
    // The answer-by-answer checks, when their scorer is available, and Jev for the room's timeline
    // whenever its key is there.
    let jev_client = interview_coach::llm::jev::Client::new(endpoint.clone());
    let has_jev = proxy::using_external_proxy() || proxy::has_key(settings, KeyTarget::TypeSafe);
    let timeline_scorer = JevScorer { client: &jev_client, model: interview_coach::llm::jev::DEFAULT_MODEL.into() };
    let (jev_scorer, claude_llm);
    let claude_scorer;
    let checker: Option<&dyn Scorer> = match &settings.scorer {
        ScorerRef::Jev(m) if has_jev => {
            jev_scorer = JevScorer { client: &jev_client, model: m.clone() };
            Some(&jev_scorer)
        }
        ScorerRef::Claude(m) if auth::has_login() => {
            claude_llm = llm::client(&ModelRef { provider: Provider::Anthropic, name: m.clone() }, endpoint);
            claude_scorer = ClaudeScorer { llm: claude_llm.as_ref(), model: m.clone(), effort: llm::Effort::Low };
            Some(&claude_scorer)
        }
        _ => None,
    };
    let extras = pipeline::ReportExtras { checker, timeline: has_jev.then_some(&timeline_scorer as &dyn Scorer) };
    let stored = pipeline::analyze_session_with(db, client.as_ref(), model, id, extras, &mut ui);
    ui.finish();
    let stored = stored?;
    let session = db.get_session(id)?;
    let outcome = db.get_outcome(id)?;
    report::write_analysis_html(&session, &stored, outcome.as_ref())?;
    let path = report::write_html(&session, &stored, outcome.as_ref())?;
    report::print_report(&session, &stored, outcome.as_ref(), full);
    if open {
        Command::new("open").arg(path).status()?;
    }
    Ok(())
}

/// Stage 4: next-round prep and a practice plan from the current report.
fn run_next(db: &mut Db, settings: &Settings, model: &ModelRef, id: i64, show: bool) -> Result<()> {
    if let Some(reason) = blocker_for(settings, model) {
        bail!("Can't plan next steps yet: {reason}\nThen run: ic run next {id}");
    }
    let mut ui = Ui::new();
    if let Err(e) = proxy::ensure_running(settings, &mut ui) {
        ui.finish();
        return Err(e);
    }
    let endpoint = LlmEndpoint::load(settings).expect("checked above");
    let client = llm::client(model, endpoint);
    let stored = pipeline::plan_next_steps(db, client.as_ref(), model, id, &mut ui);
    ui.finish();
    let stored = stored?;
    if show {
        report::print_next_steps(&db.get_session(id)?, &stored);
    }
    Ok(())
}

/// The rest of the pipeline after the recording stage: transcript, report, what to do next.
fn after_transcription(db: &mut Db, settings: &Settings, id: i64, analyze: bool) -> Result<()> {
    run_transcription(db, settings, id)?;
    if !analyze {
        return Ok(());
    }
    match analysis_blocker(settings) {
        None => {
            run_analysis(db, settings, &settings.model, id, false, false)?;
            run_next(db, settings, &settings.model, id, false)?;
            outln!("{}", style(format!("What to do next: ic next {id} · All stages: ic steps {id}")).dim());
        }
        Some(reason) => outln!("{}", style(format!("Skipping analysis: {reason}, then: ic run report {id} --then-later")).dim()),
    }
    Ok(())
}

/// Re-run one stage; with `then_later`, also update every later stage that's out of date.
fn run_step(db: &mut Db, settings: &Settings, step: Step, id: i64, explicit_model: Option<&ModelRef>,
            speakers: Option<i64>, then_later: bool) -> Result<()> {
    run_one(db, settings, step, id, explicit_model, speakers)?;
    if then_later {
        for later in Step::ALL.iter().copied().skip_while(|s| *s != step).skip(1) {
            let state = steps::flow(db, id)?.into_iter().find(|s| s.step == later).expect("every stage has a state");
            if matches!(state.status, StageStatus::OutOfDate | StageStatus::NotRun | StageStatus::Failed) {
                run_one(db, settings, later, id, explicit_model, None)?;
            }
        }
    }
    outln!();
    print_steps(db, id)
}

fn run_one(db: &mut Db, settings: &Settings, step: Step, id: i64, explicit_model: Option<&ModelRef>,
           speakers: Option<i64>) -> Result<()> {
    match step {
        Step::Recording => {
            let mut ui = Ui::new();
            let result = pipeline::reprocess_audio(db, id, &mut ui);
            ui.finish();
            result?;
            outln!("{} Re-processed the audio for session {id}.", style("✓").green());
        }
        Step::Transcript => {
            if let Some(n) = speakers {
                let mut session = db.get_session(id)?;
                session.num_speakers = Some(n);
                db.save_session(&session)?;
            }
            run_transcription(db, settings, id)?;
        }
        Step::Report => {
            let model = explicit_model.cloned().unwrap_or_else(|| settings.model.clone());
            run_analysis(db, settings, &model, id, false, false)?;
        }
        Step::Next => {
            // Next steps default to the model that wrote the report they build on.
            let model = match explicit_model {
                Some(m) => m.clone(),
                None => pipeline::report_model(db, id)?.unwrap_or_else(|| settings.model.clone()),
            };
            run_next(db, settings, &model, id, true)?;
        }
    }
    Ok(())
}

fn print_steps(db: &Db, id: i64) -> Result<()> {
    let view = session_view::build(db, id)?;
    outln!("{}", style(format!("━━ {} ━━", view.session.title)).bold());
    for stage in &view.stages {
        let (mark, status) = match stage.status {
            StageStatus::Done => (style("✓").green(), style("done".to_string()).green()),
            StageStatus::Running => (
                style("…").cyan(),
                style(match stage.progress {
                    Some(p) => format!("running {p:.0}%"),
                    None => "running".into(),
                })
                .cyan(),
            ),
            StageStatus::OutOfDate => (style("⚠").yellow(), style("out of date".to_string()).yellow()),
            StageStatus::Failed => (style("✗").red(), style("failed".to_string()).red()),
            StageStatus::NotRun => (style("○").dim(), style("not run".to_string()).dim()),
        };
        let when = stage.last_run_at.as_deref().map(|t| t.chars().take(16).collect::<String>().replace('T', " "));
        outln!(" {mark} {:<20} {:<12} {}  {}", stage.label, status.to_string(), stage.summary.as_deref().unwrap_or(""),
                 style(when.unwrap_or_default()).dim());
        if let Some(error) = &stage.error {
            outln!("     {}", style(error).red());
        }
    }
    if let Some(first) = view.stages.iter().find(|s| s.status == StageStatus::OutOfDate) {
        let upstream = first.step.upstream().map(|u| u.as_str()).unwrap_or("recording");
        outln!("{}", style(format!("Update later stages: ic run {} {id} --then-later   (or just: ic run {} {id} --then-later)",
                                     first.step, upstream)).dim());
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    out!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn record(settings: &Settings, title: Option<String>, company: Option<String>, aec: bool, duration: Option<u32>,
          yes: bool, analyze: bool) -> Result<()> {
    if !yes && !confirm("Has everyone on the call agreed to be recorded?")? {
        bail!("Not recording. Some places require everyone's consent to record a call.");
    }
    let mut db = open_db(settings)?;
    let title = title.unwrap_or_else(|| chrono::Local::now().format("Interview %Y-%m-%d %H:%M").to_string());
    let session = pipeline::create_recording_session(&db, settings, &title, company)?;
    let dir = PathBuf::from(&session.dir);

    let started = capture::launch(&dir, duration, aec).and_then(|_| {
        let spinner = Ui::new();
        spinner.bar.set_message("Starting recorder — if macOS asks for permission, click Allow…");
        let r = capture::wait_started(&dir, Duration::from_secs(90));
        spinner.finish();
        r
    });
    if let Err(e) = started {
        db.set_status(session.id, Status::Failed, Some(format!("{e:#}")))?;
        return Err(e.context("Couldn't start recording"));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))?;
    let ui = Ui::new();
    let t0 = Instant::now();
    while capture::is_running(&dir) && !stop.load(Ordering::SeqCst) {
        ui.bar.set_message(format!("{} Recording {} — press Ctrl+C to stop", style("●").red(),
                                   fmt_ts(t0.elapsed().as_secs_f64())));
        std::thread::sleep(Duration::from_millis(250));
    }
    ui.finish();
    finish_recording(&mut db, settings, session.id, analyze, true)
}

/// `stop_recorder`: signal ICRecorder.app to stop first (the CLI flow). The Interview Coach app
/// stops its own in-process recording before calling `ic recording finish`.
fn finish_recording(db: &mut Db, settings: &Settings, id: i64, analyze: bool, stop_recorder: bool) -> Result<()> {
    let mut session = db.get_session(id)?;
    let dir = PathBuf::from(&session.dir);
    if stop_recorder {
        capture::stop(&dir)?;
    } else if capture::is_running(&dir) {
        bail!("session {id} is still recording");
    } else if capture::read_report(&dir).is_none() {
        db.set_status(id, Status::Failed, Some("No recording was made".into()))?;
        bail!("session {id} has no finished recording (no recorder.json in {})", dir.display());
    }
    let ui = Ui::new();
    ui.bar.set_message("Saving recording…");
    let report = capture::wait_finished(&dir, Duration::from_secs(30));
    ui.finish();
    let report = report.inspect_err(|e| {
        let _ = db.set_status(id, Status::Failed, Some(e.to_string()));
    })?;
    for w in capture::report_warnings(&report) {
        warn(&w);
    }
    let (mic, system) = (dir.join("mic.wav"), dir.join("system.wav"));
    if !mic.exists() || !system.exists() {
        db.set_status(id, Status::Failed, Some("Recorder produced no audio files".into()))?;
        bail!("The recorder produced no audio. See {}", dir.join("recorder.log").display());
    }
    // The raw 48 kHz WAVs stay next to the FLACs for now: they're what we'd inspect if the recorder
    // misbehaves. Revisit deleting them once real calls have validated the recorder.
    let mut ui = Ui::new();
    let processed = pipeline::process_recording(db, id, &mic, &system, &mut ui);
    ui.finish();
    session = processed.with_context(|| format!("Couldn't process the recording; the raw audio is still in {}", dir.display()))?;
    outln!("Saved session {} ({}) → {}", style(id).bold(), fmt_ts(session.duration_s.unwrap_or(0.0)), dir.display());
    after_transcription(db, settings, id, analyze)
}

/// One row of `ic list --json`.
#[derive(serde::Serialize)]
struct SessionRow {
    id: i64,
    created_at: String,
    title: String,
    company: Option<String>,
    stage: Option<&'static str>,
    status: &'static str,
    mode: &'static str,
    duration_s: Option<f64>,
    dir: String,
    verdict: Option<&'static str>,
    verdict_label: Option<&'static str>,
    outcome: Option<&'static str>,
    outcome_label: Option<&'static str>,
    report_path: Option<String>,
    transcript_path: Option<String>,
    error: Option<String>,
}

fn list_json(db: &Db) -> Result<()> {
    let (verdicts, outcomes) = (db.latest_verdicts()?, db.all_outcomes()?);
    let existing = |dir: &str, name: &str| {
        let p = Path::new(dir).join(name);
        p.exists().then(|| p.display().to_string())
    };
    let rows: Vec<SessionRow> = db
        .list_sessions()?
        .into_iter()
        .map(|s| SessionRow {
            id: s.id,
            stage: s.stage.map(|st| st.label()),
            status: s.status.as_str(),
            mode: s.mode.as_str(),
            verdict: verdicts.get(&s.id).map(|v| v.as_str()),
            verdict_label: verdicts.get(&s.id).map(|v| v.label()),
            outcome: outcomes.get(&s.id).map(|o| o.result.as_str()),
            outcome_label: outcomes.get(&s.id).map(|o| o.result.label()),
            report_path: existing(&s.dir, "report.html"),
            transcript_path: existing(&s.dir, "transcript.md"),
            created_at: s.created_at,
            title: s.title,
            company: s.company,
            duration_s: s.duration_s,
            dir: s.dir,
            error: s.error,
        })
        .collect();
    outln!("{}", serde_json::to_string(&rows)?);
    Ok(())
}

fn list(settings: &Settings, json: bool) -> Result<()> {
    let db = open_db(settings)?;
    if json {
        return list_json(&db);
    }
    let sessions = db.list_sessions()?;
    if sessions.is_empty() {
        outln!("No sessions yet. Try: ic record   or   ic import path/to/interview.m4a");
        return Ok(());
    }
    let (verdicts, outcomes) = (db.latest_verdicts()?, db.all_outcomes()?);
    let header = ["ID", "Date", "Title", "Company", "Length", "Status", "Verdict", "Outcome"];
    let rows: Vec<[String; 8]> = sessions
        .iter()
        .map(|s| {
            [
                s.id.to_string(),
                s.created_at.chars().take(10).collect(),
                s.title.clone(),
                s.company.clone().unwrap_or_default(),
                fmt_ts(s.duration_s.unwrap_or(0.0)),
                s.status.to_string(),
                verdicts.get(&s.id).map(|v| v.label().to_string()).unwrap_or_default(),
                outcomes.get(&s.id).map(|o| o.result.label().to_string()).unwrap_or_default(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| rows.iter().map(|r| console::measure_text_width(&r[i])).chain([header[i].len()]).max().unwrap_or(0))
        .collect();
    let line = |cells: Vec<String>| {
        cells.iter().zip(&widths).map(|(c, w)| console::pad_str(c, *w, console::Alignment::Left, None).into_owned())
            .collect::<Vec<_>>()
            .join("  ")
    };
    outln!("{}", style(line(header.iter().map(|h| h.to_string()).collect())).bold());
    for (row, s) in rows.iter().zip(&sessions) {
        let text = line(row.to_vec());
        outln!("{}", if s.status == Status::Failed { style(text).red().to_string() } else { text });
    }
    Ok(())
}

fn transcript(settings: &Settings, id: i64, json: bool) -> Result<()> {
    let db = open_db(settings)?;
    let session = db.get_session(id)?;
    let segments = db.get_segments(id)?;
    if segments.is_empty() {
        let hint = session.error.map(|e| format!(" Error: {e}")).unwrap_or_else(|| format!(" Run: ic transcribe {id}"));
        bail!("Session {id} has no transcript yet ({}).{hint}", session.status);
    }
    if json {
        let plain: Vec<_> = segments.into_iter().map(|s| interview_coach::models::Segment { words: vec![], ..s }).collect();
        outln!("{}", serde_json::to_string_pretty(&plain)?);
        return Ok(());
    }
    outln!("{}\n", style(format!("━━ {} · {} ━━", session.title, fmt_ts(session.duration_s.unwrap_or(0.0)))).bold());
    for t in to_turns(&segments) {
        let name = speaker_label(&t.speaker);
        let name = if t.speaker == YOU { style(format!("{name}:")).cyan().bold() } else { style(format!("{name}:")).magenta().bold() };
        outln!("{} {name} {}\n", style(fmt_ts(t.start)).dim(), t.text);
    }
    Ok(())
}

fn swap(settings: &Settings, id: i64) -> Result<()> {
    let mut db = open_db(settings)?;
    pipeline::swap_speakers(&mut db, id, &mut interview_coach::progress::Quiet)?;
    outln!("Swapped speakers for session {id}. The report and next steps are now out of date:");
    outln!("  ic run report {id} --then-later");
    Ok(())
}

fn show_report(settings: &Settings, id: i64, open: bool, full: bool) -> Result<()> {
    let db = open_db(settings)?;
    let session = db.get_session(id)?;
    let Some(stored) = db.latest_analysis(id)? else {
        bail!("Session {id} hasn't been analysed yet. Run: ic analyze {id}");
    };
    let outcome = db.get_outcome(id)?;
    let path = report::write_html(&session, &stored, outcome.as_ref())?;
    report::print_report(&session, &stored, outcome.as_ref(), full);
    if open {
        Command::new("open").arg(path).status()?;
    }
    Ok(())
}

fn refresh_timeline(settings: &Settings, id: i64) -> Result<()> {
    let mut db = open_db(settings)?;
    let mut ui = Ui::new();
    let has_jev = proxy::using_external_proxy() || proxy::has_key(settings, KeyTarget::TypeSafe);
    let (jev_client, jev_scorer);
    let scorer: Option<&dyn Scorer> = if has_jev {
        if let Err(e) = proxy::ensure_running(settings, &mut ui) {
            ui.finish();
            return Err(e);
        }
        jev_client = interview_coach::llm::jev::Client::new(LlmEndpoint::load(settings).context("the proxy isn't set up")?);
        jev_scorer = JevScorer { client: &jev_client, model: interview_coach::llm::jev::DEFAULT_MODEL.into() };
        Some(&jev_scorer)
    } else {
        None
    };
    let result = pipeline::refresh_timeline(&mut db, id, scorer, &mut ui);
    ui.finish();
    let (stored, warnings) = result?;
    for w in &warnings {
        errln!("{} {w}", style("Warning:").yellow());
    }
    let session = db.get_session(id)?;
    let outcome = db.get_outcome(id)?;
    report::write_analysis_html(&session, &stored, outcome.as_ref())?;
    report::write_html(&session, &stored, outcome.as_ref())?;
    if stored.turn_signals.iter().any(|s| s.temperature.is_some()) {
        report::print_room(&stored, true);
    } else {
        outln!("The interviewer didn't say enough to read the room.");
    }
    Ok(())
}

fn outcome(settings: &Settings, id: i64, result: OutcomeResult, notes: Option<String>) -> Result<()> {
    let db = open_db(settings)?;
    let outcome = db.set_outcome(id, result, notes.as_deref())?;
    let session = db.get_session(id)?;
    for analysis in db.analyses(id)? {
        report::write_analysis_html(&session, &analysis, Some(&outcome))?;  // every report page shows the outcome
    }
    match db.latest_analysis(id)? {
        Some(stored) => {
            report::write_html(&session, &stored, Some(&outcome))?;
            outln!("Recorded {} for session {id} (the analysis predicted: {}).", style(outcome.result.label()).bold(),
                     stored.analysis.outlook.verdict.label());
        }
        None => outln!("Recorded {} for session {id}.", style(outcome.result.label()).bold()),
    }
    Ok(())
}

fn read_key(target: KeyTarget, from_stdin: bool) -> Result<String> {
    let key = if from_stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    } else if let Ok(key) = std::env::var(target.env_var()) {
        outln!("Using the {} key from ${}.", target.label(), target.env_var());
        key
    } else {
        outln!("Opening {} — create a {} API key there (or copy an existing one).", target.keys_url(), target.label());
        let _ = Command::new("/usr/bin/open").arg(target.keys_url()).status();
        let prompt = format!("Paste your {} API key (input hidden): ", target.label());
        rpassword::prompt_password(&prompt).or_else(|_| {
            // No terminal to hide input on (e.g. piped): read a plain line instead.
            out!("{prompt}");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok::<_, std::io::Error>(line)
        })?
    };
    let key = key.trim().to_string();
    target.check(&key).map_err(|reason| anyhow::anyhow!("{reason} Nothing was changed."))?;
    Ok(key)
}

fn proxy_setup(settings: &Settings) -> Result<()> {
    let mut ui = Ui::new();
    let result = proxy::setup(settings, &mut ui);
    ui.finish();
    result?;
    outln!("{} AI proxy running at {}.", style("✓").green(), proxy::base_url(settings));
    outln!("{}", style(format!("Config: {} · Spend: ic proxy status", proxy::dir(settings).display())).dim());
    Ok(())
}

fn proxy_key(settings: &Settings, target: KeyTarget, from_stdin: bool) -> Result<()> {
    if proxy::using_external_proxy() {
        bail!("ic is using the shared proxy at IC_LLM_URL; add the {} key there.", target.label());
    }
    let key = read_key(target, from_stdin)?;
    let mut ui = Ui::new();
    let ready = proxy::ensure_running(settings, &mut ui);
    ui.finish();
    ready?;
    proxy::write_files(settings, Some((target, &key)))?;
    proxy::restart_with_new_env(settings)?;
    proxy::wait_ready(&proxy::base_url(settings), Duration::from_secs(300))?;
    proxy::ensure_key(settings)?;
    outln!("{} The AI proxy now holds your {} key.", style("✓").green(), target.label());
    if target == KeyTarget::OpenAi {
        outln!("Use it with --model openai/<model>, or make it the default: ic config set model openai/<model>");
    }
    Ok(())
}

fn login(events: bool) -> Result<()> {
    if events {
        let mut out = JsonEvents::new();
        let result = auth::login(Some(&mut |event| match event {
            auth::LoginEvent::OpenUrl(url) => out.open_url(&url),
            auth::LoginEvent::NeedCode => out.need_code(),
            auth::LoginEvent::Log(line) => out.log(&line),
        }))
        .and_then(|_| auth::access_token().map(|_| ()));
        return match result {
            Ok(()) => {
                out.done();
                Ok(())
            }
            Err(e) => {
                out.error(&format!("{e:#}"));
                Err(e)
            }
        };
    }
    outln!("{} — a browser window will open. Approve access there, then come back here.", style("Claude sign-in").bold());
    outln!("{}", style("If the page shows a code instead of closing, paste it at the Code: prompt below.").dim());
    auth::login(None)?;
    auth::access_token()?; // prove the session works before saying so
    outln!("{} Signed in to Claude — no API key stored. ic gets short-lived tokens from this session as needed.",
             style("✓").green());
    Ok(())
}

fn setup_run(settings: &Settings, step: setup::Step, events: bool) -> Result<()> {
    if events {
        let mut out = JsonEvents::new();
        let result = setup::run(step, settings, &mut out);
        match &result {
            Ok(()) => out.done(),
            Err(e) => out.error(&format!("{e:#}")),
        }
        return result;
    }
    let mut ui = Ui::new();
    let result = setup::run(step, settings, &mut ui);
    ui.finish();
    result?;
    outln!("{} Done.", style("✓").green());
    Ok(())
}

fn config_show(settings: &Settings) {
    outln!("{}", style(format!("Config file: {}", settings.config_path().display())).dim());
    outln!("model          {}", settings.model);
    outln!("language       {}", settings.language.as_deref().unwrap_or("auto"));
    outln!("whisper_model  {}", settings.whisper_model);
    outln!("scorer         {}", settings.scorer);
    outln!("models_dir     {}", settings.models_dir.display());
    outln!("{}", style("Environment variables IC_MODEL, IC_LANGUAGE, IC_WHISPER_MODEL, IC_MODELS_DIR override the file.").dim());
}

fn config_set(settings: &Settings, key: ConfigKey, value: &str) -> Result<()> {
    let path = settings.config_path();
    let mut file = FileConfig::read(&path)?;
    match key {
        ConfigKey::Model => file.model = Some(value.parse().map_err(|e: String| anyhow::anyhow!("model: {e}"))?),
        ConfigKey::Language => file.language = Some(value.to_string()),
        ConfigKey::WhisperModel => file.whisper_model = Some(value.to_string()),
        ConfigKey::Scorer => file.scorer = Some(value.parse().map_err(|e: String| anyhow::anyhow!("scorer: {e}"))?),
    }
    let previous = std::fs::read_to_string(&path).ok();
    file.write(&path)?;
    // Re-load so an invalid value (e.g. a bad language code) is rejected and nothing is left half-saved.
    if let Err(e) = Settings::load() {
        match previous {
            Some(text) => std::fs::write(&path, text)?,
            None => std::fs::remove_file(&path)?,
        }
        return Err(e.context("not saved"));
    }
    outln!("{} Saved to {}", style("✓").green(), path.display());
    Ok(())
}

fn proxy_status(settings: &Settings) -> Result<()> {
    let Some(endpoint) = LlmEndpoint::load(settings) else {
        bail!("The LLM proxy isn't set up yet. Run: ic proxy setup");
    };
    match proxy::readiness(&endpoint.base_url) {
        Ok(r) => outln!("{} Proxy up at {} (database {})", style("✓").green(), endpoint.base_url,
                          r["db"].as_str().unwrap_or("unknown")),
        Err(_) => bail!("The proxy at {} isn't responding. Start it with: ic proxy start", endpoint.base_url),
    }
    let info = proxy::key_info(&endpoint)?;
    outln!("{} ic's key: {} · spent so far: ${:.4}", style("✓").green(),
             info["key_alias"].as_str().unwrap_or("?"), info["spend"].as_f64().unwrap_or(0.0));
    Ok(())
}

fn ai_endpoint(settings: &Settings) -> Result<LlmEndpoint> {
    let mut ui = Ui::new();
    let ready = proxy::ensure_running(settings, &mut ui);
    ui.finish();
    ready?;
    LlmEndpoint::load(settings).context("The AI proxy isn't set up yet. Open Setup in the app (or run: ic proxy setup).")
}

fn jev_ping(settings: &Settings) -> Result<()> {
    use interview_coach::llm::jev::{self, Question, SystemOne};
    if !proxy::using_external_proxy() && !proxy::has_key(settings, KeyTarget::TypeSafe) {
        bail!("The AI proxy has no TypeSafe key yet. Add it in Setup, or run: ic proxy key typesafe");
    }
    let client = jev::Client::new(ai_endpoint(settings)?);
    let req = jev::Request {
        state: serde_json::json!("Help! My payouts have been failing for 3 days."),
        model: jev::DEFAULT_MODEL.into(),
        questions: vec![("is_urgent".into(), Question::Noul { instructions: "Does this convey urgency?".into(), yes: None, no: None })],
    };
    let resp = client.ask(&req)?;
    outln!("{} {} answered in {} ms: {:?}", style("✓").green(), resp.model, resp.latency_ms, resp.answers["is_urgent"]);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn eval_scorers(settings: &Settings, which: EvalSet, items_path: Option<PathBuf>, arm_names: &[String], runs: usize,
                out: Option<PathBuf>, limit: Option<usize>, concurrency: usize) -> Result<()> {
    use interview_coach::eval;
    let set = which.check_set();
    let out = out.unwrap_or_else(|| which.default_out());
    let out = out.as_path();
    let mut items = eval::load_items(&items_path.unwrap_or_else(|| which.default_items()))?;
    if let Some(n) = limit {
        items.truncate(n);
    }
    let arms: Vec<eval::Arm> = arm_names.iter().map(|a| eval::Arm::parse(a)).collect::<Result<_, _>>().map_err(|e| anyhow::anyhow!(e))?;
    if arms.iter().any(|a| a.name == "jev") && !proxy::using_external_proxy() && !proxy::has_key(settings, KeyTarget::TypeSafe) {
        bail!("The AI proxy has no TypeSafe key yet. Add it in Setup, or run: ic proxy key typesafe");
    }
    if arms.iter().any(|a| a.name != "jev") && !auth::has_login() {
        bail!("The Claude arms need you signed in to Claude (Setup in the app, or: ic login).");
    }
    let endpoint = ai_endpoint(settings)?;
    outln!("Scoring {} {} × {runs} runs with {} → {}", items.len(), if set.id == "answers" { "answers" } else { "turns" },
             arms.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "), out.display());
    let results = eval::run(
        &eval::Plan { set: &set, items: &items, arms: &arms, runs, concurrency, out_dir: out },
        &endpoint,
        &|r, done, total| {
            let status = match &r.result {
                Ok(a) => format!("{} ms", a.latency_ms),
                Err(e) => format!("failed: {}", e.lines().next().unwrap_or_default()),
            };
            errln!("[{done}/{total}] {:<6} {:<28} run {} {status}", r.arm, r.item, r.run);
        },
    )?;
    let stats = eval::summarize(&results, &items, &arms, &set);
    let decisions = eval::decide(&stats, &arms, &set);
    let fallback = arms.iter().filter(|a| a.name != "jev").min_by_key(|a| a.cost_rank()).map(|a| a.name.clone());
    let cascade_rows = match (&fallback, arms.iter().any(|a| a.name == "jev")) {
        (Some(f), true) => eval::cascade(&results, &items, f, &set),
        _ => vec![],
    };
    let mut md = eval::markdown(&stats, &decisions, &cascade_rows, fallback.as_deref().unwrap_or("-"), items.len(), runs, &set);
    md += &eval::disagreements(&results, &items, &arms, &set);
    let path = eval::write_summary(out, &stats, &decisions, &md)?;
    for d in &decisions {
        outln!("{:<22} {}", d.check, match (&d.winner, &d.fallback) {
            (Some(w), _) => style(w.clone()).green().to_string(),
            (None, Some(f)) => style(format!("none qualifies (most accurate: {f})")).yellow().to_string(),
            (None, None) => style("none qualifies".to_string()).yellow().to_string(),
        });
    }
    outln!("Summary: {}", path.display());
    Ok(())
}

/// `ic setup status` (and `ic doctor`): what's done, what's left, and how to do it.
fn setup_status(settings: &Settings, json: bool) -> Result<()> {
    let status = setup::status(&setup::System { settings });
    if json {
        outln!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    for check in &status.checks {
        let mark = match check.status {
            setup::Status::Ok => style("✓").green(),
            setup::Status::Action => style("•").yellow(),
            setup::Status::Blocked => style("…").dim(),
            setup::Status::Optional => style("○").dim(),
        };
        outln!("{mark} {}", check.title);
        if !check.detail.is_empty() && check.status != setup::Status::Ok {
            outln!("  {}", style(&check.detail).dim());
        }
    }
    for tool in [Tool::Ffmpeg, Tool::Ant, Tool::Docker] {
        if let Some(found) = tool.find() {
            let origin = match found.origin {
                Origin::Bundled => "bundled with the app",
                Origin::Override => "from IC_* override",
                Origin::System => "installed on this Mac",
            };
            outln!("{}", style(format!("  {} {} ({origin})", tool.name(), found.path.display())).dim());
        }
    }
    let app = capture::app_path();
    if !app.exists() {
        outln!("{} Recorder app not found ({}) — `ic record` needs it; the app records by itself",
                 style("•").yellow(), app.display());
    }
    if status.ready {
        outln!("{} Ready. Analysis model: {}", style("✓").green(), settings.model);
    } else {
        outln!("{} {} thing(s) left — open Setup in the app, or: ic setup run all, then ic login",
                 style("•").yellow(), status.remaining);
    }
    outln!("{}", style(format!("Data folder: {} · Models: {}", settings.data_dir.display(),
                                 settings.models_dir.display())).dim());
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut settings = Settings::load()?;
    // Kept apart from the default: `ic run next` falls back to the report's model, not the default.
    let explicit_model = cli.model.clone();
    if let Some(model) = cli.model {
        settings.model = model;
    }
    match cli.command {
        Cmd::Record { title, company, aec, duration, yes, no_analyze } => {
            record(&settings, title, company, aec, duration, yes, !no_analyze)
        }
        Cmd::Stop { no_analyze } => {
            let mut db = open_db(&settings)?;
            let active: Vec<_> = db.list_sessions()?.into_iter().filter(|s| s.status == Status::Recording).collect();
            if active.is_empty() {
                outln!("Nothing is recording.");
            }
            for s in active {
                if let Err(e) = finish_recording(&mut db, &settings, s.id, !no_analyze, true) {
                    warn(&format!("session {}: {e:#}", s.id));
                }
            }
            Ok(())
        }
        Cmd::Import { path, mic, system, title, company, speakers, no_transcribe, no_analyze } => {
            let mut db = open_db(&settings)?;
            let session = match (path, mic, system) {
                (Some(path), None, None) => {
                    let title = title.unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into());
                    let mut ui = Ui::new();
                    let session = pipeline::ingest_file(&mut db, &settings, &path, &title, company, Some(speakers), &mut ui);
                    ui.finish();
                    session?
                }
                (None, Some(mic), Some(system)) => {
                    let title = title.unwrap_or_else(|| {
                        mic.canonicalize().ok().and_then(|p| Some(p.parent()?.file_name()?.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| "Interview".into())
                    });
                    let mut ui = Ui::new();
                    let session = pipeline::ingest_tracks(&mut db, &settings, &mic, &system, &title, company, &mut ui);
                    ui.finish();
                    session?
                }
                _ => bail!("Pass a recording file, or both --mic and --system tracks."),
            };
            outln!("Imported as session {} ({}) → {}", style(session.id).bold(),
                     fmt_ts(session.duration_s.unwrap_or(0.0)), session.dir);
            if no_transcribe {
                return Ok(());
            }
            after_transcription(&mut db, &settings, session.id, !no_analyze)
        }
        Cmd::Transcribe { id } => run_transcription(&mut open_db(&settings)?, &settings, id),
        Cmd::List { json } => list(&settings, json),
        Cmd::Recording { action } => match action {
            RecordingCmd::Begin { title, company } => {
                let db = open_db(&settings)?;
                let title = title.unwrap_or_else(|| chrono::Local::now().format("Interview %Y-%m-%d %H:%M").to_string());
                let session = pipeline::create_recording_session(&db, &settings, &title, company)?;
                outln!("{}", serde_json::json!({"id": session.id, "dir": session.dir}));
                Ok(())
            }
            RecordingCmd::Finish { id, no_analyze } => {
                finish_recording(&mut open_db(&settings)?, &settings, id, !no_analyze, false)
            }
        },
        Cmd::Transcript { id, json } => transcript(&settings, id, json),
        Cmd::Swap { id } => swap(&settings, id),
        Cmd::Analyze { id, open, full } => run_analysis(&mut open_db(&settings)?, &settings, &settings.model, id, open, full),
        Cmd::Steps { id, json } => {
            let db = open_db(&settings)?;
            if json {
                outln!("{}", serde_json::to_string_pretty(&session_view::build(&db, id)?.stages)?);
                Ok(())
            } else {
                print_steps(&db, id)
            }
        }
        Cmd::Run { step, id, speakers, then_later } => {
            run_step(&mut open_db(&settings)?, &settings, step, id, explicit_model.as_ref(), speakers, then_later)
        }
        Cmd::Next { id } => {
            let db = open_db(&settings)?;
            let Some(next) = db.latest_next_steps(id)? else {
                bail!("Session {id} has no next steps yet. Run: ic run next {id}");
            };
            report::print_next_steps(&db.get_session(id)?, &next);
            Ok(())
        }
        Cmd::Session { id } => {
            outln!("{}", serde_json::to_string(&session_view::build(&open_db(&settings)?, id)?)?);
            Ok(())
        }
        Cmd::Report { id, open, full } => show_report(&settings, id, open, full),
        Cmd::Timeline { id } => refresh_timeline(&settings, id),
        Cmd::Outcome { id, result, notes } => outcome(&settings, id, result, notes),
        Cmd::Proxy { action } => match action {
            ProxyCmd::Setup => {
                proxy_setup(&settings)?;
                if !auth::has_login() {
                    outln!("Next, sign in to Claude: ic login");
                }
                Ok(())
            }
            ProxyCmd::Key { target, stdin } => proxy_key(&settings, target, stdin),
            ProxyCmd::Start => {
                let mut ui = Ui::new();
                let result = proxy::ensure_running(&settings, &mut ui);
                ui.finish();
                result?;
                outln!("{} AI proxy running at {}", style("✓").green(), proxy::base_url(&settings));
                Ok(())
            }
            ProxyCmd::Stop => proxy::stop(&settings),
            ProxyCmd::Status => proxy_status(&settings),
        },
        Cmd::Login { events } => login(events),
        Cmd::Setup { action } => match action {
            SetupCmd::Status { json } => setup_status(&settings, json),
            SetupCmd::Run { step, events } => setup_run(&settings, step, events),
        },
        Cmd::Config { action } => match action {
            ConfigCmd::Show => {
                config_show(&settings);
                Ok(())
            }
            ConfigCmd::Set { key, value } => config_set(&settings, key, &value),
        },
        Cmd::Doctor => setup_status(&settings, false),
        Cmd::Eval { action: EvalCmd::Scorers { set, items, arms, runs, out, limit, concurrency } } => {
            eval_scorers(&settings, set, items, &arms, runs, out, limit, concurrency)
        }
        Cmd::Jev { action: JevCmd::Ping } => jev_ping(&settings),
    }
}

fn main() {
    if let Err(e) = run() {
        errln!("{} {e:#}", style("Error:").red().bold());
        std::process::exit(1);
    }
}
