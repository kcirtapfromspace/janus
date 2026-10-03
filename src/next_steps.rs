//! "What to do next": next-round prep and a practice plan, built from the after-action report and
//! the transcript (and the real outcome, once you've recorded it).

use anyhow::Result;

use crate::analyze::{missing_quotes, notes_block, transcript_text};
use crate::coverage::RecordingNotes;
use crate::db::{Outcome, StoredAnalysis};
use crate::llm::{self, Effort, Llm, StructuredOutput};
use crate::models::{NextSteps, Session, Turn};

pub const PROMPT_VERSION: &str = "next-v2";
pub const SYSTEM_PROMPT: &str = include_str!("../prompts/next_steps_v2.md");

impl StructuredOutput for NextSteps {
    const NAME: &'static str = "next_steps";
    fn validate(&self) -> Result<(), String> {
        NextSteps::validate(self)
    }
}

pub fn build_user_message(session: &Session, report: &StoredAnalysis, outcome: Option<&Outcome>, turns: &[Turn],
                          today: &str, notes: &RecordingNotes) -> String {
    let a = &report.analysis;
    let outcome = match outcome {
        Some(o) => format!("{}{}", o.result.label(), o.notes.as_deref().map(|n| format!(" — {n}")).unwrap_or_default()),
        None => "not known yet".into(),
    };
    // A compact view of the report: what the plan should build on, without the full schema.
    let report_view = serde_json::json!({
        "verdict": a.outlook.verdict,
        "confidence": a.outlook.confidence,
        "reasoning": a.outlook.reasoning,
        "interviewer_signals": a.outlook.signals.iter().map(|s| serde_json::json!({
            "direction": s.direction, "signal": s.signal, "quote": s.evidence.quote, "timestamp": s.evidence.timestamp,
        })).collect::<Vec<_>>(),
        "coaching": a.coaching.iter().map(|c| serde_json::json!({"title": c.title, "fix": c.fix, "drill": c.drill}))
            .collect::<Vec<_>>(),
        "questions": a.questions.iter().map(|q| serde_json::json!({
            "timestamp": q.timestamp, "question": q.question, "score": q.score, "missing": q.what_was_missing,
        })).collect::<Vec<_>>(),
        "red_flags": a.red_flags.iter().map(|h| &h.point).collect::<Vec<_>>(),
    });
    format!(
        "<session>\nTitle: {}\nCompany: {}\nStage: {}\nInterview date: {}\nToday: {today}\nReal outcome: {outcome}\n</session>\n\n\
         {}<report>\n{}\n</report>\n\n<transcript>\n{}\n</transcript>",
        session.title,
        session.company.as_deref().or(a.context.company.as_deref()).unwrap_or("not given"),
        a.context.stage.label(),
        &session.created_at[..10.min(session.created_at.len())],
        notes_block(notes).map(|b| format!("{b}\n\n")).unwrap_or_default(),
        serde_json::to_string_pretty(&report_view).expect("report view serializes"),
        transcript_text(turns),
    )
}

pub fn generate(llm: &dyn Llm, model: &str, user_message: &str, on_progress: &mut dyn FnMut(usize)) -> Result<NextSteps> {
    llm::generate::<NextSteps>(llm, model, SYSTEM_PROMPT, user_message, Effort::High, on_progress)
}

/// Quotes in the plan that don't appear in the transcript.
pub fn unverified_quotes(plan: &NextSteps, turns: &[Turn]) -> Vec<String> {
    missing_quotes(plan.evidence(), turns)
}
