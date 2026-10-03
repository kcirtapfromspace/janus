//! Speaker diarization for single-track uploads: sherpa-onnx (pyannote segmentation + NeMo TitaNet
//! speaker embeddings), called through its C API so we can use more than one thread. No account or token.
//!
//! TitaNet was chosen over CAM++/ResNet34: on our fixtures those sometimes collapsed two clearly
//! different voices into one cluster even with the cluster count fixed at 2.

use std::ffi::CString;
use std::path::PathBuf;

use anyhow::{Result, bail};
use sherpa_rs::sherpa_rs_sys as sys;

use crate::config::Settings;
use crate::download::{self, Asset};
use crate::models::SpeechSpan;
use crate::progress::Progress;

const SEGMENTATION_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
// "recongition" is how the release is actually named.
const EMBEDDING_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx";

fn segmentation_model(settings: &Settings) -> PathBuf {
    settings.models_dir.join("sherpa-onnx-pyannote-segmentation-3-0/model.onnx")
}

fn embedding_model(settings: &Settings) -> PathBuf {
    settings.models_dir.join("nemo_en_titanet_small.onnx")
}

fn segmentation_asset() -> Asset {
    Asset::pinned(SEGMENTATION_URL, "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488", 6_958_444)
}

fn embedding_asset() -> Asset {
    Asset::pinned(EMBEDDING_URL, "ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e", 40_257_283)
}

pub fn is_downloaded(settings: &Settings) -> bool {
    segmentation_model(settings).exists() && embedding_asset().is_present(&embedding_model(settings))
}

/// Bytes still to download for speaker detection.
pub fn download_size(settings: &Settings) -> u64 {
    let mut total = 0;
    if !segmentation_model(settings).exists() {
        total += segmentation_asset().size.unwrap_or(0);
    }
    if !embedding_asset().is_present(&embedding_model(settings)) {
        total += embedding_asset().size.unwrap_or(0);
    }
    total
}

/// Fetch the speaker-detection models (used for single-track imports).
pub fn download(settings: &Settings, progress: &mut dyn Progress) -> Result<PathBuf> {
    download::ensure_archive(&segmentation_asset(), &settings.models_dir, &segmentation_model(settings),
                             "speaker-segmentation model", progress)?;
    download::ensure_file(&embedding_asset(), &embedding_model(settings), "speaker-embedding model", progress)
}

/// Who spoke when. `num_speakers` fixes the number of clusters (interviews are usually 2).
pub fn diarize(samples: &[f32], num_speakers: Option<usize>, settings: &Settings, progress: &mut dyn Progress)
    -> Result<Vec<SpeechSpan>> {
    let segmentation = segmentation_model(settings);
    let embedding = download(settings, progress)?;
    progress.stage("Detecting speakers");

    let seg = CString::new(segmentation.to_string_lossy().as_bytes())?;
    let emb = CString::new(embedding.to_string_lossy().as_bytes())?;
    let provider = CString::new("cpu")?;
    let threads = 8;
    let config = sys::SherpaOnnxOfflineSpeakerDiarizationConfig {
        segmentation: sys::SherpaOnnxOfflineSpeakerSegmentationModelConfig {
            pyannote: sys::SherpaOnnxOfflineSpeakerSegmentationPyannoteModelConfig { model: seg.as_ptr() },
            num_threads: threads,
            debug: 0,
            provider: provider.as_ptr(),
        },
        embedding: sys::SherpaOnnxSpeakerEmbeddingExtractorConfig {
            model: emb.as_ptr(),
            num_threads: threads,
            debug: 0,
            provider: provider.as_ptr(),
        },
        // num_clusters <= 0 means "decide from the threshold".
        clustering: sys::SherpaOnnxFastClusteringConfig {
            num_clusters: num_speakers.map_or(-1, |n| n as i32),
            threshold: 0.5,
        },
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };

    let mut spans = vec![];
    // SAFETY: the CStrings outlive every call; each object created here is destroyed exactly once,
    // and the segment slice is read before its backing memory is freed.
    unsafe {
        let sd = sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config);
        if sd.is_null() {
            bail!("couldn't load the speaker-detection models from {}", settings.models_dir.display());
        }
        let result = sys::SherpaOnnxOfflineSpeakerDiarizationProcess(sd, samples.as_ptr(), samples.len() as i32);
        if result.is_null() {
            sys::SherpaOnnxDestroyOfflineSpeakerDiarization(sd);
            bail!("speaker detection failed");
        }
        let n = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result);
        let segs = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
        if !segs.is_null() && n > 0 {
            for s in std::slice::from_raw_parts(segs, n as usize) {
                spans.push(SpeechSpan { start: s.start as f64, end: s.end as f64, speaker: format!("speaker_{}", s.speaker) });
            }
        }
        if !segs.is_null() {
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segs);
        }
        sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
        sys::SherpaOnnxDestroyOfflineSpeakerDiarization(sd);
    }
    Ok(spans)
}
