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
use interview_coach::capture;
use interview_coach::config::{FileConfig, ModelRef, Provider, Settings, which};
use interview_coach::db::Db;
use interview_coach::diarize;
use interview_coach::llm;
use interview_coach::merge::{swap_you_and_interviewer, to_turns};
use interview_coach::models::{Mode, OutcomeResult, Status, YOU, fmt_ts, speaker_label};
use interview_coach::pipeline;
use interview_coach::progress::Progress;
use interview_coach::proxy::{self, LlmEndpoint};
use interview_coach::report;
use interview_coach::transcribe;

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
    /// Sign in to Claude through your browser (no API key needed). Starts the LLM proxy if needed.
    Login,
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
    Doctor {
        /// Machine-readable summary (used by the Interview Coach app).
        #[arg(long)]
        json: bool,
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
}

#[derive(Subcommand)]
enum ProxyCmd {
    /// Set up (or repair) the proxy: write its config, start it, and give ic its own key.
    Setup,
    /// Add or replace OpenAI's API key in the proxy (Claude uses `ic login` instead).
    Key { provider: Provider },
    /// Start the proxy containers.
    Start,
    /// Stop the proxy containers (your keys and spend history are kept).
    Stop,
    /// Show whether the proxy is up and what ic has spent through it.
    Status,
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
    eprintln!("{} {msg}", style("Warning:").red().bold());
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
    println!("{} {} turns. View it with {}", style(format!("Transcribed session {id}:")).green(),
             to_turns(&result.segments).len(), style(format!("ic transcript {id}")).bold());
    Ok(())
}

/// Why analysis can't run with the current setup, if it can't.
fn analysis_blocker(settings: &Settings) -> Option<String> {
    if LlmEndpoint::load(settings).is_none() {
        return Some("the LLM proxy isn't set up yet — run: ic proxy setup".into());
    }
    match settings.model.provider {
        Provider::Anthropic => (!auth::has_login()).then(|| "you're not signed in to Claude — run: ic login".to_string()),
        Provider::OpenAi => (!proxy::using_external_proxy() && !proxy::has_provider_key(settings, Provider::OpenAi))
            .then(|| format!("the proxy has no OpenAI key, which {} needs — add one with: ic proxy key openai",
                             settings.model)),
    }
}

fn run_analysis(db: &mut Db, settings: &Settings, id: i64, open: bool, full: bool) -> Result<()> {
    if let Some(reason) = analysis_blocker(settings) {
        bail!("Can't analyse yet: {reason}\nThen run: ic analyze {id}");
    }
    let endpoint = LlmEndpoint::load(settings).expect("checked above");
    let client = llm::client(&settings.model, endpoint);
    let mut ui = Ui::new();
    let stored = pipeline::analyze_session(db, client.as_ref(), &settings.model, id, &mut ui);
    ui.finish();
    let stored = stored?;
    let session = db.get_session(id)?;
    let outcome = db.get_outcome(id)?;
    let path = report::write_html(&session, &stored, outcome.as_ref())?;
    report::print_report(&session, &stored, outcome.as_ref(), full);
    if open {
        Command::new("open").arg(path).status()?;
    }
    Ok(())
}

