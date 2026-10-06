//! `ic` — the Janus command line.

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
use interview_coach::config::{FileConfig, ModelRef, Provider, ScorerRef, Settings};
use interview_coach::db::Db;
use interview_coach::events::JsonEvents;
use interview_coach::label;
use interview_coach::library;
use interview_coach::llm;
use interview_coach::merge::to_turns;
use interview_coach::models::{OutcomeResult, Status, Step, YOU, fmt_ts, speaker_label};
use interview_coach::pipeline;
use interview_coach::progress::Progress;
use interview_coach::proxy::{self, KeyTarget, LlmEndpoint};
use interview_coach::report;
use interview_coach::scoring::{JevScorer, Scorer};
use interview_coach::session_view;
use interview_coach::setup;
use interview_coach::steps::{self, StageStatus};
use interview_coach::tools::{Origin, Tool};
use interview_coach::video;
use interview_coach::{errln, out, outln};

#[derive(Parser)]
#[command(
    name = "ic",
    about = "Janus — record, transcribe, and get coached on your interviews.",
    version
)]
struct Cli {
    /// Model for analysis, e.g. anthropic/claude-opus-5-5 or openai/gpt-5.6, or "cheapest" for the
    /// cheapest one available (by list price; see `ic models`). Overrides the configured default.
    #[arg(long, global = true, value_name = "PROVIDER/MODEL")]
    model: Option<String>,
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
        /// Also record the call's window (Zoom, Teams, Meet, …) to video. Needs Screen Recording
        /// permission for ICRecorder; the audio is recorded either way.
        #[arg(long)]
        video: bool,
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
    /// Used by the Janus app, which records in-process and hands the result to ic.
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
        /// Machine-readable output (used by the Janus app).
        #[arg(long)]
        json: bool,
        /// Include archived and deleted interviews (and, with --json, every role).
        #[arg(long)]
        all: bool,
    },
    /// Change an interview's title, company, role or round.
    Edit {
        id: i64,
        #[arg(long)]
        title: Option<String>,
        /// The company, as you'd enter it (the report reads it).
        #[arg(long, conflicts_with = "no_company")]
        company: Option<String>,
        #[arg(long)]
        no_company: bool,
        /// File it under this role (created if it's new) at its company.
        #[arg(long, conflicts_with_all = ["role_id", "no_role"])]
        role: Option<String>,
        /// File it under an existing role, by id (see: ic role list).
        #[arg(long, conflicts_with = "no_role")]
        role_id: Option<i64>,
        /// Take it out of its role.
        #[arg(long)]
        no_role: bool,
        /// The round: recruiter_screen, hiring_manager, technical, behavioral, case, panel, final,
        /// informational, other, or none.
        #[arg(long)]
        round: Option<String>,
    },
    /// Archive interviews (hidden from the main list; --undo brings them back).
    Archive {
        #[arg(required = true)]
        ids: Vec<i64>,
        #[arg(long)]
        undo: bool,
    },
    /// Move interviews to Recently Deleted, where they stay for 30 days.
    Delete {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Bring interviews back from Recently Deleted.
    Restore {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Erase interviews in Recently Deleted for good, now.
    Erase {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Erase interviews deleted more than 30 days ago (--now: everything in Recently Deleted).
    EmptyDeleted {
        #[arg(long)]
        now: bool,
    },
    /// The roles you're interviewing for: where each stands, rename, archive, merge.
    Role {
        #[command(subcommand)]
        action: RoleCmd,
    },
    /// Rename or archive a company (its roles and interviews).
    Company {
        #[command(subcommand)]
        action: CompanyCmd,
    },
    /// Find interviews by title, company, role, or anything said in them.
    Search {
        query: String,
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
    /// Everything about one session as JSON (used by the Janus app).
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
    /// The models a report can be written with, and their list prices.
    Models {
        #[arg(long)]
        json: bool,
    },
    /// Rebuild how the room felt for an analysed interview (no new Claude analysis; uses Jev when
    /// its key is set).
    Timeline { id: i64 },
    /// What Janus shares (questions, anonymous diagnostics) and how to turn it off. `--seen` records
    /// that it was shown (the app's notice window); nothing is shared before then.
    PrivacyNotice {
        #[arg(long)]
        seen: bool,
    },
    /// Every interviewer question from your reviews, merged across interviews, with how your answers
    /// went: the registry mock interviews draw from.
    Questions {
        /// Only questions asked by this company.
        #[arg(long)]
        company: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The shared question registry: pull the approved questions, withdraw what you shared, and
    /// (maintainers, with wrangler) moderate contributions.
    Registry {
        #[command(subcommand)]
        action: RegistryCmd,
    },
    /// Practise with a mock interview: an interviewer agent asks real questions from your registry
    /// out loud (the app's Mock interview), and the review scores it like any other.
    Mock {
        #[command(subcommand)]
        action: MockCmd,
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
    /// Sign in to Claude or ChatGPT through your browser (no API key needed).
    Login {
        /// Browser sign-in provider. OpenAI uses Sign in with ChatGPT.
        #[arg(long, default_value = "anthropic")]
        provider: Provider,
        /// Reauthorize a saved ChatGPT registration (its issued client ID).
        #[arg(long)]
        account: Option<String>,
        /// Explicitly request permission to use your ChatGPT plan.
        #[arg(long)]
        enable_plan: bool,
        /// JSON-lines events for the app (the sign-in URL, a code prompt); a code is read from stdin.
        #[arg(long, hide = true)]
        events: bool,
        /// Claude: sign out first. OpenAI: add a separate account, keeping existing registrations.
        #[arg(long)]
        switch: bool,
    },
    /// Sign out of the selected provider (only Janus's session).
    Logout {
        #[arg(long, default_value = "anthropic")]
        provider: Provider,
        #[arg(long, hide = true)]
        events: bool,
    },
    /// What this Mac still needs before Janus works, and the steps that set it up.
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
enum RegistryCmd {
    /// Download the approved shared questions now.
    Pull,
    /// Share one interview's questions now (when sharing is on; reviews do it by themselves).
    Share { id: i64 },
    /// Withdraw everything this Mac shared.
    Withdraw,
    /// Maintainers: contributions waiting for review.
    Pending,
    /// Maintainers: publish contributions (merged into the same approved question when there is one).
    Approve {
        ids: Vec<i64>,
        /// Publish with this wording instead (one contribution at a time).
        #[arg(long = "as")]
        as_text: Option<String>,
    },
    /// Maintainers: turn contributions down.
    Reject { ids: Vec<i64> },
}

#[derive(Subcommand)]
enum MockCmd {
    /// Plan a mock interview and create its session; prints {"id", "dir", "plan"} as JSON.
    Begin {
        #[arg(long)]
        company: Option<String>,
        #[arg(long)]
        role: Option<String>,
        /// The round, e.g. "Hiring manager".
        #[arg(long)]
        round: Option<String>,
        /// How many questions.
        #[arg(long, default_value_t = 5)]
        count: usize,
    },
    /// Run the interviewer for a mock session (the app drives it): JSON-lines events out, each
    /// answer in as {"answer": "<wav>"} on stdin; {"stop": true} ends it early.
    Run {
        id: i64,
        #[arg(long)]
        events: bool,
    },
}

#[derive(Subcommand)]
enum RoleCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    /// interviewing, offer, accepted, rejected or withdrawn.
    Status {
        id: i64,
        status: String,
    },
    Rename {
        id: i64,
        title: String,
    },
    Archive {
        id: i64,
        #[arg(long)]
        undo: bool,
    },
    /// Move every interview of one role into another, and remove the first.
    Merge {
        from: i64,
        into: i64,
    },
}

#[derive(Subcommand)]
enum CompanyCmd {
    Rename {
        old: String,
        new: String,
    },
    Archive {
        name: String,
        #[arg(long)]
        undo: bool,
    },
}

#[derive(Subcommand)]
enum EvalCmd {
    /// Score every labelled item with each arm, then apply the set's rule in docs/eval/.
    Scorers {
        /// What to score: answers (your answers' checks), interviewer (the timeline's turn checks),
        /// or video (the video cues against your labelled clips).
        #[arg(long, value_enum, default_value_t = EvalSet::Answers)]
        set: EvalSet,
        /// The labelled items (default: tests/fixtures/answers.jsonl or interviewer_turns.jsonl;
        /// for video, the clips you labelled: ~/InterviewCoach/eval/video/clips.jsonl).
        #[arg(long)]
        items: Option<PathBuf>,
        /// Any of jev, haiku, sonnet, opus (default: all four); for video, video-v1.1 and video-v1
        /// (default: both).
        #[arg(long, value_delimiter = ',')]
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
    /// Cut interviews recorded with video into the clips docs/eval/video-decision.md scores, ready
    /// to label. Adding an interview again keeps its labels.
    Clips {
        /// The interviews (ids from `ic list`).
        #[arg(required = true)]
        ids: Vec<i64>,
        /// designed: staged calls (for tuning); realistic: the test set.
        #[arg(long, value_enum, default_value_t = ClipOrigin::Realistic)]
        origin: ClipOrigin,
        /// Where the labels live (default: ~/InterviewCoach/eval/video, outside the repository).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Correct what the video showed during one of your answers (the app's Correct video cues):
    /// saved as a label for measuring accuracy, apart from the blind clip labels.
    Correct {
        /// The interview.
        id: i64,
        /// When the answer starts, in seconds (as the session's video answers list it).
        #[arg(long)]
        start: f64,
        /// What you saw, as JSON (the labelling page's checks): read from stdin when absent.
        #[arg(long)]
        labels: Option<String>,
    },
    /// How capture and face reading went in every interview recorded with video: what to fix
    /// before trusting (or labelling) its cues.
    VideoHealth {
        #[arg(long)]
        json: bool,
    },
    /// Label the clips in the browser, on a page served to this Mac only.
    Label {
        /// Where the labels live (default: ~/InterviewCoach/eval/video).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Port to serve on (default: any free one).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Print the page's link without opening it.
        #[arg(long)]
        no_open: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ClipOrigin {
    Designed,
    Realistic,
}

impl ClipOrigin {
    fn as_str(self) -> &'static str {
        match self {
            ClipOrigin::Designed => "designed",
            ClipOrigin::Realistic => "realistic",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EvalSet {
    Answers,
    Interviewer,
    Video,
}

impl EvalSet {
    fn check_set(self) -> interview_coach::scoring::CheckSet {
        match self {
            EvalSet::Answers => interview_coach::scoring::answer_set(),
            EvalSet::Interviewer => interview_coach::temperature::interviewer_set(),
            EvalSet::Video => interview_coach::video_eval::video_set(),
        }
    }

    fn default_items(self, settings: &Settings) -> PathBuf {
        match self {
            EvalSet::Answers => PathBuf::from("tests/fixtures/answers.jsonl"),
            EvalSet::Interviewer => PathBuf::from("tests/fixtures/interviewer_turns.jsonl"),
            EvalSet::Video => label::default_dir(&settings.data_dir).join(label::CLIPS_FILE),
        }
    }

    fn default_out(self) -> PathBuf {
        PathBuf::from(match self {
            EvalSet::Answers => "dist/eval",
            EvalSet::Interviewer => "dist/eval/interviewer",
            EvalSet::Video => "dist/eval/video",
        })
    }

    fn default_arms(self) -> Vec<String> {
        let names: &[&str] = match self {
            EvalSet::Video => &["video-v1.1", "video-v1"],
            _ => &["jev", "haiku", "sonnet", "opus"],
        };
        names.iter().map(|s| s.to_string()).collect()
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
    /// Share your interviews' questions, names removed, with the shared registry: on or off.
    ShareQuestions,
    /// Include the company with shared questions (moderation only, never published): on or off.
    ShareCompany,
    /// Send anonymous, content-free diagnostics: on or off.
    Diagnostics,
    /// The shared registry's address.
    RegistryUrl,
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
    s.parse()
        .map_err(|_| "expected one of: recording, transcript, report, next".to_string())
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
        self.bar
            .set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
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
        self.bar.set_style(
            ProgressStyle::with_template(template)
                .unwrap()
                .progress_chars("━╸─"),
        );
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
    outln!(
        "{} {} turns. View it with {}",
        style(format!("Transcribed session {id}:")).green(),
        to_turns(&result.segments).len(),
        style(format!("ic transcript {id}")).bold()
    );
    Ok(())
}

/// Why the default model can't be used with the current setup, if it can't.
fn analysis_blocker(settings: &Settings) -> Option<String> {
    blocker_for(settings, &settings.model).or_else(||
        (!proxy::using_external_proxy() && !interview_coach::jev_auth::has_key(settings))
            .then(|| "Jev evaluation requires a TypeSafe key. Add it in Setup (or: ic proxy key typesafe)".into()))
}

/// Why `model` can't be used with the current setup, if it can't.
fn blocker_for(settings: &Settings, model: &ModelRef) -> Option<String> {
    if proxy::using_external_proxy() && LlmEndpoint::load(settings).is_none() {
        return Some(
            "IC_LLM_URL requires IC_LLM_KEY; configure both or remove the proxy override".into(),
        );
    }
    match model.provider {
        Provider::Anthropic => (!auth::has_login())
            .then(|| "you're not signed in to Claude — sign in from Setup (or: ic login)".into()),
        Provider::OpenAi if proxy::using_external_proxy() => None,
        Provider::OpenAi => match interview_coach::openai_auth::status(settings) {
            Err(e) => Some(e.to_string()),
            Ok(status) if !status.using_api_key && status.active.is_some() => {
                if !status.signed_in { Some("sign in with ChatGPT again in Setup (or: ic login --provider openai)".into()) }
                else if !status.plan_enabled { Some("enable ChatGPT plan usage in Setup, or explicitly choose an API key".into()) }
                else { None }
            }
            Ok(status) => (!status.api_key && proxy::legacy_openai_key(settings).is_none())
                .then(|| "Continue with ChatGPT in Setup (or: ic login --provider openai), or add an API key".into()),
        },
    }
}

fn run_analysis(
    db: &mut Db,
    settings: &Settings,
    model: &ModelRef,
    id: i64,
    open: bool,
    full: bool,
) -> Result<()> {
    if let Some(reason) = blocker_for(settings, model) {
        bail!("Can't analyse yet: {reason}\nThen run: ic run report {id}");
    }
    if !proxy::using_external_proxy() && !interview_coach::jev_auth::has_key(settings) {
        bail!(
            "Jev evaluation requires a TypeSafe key. Add it in Setup (or: ic proxy key typesafe), then rerun the report. Your recording and transcript are kept."
        );
    }
    let mut ui = Ui::new();
    let client = llm::configured_client(settings, model)?;
    let jev_client = interview_coach::llm::jev::Client::configured(settings)?;
    let evaluator = JevScorer {
        client: &jev_client,
        model: match &settings.scorer {
            ScorerRef::Jev(model) => model.clone(),
            // Legacy off/Claude settings no longer disable the standard product's Jev evaluations.
            _ => interview_coach::llm::jev::DEFAULT_MODEL.into(),
        },
    };
    let extras = pipeline::ReportExtras {
        checker: Some(&evaluator),
        timeline: Some(&evaluator),
        require_evaluation: true,
    };
    let stored = pipeline::analyze_session_with(db, client.as_ref(), model, id, extras, &mut ui);
    ui.finish();
    let session = db.get_session(id)?;
    let has_video = Path::new(&session.dir).join(video::VIDEO_FILE).exists();
    interview_coach::diagnostics::capture(settings, "report_finished", serde_json::json!({
        "ok": stored.is_ok(), "provider": model.provider.as_str(), "mode": session.mode.as_str(), "video": has_video,
        "practice": session.practice, "minutes": interview_coach::diagnostics::minutes(session.duration_s.unwrap_or(0.0)),
    }));
    let stored = stored?;
    let outcome = db.get_outcome(id)?;
    let path = report::write_pages(db, &session, outcome.as_ref())?.context("no report page")?;
    report::print_report(&session, &stored, outcome.as_ref(), full);
    if open {
        Command::new("open").arg(path).status()?;
    }
    // Sharing is extra: a failure is reported, never fails the review. The first time, the notice is
    // shown instead, and sharing starts with the next review.
    if settings.share_questions && !interview_coach::privacy::seen(settings) {
        outln!("\n{}\n", style(interview_coach::privacy::NOTICE).dim());
        interview_coach::privacy::mark_seen(settings)?;
    } else if settings.share_questions {
        match interview_coach::registry::contribute_session(settings, db, client.as_ref(), &model.name, id) {
            Ok(0) => {}
            Ok(n) => {
                interview_coach::diagnostics::capture(settings, "questions_shared", serde_json::json!({"shared": n}));
                outln!("{}", style(format!("Shared {n} of this interview's questions (names removed) with the registry.")).dim())
            }
            Err(e) => warn(&format!("This interview's questions weren't shared: {e:#}")),
        }
    }
    Ok(())
}

/// Stage 4: next-round prep and a practice plan from the current report.
fn run_next(db: &mut Db, settings: &Settings, model: &ModelRef, id: i64, show: bool) -> Result<()> {
    if let Some(reason) = blocker_for(settings, model) {
        bail!("Can't plan next steps yet: {reason}\nThen run: ic run next {id}");
    }
    let mut ui = Ui::new();
    let client = llm::configured_client(settings, model)?;
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
            outln!(
                "{}",
                style(format!(
                    "What to do next: ic next {id} · All stages: ic steps {id}"
                ))
                .dim()
            );
        }
        Some(reason) => outln!(
            "{}",
            style(format!(
                "Skipping analysis: {reason}, then: ic run report {id} --then-later"
            ))
            .dim()
        ),
    }
    Ok(())
}

/// Re-run one stage; with `then_later`, also update every later stage that's out of date.
fn run_step(
    db: &mut Db,
    settings: &Settings,
    step: Step,
    id: i64,
    explicit_model: Option<&ModelRef>,
    speakers: Option<i64>,
    then_later: bool,
) -> Result<()> {
    run_one(db, settings, step, id, explicit_model, speakers)?;
    if then_later {
        for later in Step::ALL.iter().copied().skip_while(|s| *s != step).skip(1) {
            let state = steps::flow(db, id)?
                .into_iter()
                .find(|s| s.step == later)
                .expect("every stage has a state");
            if matches!(
                state.status,
                StageStatus::OutOfDate | StageStatus::NotRun | StageStatus::Failed
            ) {
                run_one(db, settings, later, id, explicit_model, None)?;
            }
        }
    }
    outln!();
    print_steps(db, id)
}

fn run_one(
    db: &mut Db,
    settings: &Settings,
    step: Step,
    id: i64,
    explicit_model: Option<&ModelRef>,
    speakers: Option<i64>,
) -> Result<()> {
    match step {
        Step::Recording => {
            let mut ui = Ui::new();
            let result = pipeline::reprocess_audio(db, id, &mut ui);
            ui.finish();
            result?;
            outln!(
                "{} Re-processed the audio for session {id}.",
                style("✓").green()
            );
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
            let model = explicit_model
                .cloned()
                .unwrap_or_else(|| settings.model.clone());
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
            StageStatus::OutOfDate => (
                style("⚠").yellow(),
                style("out of date".to_string()).yellow(),
            ),
            StageStatus::Failed => (style("✗").red(), style("failed".to_string()).red()),
            StageStatus::NotRun => (style("○").dim(), style("not run".to_string()).dim()),
        };
        let when = stage
            .last_run_at
            .as_deref()
            .map(|t| t.chars().take(16).collect::<String>().replace('T', " "));
        outln!(
            " {mark} {:<20} {:<12} {}  {}",
            stage.label,
            status.to_string(),
            stage.summary.as_deref().unwrap_or(""),
            style(when.unwrap_or_default()).dim()
        );
        if let Some(error) = &stage.error {
            outln!("     {}", style(error).red());
        }
    }
    if let Some(first) = view
        .stages
        .iter()
        .find(|s| s.status == StageStatus::OutOfDate)
    {
        let upstream = first
            .step
            .upstream()
            .map(|u| u.as_str())
            .unwrap_or("recording");
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

/// How `ic record` asks ICRecorder to capture.
struct Capture {
    aec: bool,
    video: bool,
    duration: Option<u32>,
}

fn record(
    settings: &Settings,
    title: Option<String>,
    company: Option<String>,
    how: Capture,
    yes: bool,
    analyze: bool,
) -> Result<()> {
    let question = if how.video {
        "Has everyone on the call agreed to be recorded, including video?"
    } else {
        "Has everyone on the call agreed to be recorded?"
    };
    if !yes && !confirm(question)? {
        bail!("Not recording. Some places require everyone's consent to record a call.");
    }
    let mut db = open_db(settings)?;
    let title = title.unwrap_or_else(|| {
        chrono::Local::now()
            .format("Interview %Y-%m-%d %H:%M")
            .to_string()
    });
    let session = pipeline::create_recording_session(&db, settings, &title, company)?;
    let dir = PathBuf::from(&session.dir);

    let started = capture::launch(&dir, how.duration, how.aec, how.video).and_then(|_| {
        let spinner = Ui::new();
        spinner
            .bar
            .set_message("Starting recorder — if macOS asks for permission, click Allow…");
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
        ui.bar.set_message(format!(
            "{} Recording {} — press Ctrl+C to stop",
            style("●").red(),
            fmt_ts(t0.elapsed().as_secs_f64())
        ));
        std::thread::sleep(Duration::from_millis(250));
    }
    ui.finish();
    finish_recording(&mut db, settings, session.id, analyze, true)
}

/// `stop_recorder`: signal ICRecorder.app to stop first (the CLI flow). The Janus app
/// stops its own in-process recording before calling `ic recording finish`.
fn finish_recording(
    db: &mut Db,
    settings: &Settings,
    id: i64,
    analyze: bool,
    stop_recorder: bool,
) -> Result<()> {
    let mut session = db.get_session(id)?;
    let dir = PathBuf::from(&session.dir);
    if stop_recorder {
        capture::stop(&dir)?;
    } else if capture::is_running(&dir) {
        bail!("session {id} is still recording");
    } else if capture::read_report(&dir).is_none() {
        db.set_status(id, Status::Failed, Some("No recording was made".into()))?;
        bail!(
            "session {id} has no finished recording (no recorder.json in {})",
            dir.display()
        );
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
        db.set_status(
            id,
            Status::Failed,
            Some("Recorder produced no audio files".into()),
        )?;
        bail!(
            "The recorder produced no audio. See {}",
            dir.join("recorder.log").display()
        );
    }
    // The raw 48 kHz WAVs stay next to the FLACs for now: they're what we'd inspect if the recorder
    // misbehaves. Revisit deleting them once real calls have validated the recorder.
    let mut ui = Ui::new();
    let processed = pipeline::process_recording(db, id, &mic, &system, &mut ui);
    ui.finish();
    let codes: Vec<String> = report["warnings"].as_array().into_iter().flatten().filter_map(|w| w["code"].as_str().map(String::from)).collect();
    interview_coach::diagnostics::capture(settings, "recording_processed", serde_json::json!({
        "ok": processed.is_ok(), "video": dir.join(video::VIDEO_FILE).exists(), "warnings": codes,
        "faces_read": dir.join(video::FACES_FILE).exists(),
    }));
    session = processed.with_context(|| {
        format!(
            "Couldn't process the recording; the raw audio is still in {}",
            dir.display()
        )
    })?;
    outln!(
        "Saved session {} ({}) → {}",
        style(id).bold(),
        fmt_ts(session.duration_s.unwrap_or(0.0)),
        dir.display()
    );
    after_transcription(db, settings, id, analyze)
}

/// `ic list --json`: the interviews in the main list (not archived or deleted). With `--all`, every
/// interview (flagged) and every role, as the app shows them.
fn list_json(db: &Db, all: bool) -> Result<()> {
    let lib = library::library(db)?;
    if all {
        outln!("{}", serde_json::to_string(&lib)?);
    } else {
        let shown: Vec<_> = lib
            .sessions
            .into_iter()
            .filter(|s| !s.archived && s.deleted_days_left.is_none())
            .collect();
        outln!("{}", serde_json::to_string(&shown)?);
    }
    Ok(())
}

fn list(settings: &Settings, json: bool, all: bool) -> Result<()> {
    let db = open_db(settings)?;
    if json {
        return list_json(&db, all);
    }
    let sessions: Vec<_> = db
        .list_sessions()?
        .into_iter()
        .filter(|s| all || (s.archived_at.is_none() && s.deleted_at.is_none()))
        .collect();
    if sessions.is_empty() {
        if db.list_sessions()?.is_empty() {
            outln!("No sessions yet. Try: ic record   or   ic import path/to/interview.m4a");
        } else {
            outln!("Everything is archived or in Recently Deleted. See them with: ic list --all");
        }
        return Ok(());
    }
    let (verdicts, outcomes, companies) = (
        db.latest_verdicts()?,
        db.all_outcomes()?,
        db.latest_companies()?,
    );
    let header = [
        "ID", "Date", "Title", "Company", "Length", "Status", "Verdict", "Outcome",
    ];
    let rows: Vec<[String; 8]> = sessions
        .iter()
        .map(|s| {
            [
                s.id.to_string(),
                s.created_at.chars().take(10).collect(),
                s.title.clone(),
                s.company
                    .clone()
                    .or_else(|| companies.get(&s.id).cloned())
                    .unwrap_or_default(),
                fmt_ts(s.duration_s.unwrap_or(0.0)),
                s.status.to_string(),
                verdicts
                    .get(&s.id)
                    .map(|v| v.label().to_string())
                    .unwrap_or_default(),
                outcomes
                    .get(&s.id)
                    .map(|o| o.result.label().to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .map(|r| console::measure_text_width(&r[i]))
                .chain([header[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<String>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| console::pad_str(c, *w, console::Alignment::Left, None).into_owned())
            .collect::<Vec<_>>()
            .join("  ")
    };
    outln!(
        "{}",
        style(line(header.iter().map(|h| h.to_string()).collect())).bold()
    );
    for (row, s) in rows.iter().zip(&sessions) {
        let text = line(row.to_vec());
        outln!(
            "{}",
            if s.status == Status::Failed {
                style(text).red().to_string()
            } else {
                text
            }
        );
    }
    Ok(())
}

fn transcript(settings: &Settings, id: i64, json: bool) -> Result<()> {
    let db = open_db(settings)?;
    let session = db.get_session(id)?;
    let segments = db.get_segments(id)?;
    if segments.is_empty() {
        let hint = session
            .error
            .map(|e| format!(" Error: {e}"))
            .unwrap_or_else(|| format!(" Run: ic transcribe {id}"));
        bail!(
            "Session {id} has no transcript yet ({}).{hint}",
            session.status
        );
    }
    if json {
        let plain: Vec<_> = segments
            .into_iter()
            .map(|s| interview_coach::models::Segment { words: vec![], ..s })
            .collect();
        outln!("{}", serde_json::to_string_pretty(&plain)?);
        return Ok(());
    }
    outln!(
        "{}\n",
        style(format!(
            "━━ {} · {} ━━",
            session.title,
            fmt_ts(session.duration_s.unwrap_or(0.0))
        ))
        .bold()
    );
    for t in to_turns(&segments) {
        let name = speaker_label(&t.speaker);
        let name = if t.speaker == YOU {
            style(format!("{name}:")).cyan().bold()
        } else {
            style(format!("{name}:")).magenta().bold()
        };
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
    // The current version (what report.html opens), e.g. after going back to an earlier model's.
    let Some(stored) = pipeline::current_report(&db, id)?.or(db.latest_analysis(id)?) else {
        bail!("Session {id} hasn't been analysed yet. Run: ic analyze {id}");
    };
    let outcome = db.get_outcome(id)?;
    let path = report::write_pages(&db, &session, outcome.as_ref())?.context("no report page")?;
    report::print_report(&session, &stored, outcome.as_ref(), full);
    if open {
        Command::new("open").arg(path).status()?;
    }
    Ok(())
}

fn list_models(settings: &Settings, json: bool) -> Result<()> {
    let offers = interview_coach::catalog::fetch(settings)?;
    if json {
        outln!("{}", serde_json::to_string(&offers)?);
        return Ok(());
    }
    let money = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("${v:.2}"));
    outln!(
        "{}",
        style(format!(
            "{:<44} {:>9} {:>9} {:>14}",
            "Model", "In /M", "Out /M", "Typical report"
        ))
        .bold()
    );
    for o in &offers {
        let mark = if o.cheapest {
            style(" ← cheapest").green().to_string()
        } else {
            String::new()
        };
        outln!(
            "{:<44} {:>9} {:>9} {:>14}{mark}",
            o.model,
            money(o.input_per_mtok),
            money(o.output_per_mtok),
            money(o.typical_report)
        );
    }
    outln!("{}", style(if proxy::using_external_proxy() {
        "List prices from LiteLLM are estimates; subscription usage can be billed differently."
    } else { "Native model choices come from your accounts. Prices are not estimated; ChatGPT usage is managed in ChatGPT Settings." }).dim());
    outln!(
        "{}",
        style(
            "Re-run a report with one: ic run report <id> --model <model>   (or --model cheapest)"
        )
        .dim()
    );
    Ok(())
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

struct EditArgs {
    title: Option<String>,
    company: Option<String>,
    no_company: bool,
    role: Option<String>,
    role_id: Option<i64>,
    no_role: bool,
    round: Option<String>,
}

fn edit(db: &Db, id: i64, a: EditArgs) -> Result<()> {
    let before = db.get_session(id)?;
    let mut changed = vec![];
    if let Some(title) = a
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != before.title)
    {
        db.set_title(id, title)?;
        changed.push("title");
    }
    let company = if a.no_company {
        Some(None)
    } else {
        a.company
            .as_deref()
            .map(|c| Some(c.trim()).filter(|c| !c.is_empty()))
    };
    if let Some(company) = company.filter(|c| c.map(String::from) != before.company) {
        db.set_company(id, company)?;
        changed.push("company");
        // Its role moves with it: the same title, at the new company.
        if a.role.is_none()
            && a.role_id.is_none()
            && !a.no_role
            && let Some(role) = before.role_id.map(|r| db.role(r)).transpose()?
            && role.company.as_deref().map(library::company_key)
                != company.map(library::company_key)
        {
            let moved = db.find_or_create_role(company, &role.title)?;
            db.set_session_role(id, Some(moved.id))?;
        }
    }
    if a.no_role {
        db.set_session_role(id, None)?;
    } else if let Some(role_id) = a.role_id {
        db.role(role_id)?;
        db.set_session_role(id, Some(role_id))?;
    } else if let Some(title) = a.role.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let session = db.get_session(id)?;
        let inferred = db.latest_companies()?.remove(&id);
        let current_role = session.role_id.map(|r| db.role(r)).transpose()?;
        let company =
            library::display_company(&session, current_role.as_ref(), inferred.as_deref());
        let role = db.find_or_create_role(company.as_deref(), title)?;
        db.set_session_role(id, Some(role.id))?;
    }
    if let Some(round) = a.round.as_deref() {
        let stage = match round {
            "none" | "" => None,
            r => Some(
                r.parse::<interview_coach::models::Stage>()
                    .map_err(|e| anyhow::anyhow!(e))?,
            ),
        };
        db.set_stage(id, stage)?;
    }
    outln!("{} Saved.", style("✓").green());
    if !changed.is_empty() {
        outln!(
            "{}",
            style(format!(
                "The report reads the {}, so re-running it will make a new version.",
                changed.join(" and ")
            ))
            .dim()
        );
    }
    Ok(())
}

fn role_cmd(db: &Db, action: RoleCmd) -> Result<()> {
    match action {
        RoleCmd::List { json } => {
            let lib = library::library(db)?;
            if json {
                outln!("{}", serde_json::to_string(&lib.roles)?);
            } else {
                for r in &lib.roles {
                    let n = lib
                        .sessions
                        .iter()
                        .filter(|s| s.role_id == Some(r.id) && s.deleted_days_left.is_none())
                        .count();
                    outln!(
                        "{:>4}  {:<28} {:<24} {:<12} {}{}",
                        r.id,
                        r.title,
                        r.company.as_deref().unwrap_or("—"),
                        r.status_label,
                        plural(n, "interview"),
                        if r.archived { " (archived)" } else { "" }
                    );
                }
            }
        }
        RoleCmd::Status { id, status } => {
            let status: interview_coach::models::RoleStatus =
                status.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            db.set_role_status(id, status)?;
            outln!(
                "{} {} is now {}.",
                style("✓").green(),
                db.role(id)?.title,
                status.label()
            );
        }
        RoleCmd::Rename { id, title } => {
            db.rename_role(id, &title)?;
            outln!("{} Renamed.", style("✓").green());
        }
        RoleCmd::Archive { id, undo } => {
            db.set_role_archived(id, !undo)?;
            outln!(
                "{} {} {}.",
                style("✓").green(),
                if undo { "Brought back" } else { "Archived" },
                db.role(id)?.title
            );
        }
        RoleCmd::Merge { from, into } => {
            db.merge_role(from, into)?;
            outln!(
                "{} Merged into {}.",
                style("✓").green(),
                db.role(into)?.title
            );
        }
    }
    Ok(())
}

fn refresh_timeline(settings: &Settings, id: i64) -> Result<()> {
    let mut db = open_db(settings)?;
    let mut ui = Ui::new();
    let jev_client = interview_coach::llm::jev::Client::configured(settings)?;
    let jev_scorer = JevScorer {
        client: &jev_client,
        model: interview_coach::llm::jev::DEFAULT_MODEL.into(),
    };
    let scorer: Option<&dyn Scorer> = Some(&jev_scorer);
    let result = pipeline::refresh_timeline(&mut db, id, scorer, &mut ui);
    ui.finish();
    let (stored, warnings) = result?;
    if !pipeline::interviewer_evaluation_complete(&stored) {
        bail!(
            "Jev interviewer evaluation is incomplete. Saved results are kept; retry ic timeline {id} after restoring evaluation access."
        );
    }
    for w in &warnings {
        errln!("{} {w}", style("Warning:").yellow());
    }
    let session = db.get_session(id)?;
    let outcome = db.get_outcome(id)?;
    report::write_pages(&db, &session, outcome.as_ref())?;
    if stored.turn_signals.iter().any(|s| s.temperature.is_some()) {
        report::print_room(&stored, true);
    } else {
        outln!("The interviewer didn't say enough to read the room.");
    }
    Ok(())
}

fn outcome(
    settings: &Settings,
    id: i64,
    result: OutcomeResult,
    notes: Option<String>,
) -> Result<()> {
    let db = open_db(settings)?;
    let outcome = db.set_outcome(id, result, notes.as_deref())?;
    let session = db.get_session(id)?;
    report::write_pages(&db, &session, Some(&outcome))?; // every report page shows the outcome
    match pipeline::current_report(&db, id)?.or(db.latest_analysis(id)?) {
        Some(stored) => {
            outln!(
                "Recorded {} for session {id} (the analysis predicted: {}).",
                style(outcome.result.label()).bold(),
                stored.analysis.outlook.verdict.label()
            );
        }
        None => outln!(
            "Recorded {} for session {id}.",
            style(outcome.result.label()).bold()
        ),
    }
    Ok(())
}

fn read_key(target: KeyTarget, from_stdin: bool) -> Result<String> {
    let key = if from_stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    } else if let Ok(key) = std::env::var(target.env_var()) {
        outln!(
            "Using the {} key from ${}.",
            target.label(),
            target.env_var()
        );
        key
    } else {
        outln!(
            "Opening {} — create a {} API key there (or copy an existing one).",
            target.keys_url(),
            target.label()
        );
        let _ = Command::new("/usr/bin/open")
            .arg(target.keys_url())
            .status();
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
    target
        .check(&key)
        .map_err(|reason| anyhow::anyhow!("{reason} Nothing was changed."))?;
    Ok(key)
}

fn proxy_setup(settings: &Settings) -> Result<()> {
    let mut ui = Ui::new();
    let result = proxy::setup(settings, &mut ui);
    ui.finish();
    result?;
    outln!(
        "{} AI proxy running at {}.",
        style("✓").green(),
        proxy::base_url(settings)
    );
    outln!(
        "{}",
        style(format!(
            "Config: {} · Spend: ic proxy status",
            proxy::dir(settings).display()
        ))
        .dim()
    );
    Ok(())
}

fn proxy_key(settings: &Settings, target: KeyTarget, from_stdin: bool) -> Result<()> {
    if proxy::using_external_proxy() {
        bail!(
            "ic is using the shared proxy at IC_LLM_URL; add the {} key there.",
            target.label()
        );
    }
    let key = read_key(target, from_stdin)?;
    if target == KeyTarget::OpenAi {
        interview_coach::openai_auth::save_api_key(settings, &key)?;
        outln!(
            "{} OpenAI API key saved privately on this Mac. API billing is now selected.",
            style("✓").green()
        );
        return Ok(());
    }
    interview_coach::jev_auth::save_key(settings, &key)?;
    outln!(
        "{} TypeSafe key saved privately on this Mac. Jev evaluation connects directly; Docker is not required.",
        style("✓").green()
    );
    Ok(())
}

fn login(events: bool, switch: bool) -> Result<()> {
    if events {
        let mut out = JsonEvents::new();
        if switch {
            if let Err(e) = auth::logout() {
                out.error(&format!("{e:#}"));
                return Err(e);
            }
            out.stage("Signed out. Approve access in your browser with the account you want…");
        }
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
    if switch {
        let account = auth::account();
        auth::logout()?;
        outln!(
            "Signed out{}. Sign in with the account you want: if your browser picks the wrong one, switch accounts at \
                claude.ai first.",
            account.map(|a| format!(" of {a}")).unwrap_or_default()
        );
    }
    outln!(
        "{} — a browser window will open. Approve access there, then come back here.",
        style("Claude sign-in").bold()
    );
    outln!(
        "{}",
        style("If the page shows a code instead of closing, paste it at the Code: prompt below.")
            .dim()
    );
    auth::login(None)?;
    auth::access_token()?; // prove the session works before saying so
    outln!(
        "{} Signed in to Claude{} — no API key stored. ic gets short-lived tokens from this session as needed.",
        style("✓").green(),
        auth::account()
            .map(|a| format!(" as {a}"))
            .unwrap_or_default()
    );
    Ok(())
}

fn chatgpt_login(
    settings: &Settings,
    events: bool,
    new_account: bool,
    account: Option<&str>,
    enable_plan: bool,
) -> Result<()> {
    let mut out = JsonEvents::new();
    let result = interview_coach::openai_auth::login(
        settings,
        new_account,
        account,
        enable_plan,
        &mut |event| {
            if events {
                match event {
                    auth::LoginEvent::OpenUrl(url) => out.open_url(&url),
                    auth::LoginEvent::Log(message) => out.stage(&message),
                    auth::LoginEvent::NeedCode => {}
                }
            } else if let auth::LoginEvent::Log(message) = event {
                outln!("{message}");
            }
        },
    );
    match result {
        Ok(plan) => {
            if events {
                out.stage(if plan { "Signed in with ChatGPT. Choose an available OpenAI model in Setup." }
                          else { "Signed in. ChatGPT plan usage was not granted; enable it in Setup or choose an API key." });
                out.done();
            } else {
                outln!(
                    "Signed in with ChatGPT. {}",
                    if plan {
                        "Plan usage is enabled. Select an available model with ic models and ic config set model openai/<model>."
                    } else {
                        "Plan usage is not enabled. Enable it in Setup, or explicitly choose an API key."
                    }
                );
            }
            Ok(())
        }
        Err(e) => {
            if events {
                out.error(&e.to_string());
            }
            Err(e)
        }
    }
}

fn chatgpt_logout(settings: &Settings, events: bool) -> Result<()> {
    let mut out = JsonEvents::new();
    match interview_coach::openai_auth::logout(settings) {
        Ok(confirmed) => {
            let message = if confirmed {
                "Signed out of ChatGPT. Local session tokens have been cleared."
            } else {
                "Signed out locally. Remote revocation could not be confirmed; disconnect this app in ChatGPT Settings."
            };
            if events {
                out.stage(message);
                out.done();
            } else {
                outln!("{message}");
            }
            Ok(())
        }
        Err(e) => {
            if events {
                out.error(&e.to_string());
            }
            Err(e)
        }
    }
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
    outln!(
        "{}",
        style(format!("Config file: {}", settings.config_path().display())).dim()
    );
    outln!("model          {}", settings.model);
    outln!(
        "language       {}",
        settings.language.as_deref().unwrap_or("auto")
    );
    outln!("whisper_model  {}", settings.whisper_model);
    outln!("scorer         {}", settings.scorer);
    outln!("models_dir     {}", settings.models_dir.display());
    let on = |b: bool| if b { "on" } else { "off" };
    outln!("share_questions {}", on(settings.share_questions));
    outln!("share_company  {}", on(settings.share_company));
    outln!("diagnostics    {}", on(settings.diagnostics));
    outln!("registry_url   {}", settings.registry_url);
    outln!("{}", style("Environment variables IC_MODEL, IC_LANGUAGE, IC_WHISPER_MODEL, IC_MODELS_DIR override the file.").dim());
}

fn on_off(value: &str) -> Result<bool> {
    match value.to_lowercase().as_str() {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        other => bail!("{other:?} isn't on or off"),
    }
}

fn config_set(settings: &Settings, key: ConfigKey, value: &str) -> Result<()> {
    let path = settings.config_path();
    let mut file = FileConfig::read(&path)?;
    match key {
        ConfigKey::Model => {
            file.model = Some(
                value
                    .parse()
                    .map_err(|e: String| anyhow::anyhow!("model: {e}"))?,
            )
        }
        ConfigKey::Language => file.language = Some(value.to_string()),
        ConfigKey::ShareQuestions => file.share_questions = Some(on_off(value)?),
        ConfigKey::ShareCompany => file.share_company = Some(on_off(value)?),
        ConfigKey::Diagnostics => file.diagnostics = Some(on_off(value)?),
        ConfigKey::RegistryUrl => {
            anyhow::ensure!(value.starts_with("https://") || value.starts_with("http://127.0.0.1"), "registry_url must be https (or a local test server)");
            file.registry_url = Some(value.trim_end_matches('/').to_string());
        }
        ConfigKey::WhisperModel => file.whisper_model = Some(value.to_string()),
        ConfigKey::Scorer => {
            let scorer = value
                .parse()
                .map_err(|e: String| anyhow::anyhow!("scorer: {e}"))?;
            anyhow::ensure!(
                matches!(scorer, ScorerRef::Jev(_)),
                "Jev is required for standard reports. Use typesafe/<model>; compare Claude separately with ic eval scorers."
            );
            file.scorer = Some(scorer);
        }
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
        Ok(r) => outln!(
            "{} Proxy up at {} (database {})",
            style("✓").green(),
            endpoint.base_url,
            r["db"].as_str().unwrap_or("unknown")
        ),
        Err(_) => bail!(
            "The proxy at {} isn't responding. Start it with: ic proxy start",
            endpoint.base_url
        ),
    }
    let info = proxy::key_info(&endpoint)?;
    outln!(
        "{} ic's key: {} · spent so far: ${:.4}",
        style("✓").green(),
        info["key_alias"].as_str().unwrap_or("?"),
        info["spend"].as_f64().unwrap_or(0.0)
    );
    Ok(())
}

fn jev_ping(settings: &Settings) -> Result<()> {
    use interview_coach::llm::jev::{self, Question, SystemOne};
    if !proxy::using_external_proxy() && !interview_coach::jev_auth::has_key(settings) {
        bail!(
            "Jev evaluation requires a TypeSafe key. Add it in Setup, or run: ic proxy key typesafe"
        );
    }
    let client = jev::Client::configured(settings)?;
    let req = jev::Request {
        state: serde_json::json!("Help! My payouts have been failing for 3 days."),
        model: jev::DEFAULT_MODEL.into(),
        questions: vec![(
            "is_urgent".into(),
            Question::Noul {
                instructions: "Does this convey urgency?".into(),
                yes: None,
                no: None,
            },
        )],
    };
    let resp = client.ask(&req)?;
    outln!(
        "{} {} answered in {} ms: {:?}",
        style("✓").green(),
        resp.model,
        resp.latency_ms,
        resp.answers["is_urgent"]
    );
    Ok(())
}

/// Add each interview's clips to the label files. Every interview is checked before anything is
/// written, so a typo in one id adds nothing.
fn eval_clips(settings: &Settings, ids: &[i64], origin: ClipOrigin, dir: Option<PathBuf>) -> Result<()> {
    let db = open_db(settings)?;
    let mut per_interview = vec![];
    for &id in ids {
        let session = db.get_session(id)?;
        let session_dir = PathBuf::from(&session.dir);
        if !session_dir.join(video::VIDEO_FILE).is_file() {
            bail!("Interview {id} has no video. Record with video on (Janus 0.1.0-preview.14 or later).");
        }
        let segments = db.get_segments(id)?;
        if segments.is_empty() {
            bail!("Interview {id} has no transcript yet, and clips are cut from your answers. Run: ic run transcript {id}");
        }
        let faces = video::load(&session_dir)?;
        per_interview.push((id, session.title.clone(), label::session_clips(&session, &segments, faces.as_ref(), origin.as_str())));
    }
    let custom = dir.is_some();
    let dir = dir.unwrap_or_else(|| label::default_dir(&settings.data_dir));
    let store = label::Store::new(&dir);
    for (id, title, clips) in per_interview {
        let total = clips.len();
        let added = store.add_clips(clips)?;
        let clips = if total == 1 { "1 clip".to_string() } else { format!("{total} clips") };
        outln!("Interview {id} ({title}): {clips}, {} new", added.new);
        if !added.moved.is_empty() {
            warn(&format!(
                "{} already listed with a different window, since the transcript was re-run: {}. Their labels describe the old \
                 window; delete those lines from clips.jsonl to cut and label them again.",
                added.moved.len(), added.moved.join(", ")
            ));
        }
    }
    let flag = if custom { format!(" --dir {}", dir.display()) } else { String::new() };
    outln!("{}", style(format!("Label them: ic eval label{flag}")).dim());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn eval_scorers(
    settings: &Settings,
    which: EvalSet,
    items_path: Option<PathBuf>,
    arm_names: &[String],
    runs: usize,
    out: Option<PathBuf>,
    limit: Option<usize>,
    concurrency: usize,
) -> Result<()> {
    use interview_coach::eval;
    let set = which.check_set();
    let out = out.unwrap_or_else(|| which.default_out());
    let out = out.as_path();
    let mut items = eval::load_items(&items_path.unwrap_or_else(|| which.default_items(settings)))?;
    if let Some(n) = limit {
        items.truncate(n);
    }
    let arms: Vec<eval::Arm> = arm_names
        .iter()
        .map(|a| eval::Arm::parse(a))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!(e))?;
    if arms.iter().any(|a| a.name == "jev")
        && !proxy::using_external_proxy()
        && !interview_coach::jev_auth::has_key(settings)
    {
        bail!(
            "Jev evaluation requires a TypeSafe key. Add it in Setup, or run: ic proxy key typesafe"
        );
    }
    if arms.iter().any(|a| a.name != "jev") && !auth::has_login() {
        bail!("The Claude arms need you signed in to Claude (Setup in the app, or: ic login).");
    }
    outln!(
        "Scoring {} {} × {runs} runs with {} → {}",
        items.len(),
        if set.id == "answers" {
            "answers"
        } else {
            "turns"
        },
        arms.iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        out.display()
    );
    let results = eval::run(
        &eval::Plan {
            set: &set,
            items: &items,
            arms: &arms,
            runs,
            concurrency,
            out_dir: out,
        },
        settings,
        &|r, done, total| {
            let status = match &r.result {
                Ok(a) => format!("{} ms", a.latency_ms),
                Err(e) => format!("failed: {}", e.lines().next().unwrap_or_default()),
            };
            errln!(
                "[{done}/{total}] {:<6} {:<28} run {} {status}",
                r.arm,
                r.item,
                r.run
            );
        },
    )?;
    let stats = eval::summarize(&results, &items, &arms, &set);
    let decisions = eval::decide(&stats, &arms, &set);
    let fallback = arms
        .iter()
        .filter(|a| a.name != "jev")
        .min_by_key(|a| a.cost_rank())
        .map(|a| a.name.clone());
    let cascade_rows = match (&fallback, arms.iter().any(|a| a.name == "jev")) {
        (Some(f), true) => eval::cascade(&results, &items, f, &set),
        _ => vec![],
    };
    let mut md = eval::markdown(
        &stats,
        &decisions,
        &cascade_rows,
        fallback.as_deref().unwrap_or("-"),
        items.len(),
        runs,
        &set,
    );
    md += &eval::disagreements(&results, &items, &arms, &set);
    let path = eval::write_summary(out, &stats, &decisions, &md)?;
    for d in &decisions {
        outln!(
            "{:<22} {}",
            d.check,
            match (&d.winner, &d.fallback) {
                (Some(w), _) => style(w.clone()).green().to_string(),
                (None, Some(f)) => style(format!("none qualifies (most accurate: {f})"))
                    .yellow()
                    .to_string(),
                (None, None) => style("none qualifies".to_string()).yellow().to_string(),
            }
        );
    }
    outln!("Summary: {}", path.display());
    Ok(())
}

/// `ic eval scorers --set video`: the video cues against the clips labelled with `ic eval label`,
/// under docs/eval/video-decision.md's rule. Everything runs on this Mac.
fn eval_video(
    settings: &Settings,
    items_path: Option<PathBuf>,
    arm_names: &[String],
    runs: usize,
    out: Option<PathBuf>,
    limit: Option<usize>,
    concurrency: usize,
) -> Result<()> {
    use interview_coach::{eval, video_eval};
    let set = video_eval::video_set();
    let path = items_path.unwrap_or_else(|| EvalSet::Video.default_items(settings));
    let mut items = video_eval::load_clips(&path)?;
    if let Some(n) = limit {
        items.truncate(n);
    }
    if items.is_empty() {
        bail!("No labelled clips in {}. Cut some with `ic eval clips <interview ids>`, then label them with `ic eval label`.", path.display());
    }
    let labels_dir = path.parent().unwrap_or(Path::new("."));
    let marks = label::Store::new(labels_dir).read(label::YOU_FILE)?;
    let corrections_path = labels_dir.join(label::CORRECTIONS_FILE);
    let corrections = if corrections_path.exists() { video_eval::load_clips(&corrections_path)? } else { vec![] };
    let arms: Vec<eval::Arm> = arm_names.iter().map(|a| eval::Arm::parse(a)).collect::<Result<_, _>>().map_err(|e| anyhow::anyhow!(e))?;
    if let Some(a) = arms.iter().find(|a| !matches!(a.kind, eval::ArmKind::Local { .. })) {
        bail!("{} doesn't score video; use video-v1.1 or video-v1", a.name);
    }
    let out = out.unwrap_or_else(|| EvalSet::Video.default_out());
    let mut ui = Ui::new();
    let everything: Vec<eval::Item> = items.iter().chain(&corrections).cloned().collect();
    let read = video_eval::read_dense(&video_eval::session_dirs(&everything, &marks), &mut ui);
    ui.finish();
    for w in read? {
        warn(&w);
    }
    outln!("Scoring {} clips and {} corrections × {runs} runs with {} → {}", items.len(), corrections.len(),
           arms.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "), out.display());
    let plan = eval::Plan { set: &set, items: &everything, arms: &arms, runs, concurrency, out_dir: &out };
    let results = eval::run(&plan, settings, &|r, done, total| {
        if let Err(e) = &r.result {
            errln!("[{done}/{total}] {} {} run {} failed: {}", r.arm, r.item, r.run, e.lines().next().unwrap_or_default());
        }
    })?;
    let stats = eval::summarize(&results, &items, &arms, &set);
    let decisions = video_eval::decisions(&results, &items, &arms, &set);
    let mut md = eval::markdown(&stats, &[], &[], "-", items.len(), runs, &set);
    md += &video_eval::markdown(&results, &items, &arms, &set, &decisions);
    let mut you = vec![];
    if !marks.is_empty() {
        for arm in &arms {
            if let eval::ArmKind::Local { method } = &arm.kind
                && let Some(params) = video::Params::named(method)
            {
                you.push((arm.name.clone(), video_eval::you_rows(&marks, &params)?));
            }
        }
        md += &video_eval::you_markdown(&you);
    }
    md += &video_eval::corrections_markdown(&results, &corrections, &arms, &set);
    let sweep = video_eval::sweep(&items, &video::CURRENT, &set)?;
    md += &video_eval::sweep_markdown(&sweep, &set, &video::CURRENT);
    let gate = video_eval::suggested_gate(&decisions, video::CURRENT.name);
    md += &format!("\n## The gate these results suggest ({})\n\nFor `video::GATE`, once the rule is met for real: the \
                    passing checks show plainly, failing ones are hidden, and the rest stay experimental.\n\n```rust\n{gate}\n```\n",
                   video::CURRENT.name);
    md += &eval::disagreements(&results, &items, &arms, &set);
    let summary = eval::write_summary(&out, &stats, &[], &md)?;
    std::fs::write(out.join("video.json"), serde_json::to_string_pretty(&serde_json::json!({
        "decisions": decisions, "you": you, "sweep": sweep, "suggested_gate": gate,
    }))?)?;
    for d in decisions.iter().filter(|d| d.arm == video::CURRENT.name) {
        let result = if d.reasons.is_empty() {
            style("passes".to_string()).green().to_string()
        } else if d.undecided {
            style(format!("not yet: {}", d.reasons.join("; "))).dim().to_string()
        } else {
            style(format!("fails: {}", d.reasons.join("; "))).yellow().to_string()
        };
        outln!("{:<12} {result}", d.check);
    }
    outln!("Summary: {}", summary.display());
    Ok(())
}

/// `ic questions`: the registry, most asked first.
fn list_questions(settings: &Settings, company: Option<String>, json: bool) -> Result<()> {
    use interview_coach::questions;
    let db = open_db(settings)?;
    let mut all = questions::registry(&db)?;
    if let Some(company) = &company {
        let want = company.to_lowercase();
        all.retain(|q| q.companies().iter().any(|c| c.to_lowercase().contains(&want)));
    }
    if json {
        outln!("{}", serde_json::to_string(&all)?);
        return Ok(());
    }
    if all.is_empty() {
        outln!("No questions yet: they come from your interviews' reviews.");
        return Ok(());
    }
    for q in &all {
        let companies: Vec<String> = q.companies().into_iter().collect();
        let score = q.latest_score().map_or(String::new(), |s| format!(" · last answer {s}/5"));
        let practice = q.asked.iter().filter(|a| a.practice).count();
        let practice = if practice > 0 { format!(" · practised {practice}×") } else { String::new() };
        outln!("{} {}", style(format!("{}×", q.times())).bold(), q.text);
        outln!("   {}", style(format!("{}{}{score}{practice}", q.kind, if companies.is_empty() { String::new() } else { format!(" · {}", companies.join(", ")) })).dim());
    }
    Ok(())
}

fn registry_cmd(settings: &Settings, action: RegistryCmd) -> Result<()> {
    use interview_coach::registry;
    match action {
        RegistryCmd::Pull => {
            let questions = registry::pull(settings, Duration::from_secs(15))?;
            outln!("{} {} shared questions from {}", style("✓").green(), questions.len(), settings.registry_url);
        }
        RegistryCmd::Share { id } => {
            if !settings.share_questions {
                bail!("Sharing is off. Turn it on in Settings, or: ic config set share-questions on");
            }
            let db = open_db(settings)?;
            let client = llm::configured_client(settings, &settings.model)?;
            let n = registry::contribute_session(settings, &db, client.as_ref(), &settings.model.name, id)?;
            outln!("{} Shared {n} questions (names removed).", style("✓").green());
        }
        RegistryCmd::Withdraw => {
            let n = registry::withdraw(settings)?;
            outln!("{} Withdrew {n} shared questions. Questions already approved stay: they're generic, and others may have shared them too.", style("✓").green());
        }
        RegistryCmd::Pending => {
            let pending = registry::pending()?;
            if pending.is_empty() {
                outln!("Nothing is waiting.");
            }
            for p in pending {
                let context = [p.round.as_deref(), p.role.as_deref(), p.company.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                outln!("{} {} {}", style(format!("#{}", p.id)).bold(), p.text, style(format!("[{}{}{}]", p.kind, if context.is_empty() { "" } else { " · " }, context)).dim());
            }
        }
        RegistryCmd::Approve { ids, as_text } => {
            if as_text.is_some() && ids.len() != 1 {
                bail!("--as rewords one contribution at a time");
            }
            for line in registry::approve(&ids, as_text.as_deref())? {
                outln!("{} {line}", style("✓").green());
            }
        }
        RegistryCmd::Reject { ids } => {
            let n = registry::reject(&ids)?;
            outln!("{} Turned down {n}.", style("✓").green());
        }
    }
    Ok(())
}

/// `ic mock begin`: plan the questions and create the session the app records into.
fn mock_begin(settings: &Settings, company: Option<String>, role: Option<String>, round: Option<String>, count: usize) -> Result<()> {
    use interview_coach::{mock, questions};
    let db = open_db(settings)?;
    let target = questions::Target { company: company.clone(), role: role.clone(), round };
    let candidates = interview_coach::registry::merge(questions::registry(&db)?, interview_coach::registry::shared(settings));
    let plan = questions::plan(&candidates, &target, count.clamp(1, 12));
    let title = match (&role, &company) {
        (Some(r), Some(c)) => format!("Mock: {r} at {c}"),
        (Some(r), None) => format!("Mock: {r}"),
        (None, Some(c)) => format!("Mock: {c}"),
        (None, None) => "Mock interview".to_string(),
    };
    let session = pipeline::create_recording_session(&db, settings, &title, company)?;
    db.set_practice(session.id)?;
    let state = mock::MockState::new(target, &plan)?;
    state.save(Path::new(&session.dir))?;
    outln!("{}", serde_json::json!({"id": session.id, "dir": session.dir, "plan": state.plan}));
    Ok(())
}

/// `ic mock run`: the interviewer, turn by turn. Whisper stays loaded between answers, so each one
/// is heard in seconds.
fn mock_run(settings: &Settings, id: i64, events: bool) -> Result<()> {
    use interview_coach::mock;
    use std::io::BufRead;
    let mut ev = JsonEvents::new();
    let result = (|| -> Result<()> {
        if let Some(reason) = blocker_for(settings, &settings.model) {
            bail!("The interviewer needs a coaching model: {reason}");
        }
        let db = open_db(settings)?;
        let dir = PathBuf::from(db.get_session(id)?.dir);
        let mut state = mock::MockState::load(&dir)?;
        ev.stage("Getting the interviewer ready");
        let transcriber = interview_coach::transcribe::Transcriber::load(settings, &mut ev)?;
        let llm = llm::configured_client(settings, &settings.model)?;
        let say = |ev: &mut JsonEvents, text: &str, done: bool| ev.emit(serde_json::json!({"event": "say", "text": text, "done": done}));
        if state.done {
            return Ok(());
        }
        let first = state.start();
        state.save(&dir)?;
        say(&mut ev, &first, false);
        for line in std::io::stdin().lock().lines() {
            let command: serde_json::Value = serde_json::from_str(&line?).context("a command isn't JSON")?;
            if command["stop"].as_bool() == Some(true) {
                break;
            }
            let path = command["answer"].as_str().context("expected {\"answer\": \"<wav>\"} or {\"stop\": true}")?;
            ev.stage("Listening back");
            let samples = interview_coach::audio::load(Path::new(path))?;
            let heard: String = transcriber.transcribe(&samples, &mut ev)?.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
            let (text, done) = if heard.split_whitespace().count() < 2 {
                (state.repeat_please(), false)
            } else {
                state.answer(&heard);
                ev.stage("The interviewer is thinking");
                mock::next(llm.as_ref(), &settings.model.name, &mut state)?
            };
            state.save(&dir)?;
            say(&mut ev, &text, done);
            if done {
                interview_coach::diagnostics::capture(settings, "mock_finished", serde_json::json!({
                    "questions": state.plan.len(), "turns": state.turns.len(),
                }));
                break;
            }
        }
        Ok(())
    })();
    match &result {
        Ok(()) if events => ev.done(),
        Err(e) if events => ev.error(&format!("{e:#}")),
        _ => {}
    }
    result
}

/// `ic eval correct`: one answer's video cues as you saw them, with what was measured then.
fn eval_correct(settings: &Settings, id: i64, start: f64, labels: Option<String>) -> Result<()> {
    let labels = match labels {
        Some(text) => text,
        None => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            text
        }
    };
    let labels: serde_json::Value = serde_json::from_str(labels.trim()).context("the labels aren't JSON")?;
    let db = open_db(settings)?;
    let session = db.get_session(id)?;
    let report = pipeline::current_report(&db, id)?.with_context(|| format!("Interview {id} has no review yet"))?;
    let answer = report
        .turn_signals
        .iter()
        .find(|s| s.kind == interview_coach::temperature::Kind::Answer && (s.start - start).abs() < 0.05)
        .with_context(|| format!("Interview {id} has no answer starting at {}", fmt_ts(start)))?;
    let cues = answer.video.with_context(|| format!("The video showed nothing for the answer at {}", fmt_ts(start)))?;
    let set = format!("s{id:03}");
    label::Store::new(&label::default_dir(&settings.data_dir)).correct(serde_json::json!({
        "id": label::correction_id(id, answer.start), "set": set, "variant": format!("answer {}", fmt_ts(answer.start)),
        "origin": "correction", "title": session.title, "session_dir": session.dir, "start": answer.start, "end": answer.end,
        "face_height": cues.face_h, "layout": null, "labels": labels, "measured": cues,
        "method": report.video_method.as_deref().unwrap_or(video::METHOD), "labelled_at": interview_coach::db::now_iso(),
    }))?;
    interview_coach::diagnostics::capture(settings, "video_corrected", serde_json::json!({
        "checks": labels.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "method": report.video_method.as_deref().unwrap_or(video::METHOD),
    }));
    outln!("{} Saved your correction for the answer at {}.", style("✓").green(), fmt_ts(answer.start));
    Ok(())
}

/// `ic eval video-health`: capture and face reading for every interview recorded with video.
fn eval_video_health(settings: &Settings, json: bool) -> Result<()> {
    let db = open_db(settings)?;
    let mut rows = vec![];
    for session in db.list_sessions()? {
        if session.deleted_at.is_some() || !Path::new(&session.dir).join(video::VIDEO_FILE).is_file() {
            continue;
        }
        let segments = db.get_segments(session.id)?;
        rows.push(interview_coach::video_eval::health(&session, &segments)?);
    }
    if json {
        outln!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        outln!("No interviews have video yet. Record one with video on (Janus 0.1.0-preview.14 or later).");
        return Ok(());
    }
    for h in &rows {
        let share = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{:.0}%", v * 100.0));
        outln!("{} {} · {} of video · faces in {} · typical face {} · {} track(s) · you {} · others {} · cues for {} of {} answers · {} nods",
               style(format!("{:>4}", h.id)).bold(), h.title,
               h.video_s.map_or("?".into(), fmt_ts), share(h.with_faces),
               h.face_h.map_or("—".into(), |f| format!("{:.0}% tall", f * 100.0)), h.tracks,
               if h.you_found { "found" } else { "not found" }, h.others.map_or("—".into(), |n| n.to_string()),
               h.answers_with_cues, h.answers, h.nods);
        for p in &h.problems {
            outln!("     {} {p}", style("!").yellow());
        }
    }
    Ok(())
}

/// `ic setup status` (and `ic doctor`): what's done, what's left, and how to do it.
fn setup_status(settings: &Settings, json: bool) -> Result<()> {
    let status = setup::status(&setup::System { settings });
    if json {
        let mut value = serde_json::to_value(&status)?;
        // What leaves this Mac besides the coaching model and TypeSafe: both off unless chosen.
        value["privacy"] = serde_json::json!({
            "share_questions": settings.share_questions,
            "share_company": settings.share_company,
            "diagnostics": settings.diagnostics,
            "notice_seen": interview_coach::privacy::seen(settings),
        });
        outln!("{}", serde_json::to_string(&value)?);
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
    for tool in [Tool::Ffmpeg, Tool::Ant, Tool::Vision, Tool::Docker] {
        if let Some(found) = tool.find() {
            let origin = match found.origin {
                Origin::Bundled => "bundled with the app",
                Origin::Override => "from IC_* override",
                Origin::System => "installed on this Mac",
            };
            outln!(
                "{}",
                style(format!(
                    "  {} {} ({origin})",
                    tool.name(),
                    found.path.display()
                ))
                .dim()
            );
        }
    }
    let app = capture::app_path();
    if !app.exists() {
        outln!(
            "{} Recorder app not found ({}) — `ic record` needs it; the app records by itself",
            style("•").yellow(),
            app.display()
        );
    }
    if status.ready {
        outln!(
            "{} Ready. Analysis model: {}",
            style("✓").green(),
            settings.model
        );
    } else {
        outln!(
            "{} {} thing(s) left — open Setup in the app, or: ic setup run all, then ic login",
            style("•").yellow(),
            status.remaining
        );
    }
    outln!(
        "{}",
        style(format!(
            "Data folder: {} · Models: {}",
            settings.data_dir.display(),
            settings.models_dir.display()
        ))
        .dim()
    );
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut settings = Settings::load()?;
    // Kept apart from the default: `ic run next` falls back to the report's model, not the default.
    let explicit_model: Option<ModelRef> = match cli.model.as_deref() {
        None => None,
        Some("cheapest") => {
            let model = interview_coach::catalog::cheapest(&settings)?;
            errln!(
                "{}",
                style(format!("Cheapest available by list price: {model}")).dim()
            );
            Some(model)
        }
        Some(text) => Some(text.parse().map_err(|e: String| anyhow::anyhow!(e))?),
    };
    if let Some(model) = &explicit_model {
        settings.model = model.clone();
    }
    match cli.command {
        Cmd::Record {
            title,
            company,
            aec,
            video,
            duration,
            yes,
            no_analyze,
        } => record(&settings, title, company, Capture { aec, video, duration }, yes, !no_analyze),
        Cmd::Stop { no_analyze } => {
            let mut db = open_db(&settings)?;
            let active: Vec<_> = db
                .list_sessions()?
                .into_iter()
                .filter(|s| s.status == Status::Recording)
                .collect();
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
        Cmd::Import {
            path,
            mic,
            system,
            title,
            company,
            speakers,
            no_transcribe,
            no_analyze,
        } => {
            let mut db = open_db(&settings)?;
            let session = match (path, mic, system) {
                (Some(path), None, None) => {
                    let title = title.unwrap_or_else(|| {
                        path.file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into()
                    });
                    let mut ui = Ui::new();
                    let session = pipeline::ingest_file(
                        &mut db,
                        &settings,
                        &path,
                        &title,
                        company,
                        Some(speakers),
                        &mut ui,
                    );
                    ui.finish();
                    session?
                }
                (None, Some(mic), Some(system)) => {
                    let title = title.unwrap_or_else(|| {
                        mic.canonicalize()
                            .ok()
                            .and_then(|p| {
                                Some(p.parent()?.file_name()?.to_string_lossy().into_owned())
                            })
                            .unwrap_or_else(|| "Interview".into())
                    });
                    let mut ui = Ui::new();
                    let session = pipeline::ingest_tracks(
                        &mut db, &settings, &mic, &system, &title, company, &mut ui,
                    );
                    ui.finish();
                    session?
                }
                _ => bail!("Pass a recording file, or both --mic and --system tracks."),
            };
            outln!(
                "Imported as session {} ({}) → {}",
                style(session.id).bold(),
                fmt_ts(session.duration_s.unwrap_or(0.0)),
                session.dir
            );
            if no_transcribe {
                return Ok(());
            }
            after_transcription(&mut db, &settings, session.id, !no_analyze)
        }
        Cmd::Transcribe { id } => run_transcription(&mut open_db(&settings)?, &settings, id),
        Cmd::List { json, all } => list(&settings, json, all),
        Cmd::Edit {
            id,
            title,
            company,
            no_company,
            role,
            role_id,
            no_role,
            round,
        } => edit(
            &open_db(&settings)?,
            id,
            EditArgs {
                title,
                company,
                no_company,
                role,
                role_id,
                no_role,
                round,
            },
        ),
        Cmd::Archive { ids, undo } => {
            let db = open_db(&settings)?;
            for id in &ids {
                library::archive(&db, *id, !undo)?;
            }
            outln!(
                "{} {} {}.",
                style("✓").green(),
                if undo { "Brought back" } else { "Archived" },
                plural(ids.len(), "interview")
            );
            Ok(())
        }
        Cmd::Delete { ids } => {
            let db = open_db(&settings)?;
            for id in &ids {
                library::delete(&db, *id)?;
            }
            outln!(
                "{} Moved {} to Recently Deleted: restore with ic restore, or it's erased in {} days.",
                style("✓").green(),
                plural(ids.len(), "interview"),
                library::DELETED_DAYS
            );
            Ok(())
        }
        Cmd::Restore { ids } => {
            let db = open_db(&settings)?;
            for id in &ids {
                library::restore(&db, *id)?;
            }
            outln!(
                "{} Restored {}.",
                style("✓").green(),
                plural(ids.len(), "interview")
            );
            Ok(())
        }
        Cmd::Erase { ids } => {
            let db = open_db(&settings)?;
            for id in &ids {
                library::erase(&db, &settings, *id)?;
            }
            outln!("Erased {} for good.", plural(ids.len(), "interview"));
            Ok(())
        }
        Cmd::EmptyDeleted { now } => {
            let db = open_db(&settings)?;
            let erased = library::empty_deleted(
                &db,
                &settings,
                if now { 0 } else { library::DELETED_DAYS },
            )?;
            if !erased.is_empty() {
                outln!("Erased {} for good.", plural(erased.len(), "interview"));
            }
            Ok(())
        }
        Cmd::Role { action } => role_cmd(&open_db(&settings)?, action),
        Cmd::Company { action } => {
            let db = open_db(&settings)?;
            match action {
                CompanyCmd::Rename { old, new } => {
                    let n = db.rename_company(&old, &new)?;
                    outln!(
                        "{} Renamed {old} to {new} ({n} roles and interviews).",
                        style("✓").green()
                    );
                }
                CompanyCmd::Archive { name, undo } => {
                    let n = library::archive_company(&db, &name, !undo)?;
                    outln!(
                        "{} {} {name} ({n} roles and interviews).",
                        style("✓").green(),
                        if undo { "Brought back" } else { "Archived" }
                    );
                }
            }
            Ok(())
        }
        Cmd::Search { query, json } => {
            let hits = library::search(&open_db(&settings)?, &query)?;
            if json {
                outln!("{}", serde_json::to_string(&hits)?);
            } else {
                for h in &hits {
                    let at = h.at.map(|a| format!(" {}", fmt_ts(a))).unwrap_or_default();
                    outln!("{:>4} {:<10}{at} {}", h.session_id, h.kind, h.text);
                }
            }
            Ok(())
        }
        Cmd::Recording { action } => match action {
            RecordingCmd::Begin { title, company } => {
                let db = open_db(&settings)?;
                let title = title.unwrap_or_else(|| {
                    chrono::Local::now()
                        .format("Interview %Y-%m-%d %H:%M")
                        .to_string()
                });
                let session = pipeline::create_recording_session(&db, &settings, &title, company)?;
                outln!(
                    "{}",
                    serde_json::json!({"id": session.id, "dir": session.dir})
                );
                Ok(())
            }
            RecordingCmd::Finish { id, no_analyze } => {
                finish_recording(&mut open_db(&settings)?, &settings, id, !no_analyze, false)
            }
        },
        Cmd::Transcript { id, json } => transcript(&settings, id, json),
        Cmd::Swap { id } => swap(&settings, id),
        Cmd::Analyze { id, open, full } => run_analysis(
            &mut open_db(&settings)?,
            &settings,
            &settings.model,
            id,
            open,
            full,
        ),
        Cmd::Steps { id, json } => {
            let db = open_db(&settings)?;
            if json {
                outln!(
                    "{}",
                    serde_json::to_string_pretty(&session_view::build(&db, id)?.stages)?
                );
                Ok(())
            } else {
                print_steps(&db, id)
            }
        }
        Cmd::Run {
            step,
            id,
            speakers,
            then_later,
        } => run_step(
            &mut open_db(&settings)?,
            &settings,
            step,
            id,
            explicit_model.as_ref(),
            speakers,
            then_later,
        ),
        Cmd::Next { id } => {
            let db = open_db(&settings)?;
            let Some(next) = db.latest_next_steps(id)? else {
                bail!("Session {id} has no next steps yet. Run: ic run next {id}");
            };
            report::print_next_steps(&db.get_session(id)?, &next);
            Ok(())
        }
        Cmd::Session { id } => {
            let mut view = session_view::build(&open_db(&settings)?, id)?;
            // Your corrections live with the labels, outside the database.
            let corrections = label::Store::new(&label::default_dir(&settings.data_dir)).read(label::CORRECTIONS_FILE).unwrap_or_default();
            for answer in &mut view.video_answers {
                answer.corrected = corrections.iter().find(|c| c["id"] == label::correction_id(id, answer.start)).map(|c| c["labels"].clone());
            }
            outln!("{}", serde_json::to_string(&view)?);
            Ok(())
        }
        Cmd::Report { id, open, full } => show_report(&settings, id, open, full),
        Cmd::Timeline { id } => refresh_timeline(&settings, id),
        Cmd::Questions { company, json } => list_questions(&settings, company, json),
        Cmd::PrivacyNotice { seen } => {
            if seen {
                interview_coach::privacy::mark_seen(&settings)?;
            } else {
                outln!("{}", interview_coach::privacy::NOTICE);
            }
            Ok(())
        }
        Cmd::Registry { action } => registry_cmd(&settings, action),
        Cmd::Mock { action: MockCmd::Begin { company, role, round, count } } => mock_begin(&settings, company, role, round, count),
        Cmd::Mock { action: MockCmd::Run { id, events } } => mock_run(&settings, id, events),
        Cmd::Models { json } => list_models(&settings, json),
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
                outln!(
                    "{} AI proxy running at {}",
                    style("✓").green(),
                    proxy::base_url(&settings)
                );
                Ok(())
            }
            ProxyCmd::Stop => proxy::stop(&settings),
            ProxyCmd::Status => proxy_status(&settings),
        },
        Cmd::Login {
            events,
            switch,
            provider,
            account,
            enable_plan,
        } => match provider {
            Provider::Anthropic => {
                anyhow::ensure!(
                    account.is_none() && !enable_plan,
                    "--account and --enable-plan are for OpenAI sign-in"
                );
                login(events, switch)
            }
            Provider::OpenAi => {
                chatgpt_login(&settings, events, switch, account.as_deref(), enable_plan)
            }
        },
        Cmd::Logout {
            provider: Provider::OpenAi,
            events,
        } => chatgpt_logout(&settings, events),
        Cmd::Logout {
            provider: Provider::Anthropic,
            ..
        } => {
            let account = auth::account();
            auth::logout()?;
            outln!(
                "{} Signed out of Claude{}.",
                style("✓").green(),
                account.map(|a| format!(" ({a})")).unwrap_or_default()
            );
            Ok(())
        }
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
        Cmd::Eval {
            action:
                EvalCmd::Scorers {
                    set,
                    items,
                    arms,
                    runs,
                    out,
                    limit,
                    concurrency,
                },
        } => {
            let arms = if arms.is_empty() { set.default_arms() } else { arms };
            if set == EvalSet::Video {
                eval_video(&settings, items, &arms, runs, out, limit, concurrency)
            } else {
                eval_scorers(&settings, set, items, &arms, runs, out, limit, concurrency)
            }
        }
        Cmd::Eval {
            action: EvalCmd::VideoHealth { json },
        } => eval_video_health(&settings, json),
        Cmd::Eval {
            action: EvalCmd::Correct { id, start, labels },
        } => eval_correct(&settings, id, start, labels),
        Cmd::Eval {
            action: EvalCmd::Clips { ids, origin, dir },
        } => eval_clips(&settings, &ids, origin, dir),
        Cmd::Eval {
            action: EvalCmd::Label { dir, port, no_open },
        } => label::serve(&dir.unwrap_or_else(|| label::default_dir(&settings.data_dir)), port, !no_open),
        Cmd::Jev {
            action: JevCmd::Ping,
        } => jev_ping(&settings),
    }
}

fn main() {
    if let Err(e) = run() {
        // Which command failed, never the error's text (it can hold paths and names).
        let command = std::env::args().nth(1).filter(|c| c.len() <= 20 && c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-'));
        if let (Some(command), Ok(settings)) = (command, Settings::load()) {
            interview_coach::diagnostics::capture(&settings, "command_failed", serde_json::json!({"command": command}));
        }
        errln!("{} {e:#}", style("Error:").red().bold());
        std::process::exit(1);
    }
}
