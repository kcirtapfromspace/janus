//! Local Whisper transcription: whisper.cpp on the GPU (Metal), one voice-activity chunk at a time.
//!
//! We transcribe speech chunks individually rather than the whole file: on dual-track recordings
//! each track is mostly silence while the other person talks, and Whisper invents text on long
//! silence. (whisper.cpp's built-in VAD mode mis-maps timestamps back onto the original timeline,
//! so we run its VAD standalone and offset each chunk's timestamps ourselves.)

use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext,
                 WhisperVadContextParams, WhisperVadParams};

use crate::audio::SR;
use crate::config::Settings;
use crate::download::{self, Asset};
use crate::models::{Segment, Word};
use crate::progress::Progress;

/// Whisper tidies away disfluencies unless the prompt shows them; we need them for filler metrics.
pub const FILLER_PROMPT: &str = "Umm, so, uh, I was like, you know, thinking about it. Hmm, okay.";

const VAD_MODEL: &str = "ggml-silero-v6.2.0.bin";
const MAX_CHUNK_S: f32 = 28.0;
const MAX_GAP_S: f32 = 2.0;

/// Downloads come from fixed revisions, checked against their published SHA-256.
const WHISPER_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";
const VAD_REVISION: &str = "9ffd54a1e1ee413ddf265af9913beaf518d1639b";
const PINNED_WHISPER: &[(&str, &str, u64)] = &[
    ("large-v3-turbo", "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69", 1_624_555_275),
    ("large-v3-turbo-q5_0", "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2", 574_041_195),
];

pub fn model_path(settings: &Settings) -> PathBuf {
    settings.models_dir.join(format!("ggml-{}.bin", settings.whisper_model))
}

