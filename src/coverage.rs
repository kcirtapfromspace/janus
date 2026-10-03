//! Does each side of a dual-track recording cover the whole interview?
//!
//! A track can stop early or drop out — the mic did once, when a call app took it over 25 s into a
//! 42-minute interview. Then part of the conversation is missing, not silent: the analysis must be
//! told so it doesn't read the gap as a candidate who said nothing, the report has to say so up
//! front, and talk-time metrics would be nonsense. Everything here is derived from the files on
//! disk, so re-running the report on an affected interview picks it up without a migration.

use std::io::Read;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::capture;
use crate::models::{Mode, fmt_ts};

/// A track ending less than this before the other is just the two streams stopping at slightly
/// different moments.
const STOPPED_TOLERANCE_S: f64 = 10.0;
/// Dropouts shorter than this in total aren't worth a warning.
const GAPS_TOLERANCE_S: f64 = 5.0;
/// Above this share of the interview missing from either side, talk-time metrics mislead.
const METRICS_MAX_MISSING: f64 = 0.10;

/// What's missing from a recording, for the candidate and for the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RecordingNotes {
    /// Shown to the candidate: the report banner and the app's Recording stage.
    pub for_you: Vec<String>,
    /// Given to the model alongside the transcript.
    pub for_model: Vec<String>,
    /// One side is missing for a meaningful part of the interview, so talk-time metrics
    /// (share, answer lengths, pace) would be wrong and are left out.
    pub incomplete: bool,
}

impl RecordingNotes {
    pub fn is_empty(&self) -> bool {
        self.for_you.is_empty()
    }
}

/// How much of the interview one track holds: its length, and silence the recorder filled in for
/// dropouts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackSpan {
    pub seconds: f64,
    pub gaps: f64,
}

/// Notes for a dual-track recording from its two track spans.
pub fn notes(mic: TrackSpan, system: TrackSpan) -> RecordingNotes {
    let total = mic.seconds.max(system.seconds);
    let mut out = RecordingNotes::default();
    if total <= 0.0 {
        return out;
    }
    for (span, is_mic) in [(mic, true), (system, false)] {
        let stopped = total - span.seconds;
        if stopped > STOPPED_TOLERANCE_S {
            let (at, of) = (span.seconds, total);
            if is_mic {
                out.for_you.push(format!(
                    "Your microphone stopped recording at {} of {} — nothing you said after that was captured. \
                     The interviewer's side is complete.",
                    clock(at), clock(of)
                ));
                out.for_model.push(format!(
                    "The candidate's microphone track stopped at {} but the interview ran to {} (a recorder fault). \
                     Everything the candidate said after {} is missing from the transcript, even though they were \
                     answering throughout. The interviewer's side is complete.",
                    fmt_ts(at), fmt_ts(of), fmt_ts(at)
                ));
            } else {
                out.for_you.push(format!(
                    "The call audio stopped recording at {} of {} — nothing the interviewer said after that was \
                     captured. Your side is complete.",
                    clock(at), clock(of)
                ));
                out.for_model.push(format!(
                    "The interviewer's (call audio) track stopped at {} but the interview ran to {} (a recorder \
                     fault). Everything the interviewer said after {} is missing from the transcript. The candidate's \
                     side is complete.",
                    fmt_ts(at), fmt_ts(of), fmt_ts(at)
                ));
            }
        }
        if span.gaps > GAPS_TOLERANCE_S {
            let who = if is_mic { ("Your microphone", "you", "The candidate's microphone track", "the candidate") }
                      else { ("The call audio", "the interviewer", "The interviewer's (call audio) track", "the interviewer") };
            out.for_you.push(format!(
                "{} dropped out for {} in total; anything {} said during the dropouts is missing.",
                who.0, clock(span.gaps), who.1
            ));
            out.for_model.push(format!(
                "{} has dropouts totalling {} (filled with silence by the recorder); anything {} said then is missing.",
                who.2, clock(span.gaps), who.3
            ));
        }
        if (stopped.max(0.0) + span.gaps) / total > METRICS_MAX_MISSING {
            out.incomplete = true;
        }
    }
    out
}

