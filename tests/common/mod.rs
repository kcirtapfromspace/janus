//! Synthetic interview recordings made with macOS `say`, for end-to-end tests.
//!
//! For each script in tests/fixtures/*.txt, writes tests/fixtures/out/<name>/:
//!   mic.wav        the candidate only (Samantha) — like the recorder's mic track
//!   system.wav     the interviewer only (Daniel) — like the recorder's system track
//!   mixed.wav      both voices on one track — like a Zoom export or voice memo
//!   expected.json  the timeline: [{speaker, start, end, text}]
//!
//! These prove the pipeline is wired correctly. TTS voices are far easier to tell apart than real
//! people, so passing here does NOT show that speaker detection works on real calls.

#![allow(dead_code)]

pub mod fake;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

const SR: u32 = 16_000;
const GAP_S: f64 = 0.7;

fn voice(who: &str) -> (&'static str, &'static str) {
    match who {
        "I" => ("Daniel", "interviewer"),
        "Y" => ("Samantha", "you"),
        other => panic!("unknown speaker {other:?} in fixture script"),
    }
}

fn say(text: &str, voice: &str, tmp: &Path) -> Vec<f32> {
    let wav = tmp.join("line.wav");
    let status = Command::new("say")
        .args(["-v", voice, "-o"])
        .arg(&wav)
        .args(["--file-format=WAVE", &format!("--data-format=LEI16@{SR}"), text])
        .status()
        .expect("running say");
    assert!(status.success());
    let mut reader = hound::WavReader::open(&wav).unwrap();
    assert_eq!(reader.spec().sample_rate, SR);
    reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

fn write_wav(path: &Path, samples: &[f32]) {
    let spec = hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

/// A faint, deterministic noise floor (about -65 dBFS), like a real mic, so gaps aren't digital zero.
fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 2.0 * 9.7e-4
        })
        .collect()
}

pub fn build(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let out = root.join("out").join(name);
    if out.join("expected.json").exists() {
        return out;
    }
    std::fs::create_dir_all(&out).unwrap();
    let script = std::fs::read_to_string(root.join(format!("{name}.txt"))).unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let mut pieces = vec![];
    let mut t = 0.5;
    for line in script.lines().filter(|l| !l.trim().is_empty()) {
        let (who, text) = line.split_once(':').expect("script lines look like 'I: text'");
        let (voice_name, speaker) = voice(who.trim());
        let audio = say(text.trim(), voice_name, tmp.path());
        let len = audio.len() as f64 / SR as f64;
        pieces.push((speaker, t, audio, text.trim().to_string()));
        t += len + GAP_S;
    }
    let total = ((t + 0.5) * SR as f64) as usize;
    let (mut you, mut them) = (vec![0.0f32; total], vec![0.0f32; total]);
    let mut expected: Vec<Value> = vec![];
    for (speaker, start, audio, text) in &pieces {
        let track = if *speaker == "you" { &mut you } else { &mut them };
        let i = (start * SR as f64) as usize;
        for (j, s) in audio.iter().enumerate() {
            track[i + j] += s;
        }
        let end = start + audio.len() as f64 / SR as f64;
        expected.push(json!({"speaker": speaker, "start": (start * 1000.0).round() / 1000.0,
                             "end": (end * 1000.0).round() / 1000.0, "text": text}));
    }
    let add = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x + y).collect::<Vec<f32>>();
    write_wav(&out.join("mic.wav"), &add(&you, &noise(total, 1)));
    write_wav(&out.join("system.wav"), &add(&them, &noise(total, 2)));
    write_wav(&out.join("mixed.wav"), &add(&add(&you, &them), &noise(total, 3)));
    std::fs::write(out.join("expected.json"), serde_json::to_string_pretty(&expected).unwrap()).unwrap();
    out
}

pub fn expected(dir: &Path) -> Vec<(String, f64, f64)> {
    let v: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    v.iter()
        .map(|e| (e["speaker"].as_str().unwrap().to_string(), e["start"].as_f64().unwrap(), e["end"].as_f64().unwrap()))
        .collect()
}

/// Share of transcript time whose speaker matches the script's speaker at that moment.
pub fn speaker_accuracy(segments: &[interview_coach::models::Segment], expected: &[(String, f64, f64)]) -> f64 {
    let (mut right, mut total) = (0.0, 0.0);
    for seg in segments {
        for (speaker, start, end) in expected {
            let overlap = seg.end.min(*end) - seg.start.max(*start);
            if overlap > 0.0 {
                total += overlap;
                if *speaker == seg.speaker {
                    right += overlap;
                }
            }
        }
    }
    right / total
}
