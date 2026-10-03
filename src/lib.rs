//! Interview Coach: record, transcribe, and get coached on your job interviews — locally.

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
pub mod catalog;
pub mod capture;
pub mod llm;
pub mod config;
pub mod coverage;
pub mod db;
pub mod diarize;
pub mod download;
pub mod eval;
pub mod history;
pub mod events;
pub mod merge;
pub mod metrics;
pub mod models;
pub mod next_steps;
pub mod pipeline;
pub mod progress;
pub mod prosody;
pub mod proxy;
pub mod report;
pub mod schema;
pub mod scoring;
pub mod session_view;
pub mod setup;
pub mod steps;
pub mod temperature;
pub mod versions;
pub mod tools;
pub mod transcribe;
