//! What leaves this Mac besides the coaching model and TypeSafe: shared questions (registry.rs)
//! and anonymous diagnostics (diagnostics.rs). Both are on by default. Neither sends anything until
//! this notice has been shown once: in the app's "What Janus shares" window, or printed by `ic` the
//! first time a review would share. Each stays a switch in Settings (`ic config set`).

use std::path::PathBuf;

use crate::config::Settings;

pub const NOTICE: &str = "\
What Janus shares, and how to turn it off

- Your interviews' questions: after each review, the interviewer's questions are rewritten so they
  name no person, company or product, checked again, and sent to Janus's question registry. Once
  approved, they help other Janus users practise. Never your answers, transcripts, audio or video.
  Turn off: Settings, or `ic config set share-questions off`. Withdraw what was shared:
  `ic registry withdraw`.
- Anonymous diagnostics: whether recording, reading the video, reviews and practice worked:
  counts, outcomes and warning codes, with a random id for this Mac. Never content, names or file
  paths. Turn off: Settings, or `ic config set diagnostics off`.";

fn marker(settings: &Settings) -> PathBuf {
    settings.data_dir.join("privacy-notice-seen")
}

/// Whether the notice has been shown on this Mac.
pub fn seen(settings: &Settings) -> bool {
    marker(settings).exists()
}

pub fn mark_seen(settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(&settings.data_dir)?;
    std::fs::write(marker(settings), chrono::Utc::now().to_rfc3339())
}
