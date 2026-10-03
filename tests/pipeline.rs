//! End-to-end pipeline on synthetic recordings (runs Whisper, so it's slow):
//!   cargo test --release --test pipeline -- --ignored

mod common;

use interview_coach::config::Settings;
use interview_coach::db::Db;
use interview_coach::merge::to_turns;
use interview_coach::pipeline::{ingest_file, ingest_tracks, transcribe_session};
use interview_coach::progress::Quiet;

fn env() -> (tempfile::TempDir, Settings, Db) {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings { data_dir: dir.path().to_path_buf(), ..Settings::load().unwrap() };
    let db = Db::open(&settings.db_path()).unwrap();
    (dir, settings, db)
}

#[test]
#[ignore = "slow: runs Whisper"]
fn dual_track_import_labels_every_turn_correctly() {
    let fixture = common::build("strong");
    let expected = common::expected(&fixture);
    let (_tmp, settings, mut db) = env();
    let s = ingest_tracks(&mut db, &settings, &fixture.join("mic.wav"), &fixture.join("system.wav"), "strong", None, &mut Quiet).unwrap();
    let result = transcribe_session(&mut db, &settings, s.id, &mut Quiet).unwrap();
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    let turns = to_turns(&result.segments);
    let speakers: Vec<_> = turns.iter().map(|t| t.speaker.as_str()).collect();
    let want: Vec<_> = expected.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(speakers, want);
    let accuracy = common::speaker_accuracy(&result.segments, &expected);
    assert!(accuracy > 0.98, "speaker accuracy {accuracy:.3}");
    assert!(turns[5].text.contains("78") || turns[5].text.to_lowercase().contains("seventy"), "{}", turns[5].text);
}

#[test]
#[ignore = "slow: runs Whisper"]
fn silent_system_track_warns_and_keeps_your_side() {
    let fixture = common::build("weak");
    let (tmp, settings, mut db) = env();
    let silent = tmp.path().join("silent.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&silent, spec).unwrap();
    for _ in 0..16_000 * 5 {
        w.write_sample(0i16).unwrap();
    }
    w.finalize().unwrap();
    let s = ingest_tracks(&mut db, &settings, &fixture.join("mic.wav"), &silent, "weak", None, &mut Quiet).unwrap();
    let result = transcribe_session(&mut db, &settings, s.id, &mut Quiet).unwrap();
    assert!(result.warnings.iter().any(|w| w.contains("System Audio Recording")), "{:?}", result.warnings);
    assert!(result.segments.iter().all(|seg| seg.speaker == "you"));
}

#[test]
#[ignore = "slow: runs Whisper and diarization"]
fn single_track_import_detects_speakers() {
    let fixture = common::build("strong");
    let expected = common::expected(&fixture);
    let (_tmp, settings, mut db) = env();
    let s = ingest_file(&mut db, &settings, &fixture.join("mixed.wav"), "strong mixed", None, Some(2), &mut Quiet).unwrap();
    let result = transcribe_session(&mut db, &settings, s.id, &mut Quiet).unwrap();
    let accuracy = common::speaker_accuracy(&result.segments, &expected);
    assert!(accuracy > 0.9, "speaker accuracy {accuracy:.3}");
}