/// The configured Whisper model. Models outside the pinned list download unverified.
pub fn whisper_asset(model: &str) -> Asset {
    match PINNED_WHISPER.iter().find(|(name, ..)| *name == model) {
        Some((_, sha, size)) => Asset::pinned(
            format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/{WHISPER_REVISION}/ggml-{model}.bin"), sha, *size),
        None => Asset::unpinned(format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model}.bin")),
    }
}

fn vad_asset() -> Asset {
    Asset::pinned(format!("https://huggingface.co/ggml-org/whisper-vad/resolve/{VAD_REVISION}/{VAD_MODEL}"),
                  "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987", 885_098)
}

pub fn is_downloaded(settings: &Settings) -> bool {
    whisper_asset(&settings.whisper_model).is_present(&model_path(settings))
        && vad_asset().is_present(&settings.models_dir.join(VAD_MODEL))
}

/// Bytes still to download for transcription (0 when everything's there; unknown sizes count as 0).
pub fn download_size(settings: &Settings) -> u64 {
    let whisper = whisper_asset(&settings.whisper_model);
    let mut total = 0;
    if !whisper.is_present(&model_path(settings)) {
        total += whisper.size.unwrap_or(0);
    }
    if !vad_asset().is_present(&settings.models_dir.join(VAD_MODEL)) {
        total += vad_asset().size.unwrap_or(0);
    }
    total
}

/// Fetch the Whisper and voice-activity models (setup does this up front; `load` falls back to it).
pub fn download(settings: &Settings, progress: &mut dyn Progress) -> Result<(PathBuf, PathBuf)> {
    let model = download::ensure_file(&whisper_asset(&settings.whisper_model), &model_path(settings),
                                      &format!("speech model (Whisper {})", settings.whisper_model), progress)?;
    let vad = download::ensure_file(&vad_asset(), &settings.models_dir.join(VAD_MODEL), "voice-activity model",
                                    progress)?;
    Ok((model, vad))
}

pub struct Transcriber {
    ctx: WhisperContext,
    vad_model: PathBuf,
    language: Option<String>,
}

impl Transcriber {
    /// Loads the model, downloading it (and the VAD model) on first use.
    pub fn load(settings: &Settings, progress: &mut dyn Progress) -> Result<Self> {
        if std::env::var_os("IC_DEBUG").is_none() {
            whisper_rs::install_logging_hooks(); // whisper.cpp logs to stderr otherwise
        }
        let (model, vad_model) = download(settings, progress)?;
        progress.stage("Loading Whisper");
        let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
            .with_context(|| format!("loading {}", model.display()))?;
        Ok(Transcriber { ctx, vad_model, language: settings.language.clone() })
    }

    fn params(&self) -> FullParams<'_, '_> {
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(self.language.as_deref());
        p.set_no_context(true); // each chunk stands alone; carried-over context causes repetition loops
        p.set_initial_prompt(FILLER_PROMPT);
        p.set_token_timestamps(true);
        p.set_n_threads(8);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        p
    }

    /// Seconds ranges that contain speech.
    fn speech_regions(&self, samples: &[f32]) -> Result<Vec<(f32, f32)>> {
        let mut vad = WhisperVadContext::new(&self.vad_model.to_string_lossy(), WhisperVadContextParams::default())
            .context("loading the voice-activity model")?;
        let mut vp = WhisperVadParams::new();
        vp.set_min_silence_duration(300);
        vp.set_speech_pad(200);
        vp.set_max_speech_duration(MAX_CHUNK_S);
        // whisper.cpp reports VAD times in centiseconds.
        Ok(vad.segments_from_samples(vp, samples)?.map(|s| (s.start / 100.0, s.end / 100.0)).collect())
    }

    pub fn transcribe(&self, samples: &[f32], progress: &mut dyn Progress) -> Result<Vec<Segment>> {
        let chunks = group_regions(&self.speech_regions(samples)?, MAX_GAP_S, MAX_CHUNK_S);
        let eot = self.ctx.token_eot();
        let mut state = self.ctx.create_state()?;
        let mut out = vec![];
        for (i, &(c_start, c_end)) in chunks.iter().enumerate() {
            let lo = ((c_start * SR as f32) as usize).min(samples.len());
            let hi = ((c_end * SR as f32) as usize).min(samples.len());
            state.full(self.params(), &samples[lo..hi])?;
            let offset = c_start as f64;
            for seg in state.as_iter() {
                let text = seg.to_str_lossy()?.trim().to_string();
                let mut tokens = vec![];
                for t in 0..seg.n_tokens() {
                    let Some(tk) = seg.get_token(t) else { continue };
                    if tk.token_id() < eot {
                        let d = tk.token_data();
                        tokens.push((tk.to_str_lossy()?.into_owned(), d.t0, d.t1, d.plog));
                    }
                }
                let avg_logprob = if tokens.is_empty() {
                    0.0
                } else {
                    tokens.iter().map(|t| t.3 as f64).sum::<f64>() / tokens.len() as f64
                };
                if is_junk(&text, seg.no_speech_probability() as f64, avg_logprob, FILLER_PROMPT) {
                    continue;
                }
                out.push(Segment {
                    start: offset + seg.start_timestamp() as f64 / 100.0,
                    end: offset + seg.end_timestamp() as f64 / 100.0,
                    words: words_from_tokens(&tokens, offset),
                    text,
                    speaker: String::new(),
                });
            }
            progress.step(i as u64 + 1, chunks.len() as u64);
        }
        Ok(out)
    }
}

/// Merge nearby speech regions into chunks: bigger chunks give Whisper context; short gaps keep it
/// from hallucinating on silence.
pub fn group_regions(regions: &[(f32, f32)], max_gap: f32, max_len: f32) -> Vec<(f32, f32)> {
    let mut sorted = regions.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut chunks: Vec<(f32, f32)> = vec![];
    for (start, end) in sorted {
        if let Some(last) = chunks.last_mut()
            && start - last.1 <= max_gap
            && end - last.0 <= max_len
        {
            last.1 = last.1.max(end);
            continue;
        }
        chunks.push((start, end));
    }
    chunks
}

/// Whisper tokens are word pieces; a new word starts at a token with a leading space.
/// Token times are centiseconds relative to the chunk.
fn words_from_tokens(tokens: &[(String, i64, i64, f32)], offset: f64) -> Vec<Word> {
    let mut words: Vec<Word> = vec![];
    for (text, t0, t1, _) in tokens {
        let (start, end) = (offset + *t0 as f64 / 100.0, offset + *t1 as f64 / 100.0);
        match words.last_mut() {
            Some(w) if !text.starts_with(' ') => {
                w.text.push_str(text);
                w.end = end;
            }
            _ => words.push(Word { start, end, text: text.clone() }),
        }
    }
    words
}

static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z']+").unwrap());

fn norm(text: &str) -> String {
    WORDS.find_iter(&text.to_lowercase()).map(|m| m.as_str()).collect::<Vec<_>>().join(" ")
}

/// Segments that are Whisper artifacts rather than speech.
pub fn is_junk(text: &str, no_speech_prob: f64, avg_logprob: f64, prompt: &str) -> bool {
    let t = norm(text);
    if t.is_empty() {
        return true;
    }
    // Whisper's own "this was silence" signal: likely no speech and a low-confidence decode.
    if no_speech_prob > 0.6 && avg_logprob < -1.0 {
        return true;
    }
    // Repetition loops ("the the the the ...").
    let words: Vec<&str> = t.split(' ').collect();
    if words.len() >= 8 {
        let unique: std::collections::HashSet<_> = words.iter().collect();
        if (unique.len() as f64) < 0.3 * words.len() as f64 {
            return true;
        }
    }
    // On unclear audio Whisper sometimes regurgitates its prompt.
    t.len() > 10 && norm(prompt).contains(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_regions_merges_close_regions_up_to_max_length() {
        let regions = [(0.0, 3.0), (3.5, 6.0), (10.0, 12.0), (12.5, 40.0), (40.2, 41.0)];
        assert_eq!(group_regions(&regions, 2.0, 28.0), [(0.0, 6.0), (10.0, 12.0), (12.5, 40.0), (40.2, 41.0)]);
    }

    #[test]
    fn junk_filter() {
        assert!(!is_junk("I led the onboarding redesign.", 0.1, -0.3, FILLER_PROMPT));
        assert!(is_junk("   ", 0.1, -0.3, FILLER_PROMPT));
        assert!(is_junk("Thank you.", 0.9, -1.5, FILLER_PROMPT));
        assert!(is_junk("the the the the the the the the the", 0.1, -0.3, FILLER_PROMPT));
        assert!(is_junk("I was like, you know, thinking about it.", 0.1, -0.3, FILLER_PROMPT)); // prompt echo
        assert!(!is_junk("Um, okay.", 0.1, -0.3, FILLER_PROMPT)); // short fillers are real speech
    }

    #[test]
    fn words_join_token_pieces() {
        let tokens = [(" Activ".to_string(), 100, 120, 0.0), ("ation".into(), 120, 140, 0.0), (" rose".into(), 150, 170, 0.0)];
        let words = words_from_tokens(&tokens, 10.0);
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), [" Activation", " rose"]);
        assert_eq!((words[0].start, words[0].end), (11.0, 11.4));
    }
}
