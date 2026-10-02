//! Progress reporting from long-running steps (downloads, transcription, analysis) to the UI.

pub trait Progress {
    /// A new step started, e.g. "Transcribing (You)".
    fn stage(&mut self, message: &str);
    /// Progress within the current step.
    fn step(&mut self, _done: u64, _total: u64) {}
}

/// For tests and callers that don't show progress.
pub struct Quiet;

impl Progress for Quiet {
    fn stage(&mut self, _message: &str) {}
}