fn after_transcription(db: &mut Db, settings: &Settings, id: i64, analyze: bool) -> Result<()> {
    run_transcription(db, settings, id)?;
    if !analyze {
        return Ok(());
    }
    match analysis_blocker(settings) {
        None => run_analysis(db, settings, id, false, false)?,
        Some(reason) => println!("{}", style(format!("Skipping analysis: {reason}, then: ic analyze {id}")).dim()),
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
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
    pipeline::normalize_tracks(db, &mut session, &mic, &system)
        .with_context(|| format!("Couldn't process the recording; the raw audio is still in {}", dir.display()))?;
    println!("Saved session {} ({}) → {}", style(id).bold(), fmt_ts(session.duration_s.unwrap_or(0.0)), dir.display());
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
    println!("{}", serde_json::to_string(&rows)?);
    Ok(())
}

fn list(settings: &Settings, json: bool) -> Result<()> {
    let db = open_db(settings)?;
    if json {
        return list_json(&db);
    }
    let sessions = db.list_sessions()?;
    if sessions.is_empty() {
        println!("No sessions yet. Try: ic record   or   ic import path/to/interview.m4a");
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
    println!("{}", style(line(header.iter().map(|h| h.to_string()).collect())).bold());
    for (row, s) in rows.iter().zip(&sessions) {
        let text = line(row.to_vec());
        println!("{}", if s.status == Status::Failed { style(text).red().to_string() } else { text });
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
        println!("{}", serde_json::to_string_pretty(&plain)?);
        return Ok(());
    }
    println!("{}\n", style(format!("━━ {} · {} ━━", session.title, fmt_ts(session.duration_s.unwrap_or(0.0)))).bold());
    for t in to_turns(&segments) {
        let name = speaker_label(&t.speaker);
        let name = if t.speaker == YOU { style(format!("{name}:")).cyan().bold() } else { style(format!("{name}:")).magenta().bold() };
        println!("{} {name} {}\n", style(fmt_ts(t.start)).dim(), t.text);
    }
    Ok(())
}

fn swap(settings: &Settings, id: i64) -> Result<()> {
    let mut db = open_db(settings)?;
    let session = db.get_session(id)?;
    if session.mode == Mode::Dual {
        bail!("This session has separate tracks, so its labels come from the tracks themselves.");
    }
    let segments = swap_you_and_interviewer(&db.get_segments(id)?);
    db.replace_segments(id, &segments)?;
    pipeline::write_transcript_files(Path::new(&session.dir), &segments)?;
    println!("Swapped speakers for session {id}. Re-run the analysis with: ic analyze {id}");
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

fn outcome(settings: &Settings, id: i64, result: OutcomeResult, notes: Option<String>) -> Result<()> {
    let db = open_db(settings)?;
    let outcome = db.set_outcome(id, result, notes.as_deref())?;
    match db.latest_analysis(id)? {
        Some(stored) => {
            report::write_html(&db.get_session(id)?, &stored, Some(&outcome))?;
            println!("Recorded {} for session {id} (the analysis predicted: {}).", style(outcome.result.label()).bold(),
                     stored.analysis.outlook.verdict.label());
        }
        None => println!("Recorded {} for session {id}.", style(outcome.result.label()).bold()),
    }
    Ok(())
}

fn read_provider_key(provider: Provider) -> Result<String> {
    if let Ok(key) = std::env::var(provider.key_var()) {
        println!("Using the {} key from ${}.", provider.label(), provider.key_var());
        return Ok(key);
    }
    println!("Opening {} — create a {} API key there (or copy an existing one).", provider.keys_url(), provider.label());
    let _ = Command::new("open").arg(provider.keys_url()).status();
    let prompt = format!("Paste your {} API key (input hidden): ", provider.label());
    let key = rpassword::prompt_password(&prompt).or_else(|_| {
        // No terminal to hide input on (e.g. piped): read a plain line instead.
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok::<_, std::io::Error>(line)
    })?;
    let key = key.trim().to_string();
    if !key.starts_with(provider.key_prefix()) {
        bail!("That doesn't look like an {} API key (they start with {}). Nothing was changed.", provider.label(),
              provider.key_prefix());
    }
    Ok(key)
}

fn proxy_setup(settings: &Settings) -> Result<()> {
    if !proxy::docker_running() {
        bail!("Docker isn't running. Start Docker Desktop, then run: ic proxy setup");
    }
    proxy::write_files(settings, None)?;
    println!("Starting LiteLLM (the first start downloads its images and sets up its database)…");
    proxy::start(settings)?;
    let base = proxy::base_url(settings);
    let ui = Ui::new();
    ui.bar.set_message("Waiting for the proxy to be ready…");
    let ready = proxy::wait_ready(&base, Duration::from_secs(300));
    ui.finish();
    ready?;
    proxy::ensure_key(settings)?;
    println!("{} LLM proxy running at {base}.", style("✓").green());
    println!("{}", style(format!("Config: {} · Spend: ic proxy status", proxy::dir(settings).display())).dim());
    Ok(())
}

fn proxy_key(settings: &Settings, provider: Provider) -> Result<()> {
    if provider == Provider::Anthropic {
        bail!("Claude uses your browser login instead of an API key — run: ic login");
    }
    if !proxy::docker_running() {
        bail!("Docker isn't running. Start Docker Desktop, then run: ic proxy key {provider}");
    }
    let key = read_provider_key(provider)?;
    proxy::write_files(settings, Some((provider, &key)))?;
    proxy::restart_with_new_env(settings)?;
    proxy::wait_ready(&proxy::base_url(settings), Duration::from_secs(300))?;
    proxy::ensure_key(settings)?;
    println!("{} The proxy now holds your {} key. Use it with: --model {provider}/<model>, or make it the default:",
             style("✓").green(), provider.label());
    println!("  ic config set model {provider}/<model>");
    Ok(())
}

fn login(settings: &Settings) -> Result<()> {
    // Claude requests go through the proxy, so make sure it's set up and running first.
    let proxy_ready = LlmEndpoint::load(settings).is_some_and(|e| proxy::readiness(&e.base_url).is_ok());
    if !proxy_ready && !proxy::using_external_proxy() {
        proxy_setup(settings)?;
    }
    println!("{} — a browser window will open. Approve access there, then come back here.", style("Claude sign-in").bold());
    println!("{}", style("If the page shows a code instead of closing, paste it at the Code: prompt below.").dim());
    auth::login()?;
    auth::access_token()?; // prove the session works before saying so
    println!("{} Signed in to Claude — no API key stored. ic gets short-lived tokens from this session as needed.",
             style("✓").green());
    Ok(())
}

fn config_show(settings: &Settings) {
    println!("{}", style(format!("Config file: {}", settings.config_path().display())).dim());
    println!("model          {}", settings.model);
    println!("language       {}", settings.language.as_deref().unwrap_or("auto"));
    println!("whisper_model  {}", settings.whisper_model);
    println!("models_dir     {}", settings.models_dir.display());
    println!("{}", style("Environment variables IC_MODEL, IC_LANGUAGE, IC_WHISPER_MODEL, IC_MODELS_DIR override the file.").dim());
}

fn config_set(settings: &Settings, key: ConfigKey, value: &str) -> Result<()> {
    let path = settings.config_path();
    let mut file = FileConfig::read(&path)?;
    match key {
        ConfigKey::Model => file.model = Some(value.parse().map_err(|e: String| anyhow::anyhow!("model: {e}"))?),
        ConfigKey::Language => file.language = Some(value.to_string()),
        ConfigKey::WhisperModel => file.whisper_model = Some(value.to_string()),
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
    println!("{} Saved to {}", style("✓").green(), path.display());
    Ok(())
}

fn proxy_status(settings: &Settings) -> Result<()> {
    let Some(endpoint) = LlmEndpoint::load(settings) else {
        bail!("The LLM proxy isn't set up yet. Run: ic proxy setup");
    };
    match proxy::readiness(&endpoint.base_url) {
        Ok(r) => println!("{} Proxy up at {} (database {})", style("✓").green(), endpoint.base_url,
                          r["db"].as_str().unwrap_or("unknown")),
        Err(_) => bail!("The proxy at {} isn't responding. Start it with: ic proxy start", endpoint.base_url),
    }
    let info = proxy::key_info(&endpoint)?;
    println!("{} ic's key: {} · spent so far: ${:.4}", style("✓").green(),
             info["key_alias"].as_str().unwrap_or("?"), info["spend"].as_f64().unwrap_or(0.0));
    Ok(())
}

/// `ic doctor --json`: the few facts the app's status row needs.
fn doctor_json(settings: &Settings) -> Result<()> {
    let (ffmpeg, ant) = (which("ffmpeg").is_some(), which("ant").is_some());
    let docker_installed = which("docker").is_some();
    let docker = docker_installed && proxy::docker_running();
    let signed_in = ant && auth::has_login() && auth::access_token().is_ok();
    let proxy_ready = LlmEndpoint::load(settings)
        .is_some_and(|e| proxy::readiness(&e.base_url).is_ok() && proxy::key_info(&e).is_ok());
    // Most fixable first: a fresh Mac needs its tools, then Docker, then a sign-in.
    let mut problems = vec![];
    if !ffmpeg {
        problems.push("ffmpeg isn't installed — run: brew install ffmpeg".to_string());
    }
    if !ant {
        problems.push("Anthropic's CLI (for Claude sign-in) isn't installed — run: brew install anthropics/tap/ant".to_string());
    }
    if !docker_installed {
        problems.push("Docker Desktop isn't installed — get it from docker.com/products/docker-desktop".to_string());
    } else if !docker {
        problems.push("Docker isn't running — start Docker Desktop".to_string());
    }
    let can_sign_in = ant && docker;
    if can_sign_in && !proxy_ready {
        problems.push("The LLM proxy isn't set up or running — sign in to start it".to_string());
    } else if can_sign_in && settings.model.provider == Provider::Anthropic && !signed_in {
        problems.push("Not signed in to Claude (or the login expired)".to_string());
    } else if let Some(reason) = analysis_blocker(settings).filter(|_| proxy_ready) {
        problems.push(reason);
    }
    let needs_login = can_sign_in && (!proxy_ready || (settings.model.provider == Provider::Anthropic && !signed_in));
    println!("{}", serde_json::json!({
        "model": settings.model.to_string(),
        "signed_in": signed_in,
        "docker_running": docker,
        "proxy_ready": proxy_ready,
        "needs_login": needs_login,
        "problems": problems,
    }));
    Ok(())
}

fn doctor(settings: &Settings) {
    let ok = style("✓").green();
    let bad = style("✗").red();
    let todo = style("•").yellow();
    let mark = |good: bool| if good { ok.clone() } else { bad.clone() };

    println!("{} ffmpeg", mark(which("ffmpeg").is_some()));
    if transcribe::is_downloaded(settings) {
        println!("{ok} Whisper model downloaded ({})", settings.whisper_model);
    } else {
        println!("{todo} Whisper model ({}) downloads automatically on first use (~1.6 GB)", settings.whisper_model);
    }
    if diarize::is_downloaded(settings) {
        println!("{ok} Speaker-detection models downloaded (for single-track imports)");
    } else {
        println!("{todo} Speaker-detection models download automatically on first single-track import (~50 MB)");
    }
    let app = capture::app_path();
    println!("{} Recorder app {} ({})", mark(app.exists()), if app.exists() { "built" } else { "not built — run mac/build.sh" },
             app.display());
    println!("{ok} Analysis model: {} {}", settings.model, style("(change: ic config set model …, or --model)").dim());
    match (auth::has_login(), which("ant").is_some()) {
        (_, false) => println!("{todo} Anthropic's CLI (used for Claude sign-in) isn't installed — {}", auth::INSTALL_ANT),
        (true, true) => match auth::access_token() {
            Ok(_) => println!("{ok} Signed in to Claude (browser login, no API key)"),
            Err(_) => println!("{bad} Claude login expired — run: ic login"),
        },
        (false, true) => println!("{todo} Not signed in to Claude — run: ic login"),
    }
    let docker = proxy::docker_running();
    println!("{} Docker {}", mark(docker), if docker { "running" } else { "not running (needed for the LLM proxy)" });
    match LlmEndpoint::load(settings) {
        None => println!("{todo} LLM proxy not set up (needed for analysis) — run: ic proxy setup"),
        Some(e) => match proxy::readiness(&e.base_url).and_then(|_| proxy::key_info(&e)) {
            Ok(_) => {
                println!("{ok} LLM proxy up at {} and ic's key accepted", e.base_url);
                if !proxy::using_external_proxy() {
                    match proxy::has_provider_key(settings, Provider::OpenAi) {
                        true => println!("{ok} OpenAI key in the proxy"),
                        false => println!("{todo} No OpenAI key in the proxy (only needed for openai/… models) — ic proxy key openai"),
                    }
                    if proxy::has_stray_anthropic_key(settings) {
                        println!("{bad} The proxy's .env has an ANTHROPIC_API_KEY; remove it — Anthropic rejects requests \
                                  that carry both a key and your login token");
                    }
                }
            }
            Err(err) => println!("{bad} LLM proxy at {}: {err:#} — try: ic proxy start", e.base_url),
        },
    }
    println!("{}", style(format!("Data folder: {}", settings.data_dir.display())).dim());
    println!("{}", style(format!("Models folder: {}", settings.models_dir.display())).dim());
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut settings = Settings::load()?;
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
                println!("Nothing is recording.");
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
                    pipeline::ingest_file(&db, &settings, &path, &title, company, Some(speakers))?
                }
                (None, Some(mic), Some(system)) => {
                    let title = title.unwrap_or_else(|| {
                        mic.canonicalize().ok().and_then(|p| Some(p.parent()?.file_name()?.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| "Interview".into())
                    });
                    pipeline::ingest_tracks(&db, &settings, &mic, &system, &title, company)?
                }
                _ => bail!("Pass a recording file, or both --mic and --system tracks."),
            };
            println!("Imported as session {} ({}) → {}", style(session.id).bold(),
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
                println!("{}", serde_json::json!({"id": session.id, "dir": session.dir}));
                Ok(())
            }
            RecordingCmd::Finish { id, no_analyze } => {
                finish_recording(&mut open_db(&settings)?, &settings, id, !no_analyze, false)
            }
        },
        Cmd::Transcript { id, json } => transcript(&settings, id, json),
        Cmd::Swap { id } => swap(&settings, id),
        Cmd::Analyze { id, open, full } => run_analysis(&mut open_db(&settings)?, &settings, id, open, full),
        Cmd::Report { id, open, full } => show_report(&settings, id, open, full),
        Cmd::Outcome { id, result, notes } => outcome(&settings, id, result, notes),
        Cmd::Proxy { action } => match action {
            ProxyCmd::Setup => {
                proxy_setup(&settings)?;
                if !auth::has_login() {
                    println!("Next, sign in to Claude: ic login");
                }
                Ok(())
            }
            ProxyCmd::Key { provider } => proxy_key(&settings, provider),
            ProxyCmd::Start => {
                proxy::start(&settings)?;
                proxy::wait_ready(&proxy::base_url(&settings), Duration::from_secs(300))?;
                println!("{} LLM proxy running at {}", style("✓").green(), proxy::base_url(&settings));
                Ok(())
            }
            ProxyCmd::Stop => proxy::stop(&settings),
            ProxyCmd::Status => proxy_status(&settings),
        },
        Cmd::Login => login(&settings),
        Cmd::Config { action } => match action {
            ConfigCmd::Show => {
                config_show(&settings);
                Ok(())
            }
            ConfigCmd::Set { key, value } => config_set(&settings, key, &value),
        },
        Cmd::Doctor { json: true } => doctor_json(&settings),
        Cmd::Doctor { json: false } => {
            doctor(&settings);
            Ok(())
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e:#}", style("Error:").red().bold());
        std::process::exit(1);
    }
}
