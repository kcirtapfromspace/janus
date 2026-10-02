//! Session analysis: transcript + metrics in, a validated SessionAnalysis out (Claude or OpenAI).

use anyhow::Result;

use crate::llm::{self, Effort, Llm, StructuredOutput};
use crate::metrics::TalkMetrics;
use crate::models::{Mode, SessionAnalysis, Turn, fmt_ts, speaker_label};

pub const PROMPT_VERSION: &str = "session-v1";
pub const SYSTEM_PROMPT: &str = include_str!("../prompts/session_v1.md");

impl StructuredOutput for SessionAnalysis {
    const NAME: &'static str = "session_analysis";
    fn validate(&self) -> Result<(), String> {
        SessionAnalysis::validate(self)
    }
}

pub fn transcript_text(turns: &[Turn]) -> String {
    turns
        .iter()
        .map(|t| format!("[{}] {}: {}", fmt_ts(t.start), speaker_label(&t.speaker), t.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn build_user_message(turns: &[Turn], metrics: &TalkMetrics, title: &str, company: Option<&str>, mode: Mode,
                          role_profile: Option<&str>) -> String {
    let labels = match mode {
        Mode::Dual => "from separate mic/system tracks (reliable)",
        Mode::Single => "assigned automatically from one mixed track (may be swapped)",
    };
    let mut parts = vec![
        format!("<session>\nTitle: {title}\nCompany (as entered by the candidate): {}\nSpeaker labels: {labels}\n</session>",
                company.unwrap_or("not given")),
        format!("<metrics>\n{}\n</metrics>", serde_json::to_string_pretty(metrics).expect("metrics serialize")),
    ];
    if let Some(profile) = role_profile {
        parts.push(format!("<target_role>\n{profile}\n</target_role>"));
    }
    parts.push(format!("<transcript>\n{}\n</transcript>", transcript_text(turns)));
    parts.join("\n\n")
}

/// Analyse one interview. `model` is the provider's own model name (the adapter picks the API).
pub fn analyze(llm: &dyn Llm, model: &str, user_message: &str, on_progress: &mut dyn FnMut(usize)) -> Result<SessionAnalysis> {
    llm::generate::<SessionAnalysis>(llm, model, SYSTEM_PROMPT, user_message, Effort::High, on_progress)
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quotes that don't appear in the transcript (after normalizing case and punctuation).
pub fn unverified_quotes(analysis: &SessionAnalysis, turns: &[Turn]) -> Vec<String> {
    let haystack = normalize(&turns.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" "));
    analysis
        .evidence()
        .into_iter()
        .filter(|(_, ev)| !haystack.contains(&normalize(&ev.quote)))
        .map(|(_, ev)| ev.quote.clone())
        .collect()
}
