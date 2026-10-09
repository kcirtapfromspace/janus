//! Janus: record, transcribe, and get coached on your job interviews — locally.

/// Like `println!`, but a closed stdout (the app that started ic quit, or `| head`) doesn't end
/// ic mid-step: the output is dropped and the work still finishes and lands in the database.
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// `print!` that tolerates a closed stdout (see `outln!`).
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}

/// `eprintln!` that tolerates a closed stderr (see `outln!`).
#[macro_export]
macro_rules! errln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod analyze;
pub mod audio;
pub mod auth;
pub mod capture;
pub mod catalog;
pub mod config;
pub mod coverage;
pub mod db;
pub mod diagnostics;
pub mod diarize;
pub mod download;
pub mod eval;
pub mod events;
pub mod history;
pub mod jev_auth;
pub mod label;
pub mod library;
pub mod llm;
pub mod merge;
pub mod metrics;
pub mod mock;
pub mod models;
pub mod next_steps;
pub mod openai_auth;
pub mod pipeline;
pub mod privacy;
pub mod progress;
pub mod prosody;
pub mod questions;
pub mod registry;
pub mod proxy;
pub mod report;
pub mod schema;
pub mod scoring;
pub mod session_view;
pub mod setup;
pub mod steps;
pub mod temperature;
pub mod tools;
pub mod transcribe;
pub mod trends;
pub mod versions;
pub mod video;
pub mod video_eval;
