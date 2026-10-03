//! Core data types shared by the pipeline, the database layer, and the CLI.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const YOU: &str = "you";
pub const INTERVIEWER: &str = "interviewer";

/// An enum stored and serialized as a fixed lowercase string (in SQLite, JSON, and the CLI).
macro_rules! text_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        $vis enum $name { $(#[serde(rename = $text)] $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s {
                    $($text => Ok($name::$variant),)+
                    _ => Err(format!("unknown {} {s:?}", stringify!($name))),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum! {
    /// One mixed track (needs diarization) vs. separate mic + system tracks.
    pub enum Mode { Single => "single", Dual => "dual" }
}
text_enum! {
    pub enum Source { Upload => "upload", Recording => "recording" }
}
text_enum! {
    pub enum Status {
        New => "new", Recording => "recording", Transcribing => "transcribing", Transcribed => "transcribed",
        Analyzing => "analyzing", Analyzed => "analyzed", Failed => "failed",
    }
}
text_enum! {
    pub enum OutcomeResult {
        Pending => "pending", Advanced => "advanced", Offer => "offer", Rejected => "rejected",
        Withdrew => "withdrew", NoResponse => "no_response",
    }
}

impl OutcomeResult {
    pub fn label(self) -> &'static str {
        match self {
            OutcomeResult::Pending => "Waiting to hear",
            OutcomeResult::Advanced => "Advanced",
            OutcomeResult::Offer => "Offer",
            OutcomeResult::Rejected => "Rejected",
            OutcomeResult::Withdrew => "Withdrew",
            OutcomeResult::NoResponse => "No response",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    /// Whisper keeps the leading space, so words join with `concat()`.
    pub text: String,
}

/// A sentence-ish span of speech from one speaker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// "you", "interviewer", "interviewer_2", ... (a raw diarization label before mapping).
    #[serde(default)]
    pub speaker: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
}

impl Segment {
    pub fn new(start: f64, end: f64, text: &str, speaker: &str) -> Self {
        Segment { start, end, text: text.to_string(), speaker: speaker.to_string(), words: vec![] }
    }
}

/// One diarization result: who was talking from start to end.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechSpan {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

/// Consecutive segments by the same speaker — what people (and the analysis) read.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub id: i64,
    pub created_at: String,
    pub title: String,
    pub company: Option<String>,
    pub stage: Option<Stage>,
    pub role_id: Option<i64>,
    pub source: Source,
    pub mode: Mode,
    pub source_path: Option<String>,
    pub dir: String,
    pub duration_s: Option<f64>,
    pub num_speakers: Option<i64>,
    pub consent: Option<bool>,
    pub status: Status,
    pub error: Option<String>,
    /// Hidden from the main list (still searchable, and shown with "Show archived").
    pub archived_at: Option<String>,
    /// In Recently Deleted since then; erased for good 30 days later.
    pub deleted_at: Option<String>,
    /// The role was set (by filing or by you), so reports don't file it again.
    pub role_set: bool,
}

pub fn speaker_label(speaker: &str) -> String {
    match speaker {
        YOU => "You".into(),
        INTERVIEWER => "Interviewer".into(),
        s => match s.strip_prefix("interviewer_") {
            Some(n) => format!("Interviewer {n}"),
            None => s.into(),
        },
    }
}

pub fn fmt_ts(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// Seconds from `HH:MM:SS` or `MM:SS` (as in reports), or None.
pub fn parse_ts(ts: &str) -> Option<f64> {
    let parts: Vec<&str> = ts.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut seconds = 0.0;
    for (i, part) in parts.iter().enumerate() {
        let n: u32 = part.parse().ok().filter(|_| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))?;
        if i > 0 && n >= 60 {
            return None;
        }
        seconds = seconds * 60.0 + n as f64;
    }
    Some(seconds)
}

// --- Analysis schema --------------------------------------------------------------------------
// These types are Claude's structured-output schema, so doc comments are instructions to the model.

text_enum! {
    pub enum Verdict {
        Strong => "strong", LeaningPositive => "leaning_positive", Mixed => "mixed",
        LeaningNegative => "leaning_negative", Weak => "weak",
    }
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Strong => "Strong",
            Verdict::LeaningPositive => "Leaning positive",
            Verdict::Mixed => "Mixed",
            Verdict::LeaningNegative => "Leaning negative",
            Verdict::Weak => "Weak",
        }
    }
}

text_enum! {
    pub enum Confidence { Low => "low", Medium => "medium", High => "high" }
}
text_enum! {
    pub enum Stage {
        RecruiterScreen => "recruiter_screen", HiringManager => "hiring_manager", Technical => "technical",
        Behavioral => "behavioral", Case => "case", Panel => "panel", Final => "final",
        Informational => "informational", Other => "other",
    }
}

