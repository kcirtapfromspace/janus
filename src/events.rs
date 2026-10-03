//! JSON-lines events on stdout, for the app to follow a long step live (`ic setup run --events`,
//! `ic login --events`). One object per line, each with an `event` field:
//!
//! - `{"event":"stage","message":"Downloading the speech model"}`
//! - `{"event":"progress","done":123,"total":456}` (bytes or units; `total` 0 when unknown)
//! - `{"event":"open_url","url":"https://…"}` (the sign-in page; the browser normally opens itself)
//! - `{"event":"need_code"}` (sign-in wants the code its page shows, sent as a line on stdin)
//! - `{"event":"log","message":"…"}`
//! - `{"event":"done"}` or `{"event":"error","message":"…"}`, always last.
//!
//! Nothing else may print to stdout while events are on, or the app can't parse the stream.

use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::progress::Progress;

pub struct JsonEvents {
    last_progress: Option<Instant>,
}

impl Default for JsonEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonEvents {
    pub fn new() -> Self {
        JsonEvents { last_progress: None }
    }

    pub fn emit(&mut self, event: Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{event}");
        let _ = out.flush();
    }

    pub fn open_url(&mut self, url: &str) {
        self.emit(json!({"event": "open_url", "url": url}));
    }

    pub fn need_code(&mut self) {
        self.emit(json!({"event": "need_code"}));
    }

    pub fn log(&mut self, message: &str) {
        self.emit(json!({"event": "log", "message": message}));
    }

    pub fn done(&mut self) {
        self.emit(json!({"event": "done"}));
    }

    pub fn error(&mut self, message: &str) {
        self.emit(json!({"event": "error", "message": message}));
    }
}

impl Progress for JsonEvents {
    fn stage(&mut self, message: &str) {
        self.last_progress = None;
        self.emit(json!({"event": "stage", "message": message}));
    }

    /// At most about four updates a second, plus the final one.
    fn step(&mut self, done: u64, total: u64) {
        let now = Instant::now();
        if done < total && self.last_progress.is_some_and(|t| now - t < Duration::from_millis(250)) {
            return;
        }
        self.last_progress = Some(now);
        self.emit(json!({"event": "progress", "done": done, "total": total}));
    }
}
