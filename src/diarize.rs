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
use crate::download;
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

pub fn is_downloaded(settings: &Settings) -> bool {
    segmentation_model(settings).exists() && embedding_model(settings).exists()
}

/// Who spoke when. `num_speakers` fixes the number of clusters (interviews are usually 2).
pub fn diarize(samples: &[f32], num_speakers: Option<usize>, settings: &Settings, progress: &mut dyn Progress)
    -> Result<Vec<SpeechSpan>> {
    let segmentation = segmentation_model(settings);
    download::ensure_archive(SEGMENTATION_URL, &settings.models_dir, &segmentation, "speaker-segmentation model", progress)?;
    let embedding = download::ensure_file(EMBEDDING_URL, &embedding_model(settings), "speaker-embedding model", progress)?;
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