text_enum! {
    /// Where an application for a role stands.
    pub enum RoleStatus {
        Interviewing => "interviewing", Offer => "offer", Accepted => "accepted", Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
}

impl RoleStatus {
    pub fn label(self) -> &'static str {
        match self {
            RoleStatus::Interviewing => "Interviewing",
            RoleStatus::Offer => "Offer",
            RoleStatus::Accepted => "Accepted",
            RoleStatus::Rejected => "Rejected",
            RoleStatus::Withdrawn => "Withdrawn",
        }
    }
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::RecruiterScreen => "Recruiter screen",
            Stage::HiringManager => "Hiring manager",
            Stage::Technical => "Technical",
            Stage::Behavioral => "Behavioral",
            Stage::Case => "Case",
            Stage::Panel => "Panel",
            Stage::Final => "Final round",
            Stage::Informational => "Informational",
            Stage::Other => "Interview",
        }
    }
}

text_enum! {
    pub enum QuestionType {
        Intro => "intro", Behavioral => "behavioral", Technical => "technical", Situational => "situational",
        Motivation => "motivation", RoleSpecific => "role_specific", Logistics => "logistics", Other => "other",
    }
}
text_enum! {
    pub enum Direction { Positive => "positive", Negative => "negative" }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// Words copied verbatim from the transcript (5-40 words). Never paraphrase.
    pub quote: String,
    /// HH:MM:SS of the transcript turn the quote comes from.
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InterviewContext {
    /// Company the candidate is interviewing with, if stated.
    pub company: Option<String>,
    /// Role being interviewed for, if stated or clearly implied.
    pub role_title: Option<String>,
    /// Seniority level, e.g. 'senior', 'staff', 'manager', if evident.
    pub seniority: Option<String>,
    pub stage: Stage,
    /// Each interviewer as 'Name — role' where known, e.g. 'Daniel — head of product'.
    pub interviewers: Vec<String>,
    /// True only if the speaker labelled 'You' is clearly the interviewer (asks the questions,
    /// describes the company) — i.e. the automatic speaker labels are backwards.
    pub labels_swapped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Signal {
    pub direction: Direction,
    /// What the interviewer did or said and why it's telling, in one sentence.
    pub signal: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Outlook {
    /// How likely this interview is to lead to the next round/offer.
    pub verdict: Verdict,
    pub confidence: Confidence,
    /// 2-4 sentences grounding the verdict in the interviewer's behaviour and the answers.
    pub reasoning: String,
    /// Interviewer signals, strongest first (3-6).
    pub signals: Vec<Signal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RubricScore {
    /// 1-5 using the anchors in the instructions; null if this interview gave no chance to show it.
    pub score: Option<u8>,
    /// One or two sentences.
    pub rationale: String,
    /// 1-2 supporting quotes from the candidate (empty if score is null).
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rubric {
    pub clarity: RubricScore,
    pub structure: RubricScore,
    pub specificity_and_impact: RubricScore,
    pub role_fit: RubricScore,
    pub technical_depth: RubricScore,
    pub curiosity: RubricScore,
    pub composure: RubricScore,
}

impl Rubric {
    /// (label, score) pairs in display order.
    pub fn items(&self) -> [(&'static str, &RubricScore); 7] {
        [
            ("Clarity", &self.clarity),
            ("Structure", &self.structure),
            ("Specificity & impact", &self.specificity_and_impact),
            ("Role fit", &self.role_fit),
            ("Technical depth", &self.technical_depth),
            ("Curiosity", &self.curiosity),
            ("Composure", &self.composure),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionReview {
    /// HH:MM:SS where the interviewer asks it.
    pub timestamp: String,
    /// The interviewer's question, lightly condensed.
    pub question: String,
    #[serde(rename = "type")]
    pub kind: QuestionType,
    /// What the candidate actually said, in one or two sentences.
    pub answer_summary: String,
    /// Answer quality, 1-5; null only when the candidate's answer is missing from the recording (see recording_notes).
    pub score: Option<u8>,
    pub what_worked: String,
    pub what_was_missing: String,
    /// A concrete outline of a stronger answer, built from what the candidate actually knows.
    pub stronger_answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Highlight {
    pub point: String,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoachingPoint {
    /// Short imperative, e.g. 'Lead with the result'.
    pub title: String,
    pub why_it_matters: String,
    pub evidence: Evidence,
    /// Exactly what to do differently, specific enough to try in the next interview.
    pub fix: String,
    /// A practice exercise that takes under 15 minutes.
    pub drill: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionAnalysis {
    pub context: InterviewContext,
    /// 3-4 sentence overview of how the interview went.
    pub summary: String,
    pub outlook: Outlook,
    pub rubric: Rubric,
    /// Every substantive interviewer question, in order.
    pub questions: Vec<QuestionReview>,
    /// 2-4 things the candidate did well.
    pub strengths: Vec<Highlight>,
    /// Moments that likely hurt the candidate (empty if none).
    pub red_flags: Vec<Highlight>,
    /// The 3 highest-leverage improvements, most important first.
    pub coaching: Vec<CoachingPoint>,
}

impl SessionAnalysis {
    /// Checks the constraints the API's schema can't enforce (score ranges).
    pub fn validate(&self) -> Result<(), String> {
        let in_range = |s: u8| (1..=5).contains(&s);
        for (label, score) in self.rubric.items() {
            if score.score.is_some_and(|s| !in_range(s)) {
                return Err(format!("rubric score for {label} is out of range: {:?}", score.score));
            }
        }
        if let Some(q) = self.questions.iter().find(|q| q.score.is_some_and(|s| !in_range(s))) {
            return Err(format!("question score out of range: {:?}", q.score));
        }
        Ok(())
    }

    /// Every quote in the analysis, with where it's used.
    pub fn evidence(&self) -> Vec<(&'static str, &Evidence)> {
        let mut out = vec![];
        for (label, score) in self.rubric.items() {
            out.extend(score.evidence.iter().map(|e| (label, e)));
        }
        out.extend(self.outlook.signals.iter().map(|s| ("signal", &s.evidence)));
        out.extend(self.strengths.iter().map(|h| ("strength", &h.evidence)));
        out.extend(self.red_flags.iter().map(|h| ("red flag", &h.evidence)));
        out.extend(self.coaching.iter().map(|c| ("coaching", &c.evidence)));
        out
    }
}

// --- Stages ------------------------------------------------------------------------------------

text_enum! {
    /// The four stages every interview goes through, in order. Each can be re-run on its own.
    pub enum Step { Recording => "recording", Transcript => "transcript", Report => "report", Next => "next" }
}

impl Step {
    /// The stage this one is built from.
    pub fn upstream(self) -> Option<Step> {
        match self {
            Step::Recording => None,
            Step::Transcript => Some(Step::Recording),
            Step::Report => Some(Step::Transcript),
            Step::Next => Some(Step::Report),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Step::Recording => "Recording",
            Step::Transcript => "Transcript",
            Step::Report => "After-action report",
            Step::Next => "What to do next",
        }
    }
}

text_enum! {
    pub enum RunStatus { Running => "running", Succeeded => "succeeded", Failed => "failed" }
}

// --- "What to do next" schema ------------------------------------------------------------------
// Like the analysis schema above, these doc comments are instructions to the model.

text_enum! {
    pub enum Priority { High => "high", Medium => "medium", Low => "low" }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PrepItem {
    /// What the next round will likely probe, in a few words.
    pub topic: String,
    /// Why you expect it: what the interviewer said or did, in one or two sentences.
    pub why: String,
    /// The interviewer's words that signal it (or the candidate's, if a gap they showed is the reason).
    pub evidence: Evidence,
    /// Concretely how to prepare, built from what the candidate has actually done.
    pub how_to_prepare: String,
    /// 2-3 questions they are likely to ask about it.
    pub likely_questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PracticeItem {
    /// The skill to practise, in a few words.
    pub skill: String,
    /// The title of the report's coaching point this serves.
    pub from_coaching: String,
    /// A specific exercise to do, step by step in one or two sentences.
    pub drill: String,
    /// Realistic minutes for one session of the drill.
    pub minutes: u16,
    pub priority: Priority,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NextSteps {
    /// One sentence: where the candidate stands and the single most important next move.
    pub headline: String,
    /// 3-5 topics the next conversation is likely to probe, strongest signal first.
    pub next_round_prep: Vec<PrepItem>,
    /// 3-5 drills, most important first.
    pub practice_plan: Vec<PracticeItem>,
}

impl NextSteps {
    pub fn validate(&self) -> Result<(), String> {
        if self.next_round_prep.is_empty() || self.practice_plan.is_empty() {
            return Err("both next-round prep and a practice plan are required".into());
        }
        if let Some(p) = self.practice_plan.iter().find(|p| !(1..=180).contains(&p.minutes)) {
            return Err(format!("unrealistic drill length: {} minutes", p.minutes));
        }
        Ok(())
    }

    pub fn evidence(&self) -> Vec<&Evidence> {
        self.next_round_prep.iter().map(|p| &p.evidence).collect()
    }
}