/// Notes for a session's recording, from its normalized tracks and the recorder's report.
/// Single-track recordings (imports) have nothing to compare, so they get none.
pub fn recording_notes(dir: &Path, mode: Mode) -> RecordingNotes {
    if mode == Mode::Single {
        return RecordingNotes::default();
    }
    let recorder = capture::read_report(dir);
    let span = |name: &str| -> Option<TrackSpan> {
        let seconds = flac_duration_s(&dir.join(format!("{name}.flac"))).ok()?;
        let stats = recorder.as_ref().map(|r| &r["tracks"][name]);
        let gaps = stats
            .and_then(|s| Some(s["gap_fill_frames"].as_f64()? / s["sample_rate"].as_f64().filter(|r| *r > 0.0)?))
            .unwrap_or(0.0);
        Some(TrackSpan { seconds, gaps })
    };
    match (span("mic"), span("system")) {
        (Some(mic), Some(system)) => notes(mic, system),
        _ => RecordingNotes::default(),
    }
}

/// Length of a FLAC file from its STREAMINFO block, without decoding it.
pub fn flac_duration_s(path: &Path) -> Result<f64> {
    let mut head = [0u8; 26];
    std::fs::File::open(path)?.read_exact(&mut head)?;
    // "fLaC", then the first metadata block's 4-byte header (STREAMINFO is always first), then
    // STREAMINFO: 10 bytes of block/frame sizes, then 64 bits packing sample rate (20),
    // channels-1 (3), bits-per-sample-1 (5) and total samples (36).
    if &head[..4] != b"fLaC" || head[4] & 0x7f != 0 {
        bail!("{} isn't a FLAC file", path.display());
    }
    let packed = u64::from_be_bytes(head[18..26].try_into().expect("8 bytes"));
    let rate = packed >> 44;
    let samples = packed & ((1 << 36) - 1);
    if rate == 0 || samples == 0 {
        bail!("{} doesn't record its length", path.display());
    }
    Ok(samples as f64 / rate as f64)
}

/// "0:25", "41:42", "1:02:03".
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(seconds: f64) -> TrackSpan {
        TrackSpan { seconds, gaps: 0.0 }
    }

    /// The real failure: the mic stopped 25 s into a 42-minute interview.
    #[test]
    fn mic_that_stopped_early() {
        let n = notes(span(25.2), span(2502.3));
        assert!(n.incomplete);
        assert_eq!(n.for_you.len(), 1);
        assert!(n.for_you[0].starts_with("Your microphone stopped recording at 0:25 of 41:42"), "{}", n.for_you[0]);
        assert!(n.for_model[0].contains("stopped at 00:00:25 but the interview ran to 00:41:42"), "{}", n.for_model[0]);
    }

    #[test]
    fn call_audio_that_stopped_early() {
        let n = notes(span(600.0), span(100.0));
        assert!(n.incomplete);
        assert!(n.for_you[0].starts_with("The call audio stopped recording at 1:40 of 10:00"), "{}", n.for_you[0]);
    }

    #[test]
    fn tracks_ending_together_are_complete() {
        assert_eq!(notes(span(595.0), span(600.0)), RecordingNotes::default());
    }

    #[test]
    fn dropouts_are_noted_and_only_long_ones_skew_metrics() {
        let short = notes(TrackSpan { seconds: 600.0, gaps: 30.0 }, span(600.0));
        assert_eq!(short.for_you, vec!["Your microphone dropped out for 0:30 in total; anything you said during the \
                                         dropouts is missing.".to_string()]);
        assert!(!short.incomplete, "5% missing still gives usable metrics");
        assert!(notes(TrackSpan { seconds: 600.0, gaps: 120.0 }, span(600.0)).incomplete);
        assert!(notes(TrackSpan { seconds: 600.0, gaps: 3.0 }, span(600.0)).is_empty());
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(25.2), "0:25");
        assert_eq!(clock(2502.3), "41:42");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
