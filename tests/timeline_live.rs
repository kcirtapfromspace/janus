//! The room's temperature on the two scripted interviews, with the real Jev and the real voices:
//! the strong interview, where the interviewer praises, sells and sets up the onsite, must read
//! warmer than the weak one. Spends a few cents' worth of Jev calls.
//!
//!   IC_LLM_URL=http://127.0.0.1:4000 IC_LLM_KEY=sk-... cargo test --release --test timeline_live -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::Path;

use interview_coach::llm::jev;
use interview_coach::models::Segment;
use interview_coach::proxy::LlmEndpoint;
use interview_coach::scoring::JevScorer;
use interview_coach::temperature::{self, Audio, Kind, Signal};
use interview_coach::{audio, prosody};

fn timeline(name: &str, scorer: &JevScorer) -> Vec<Signal> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/out").join(name);
    let segments: Vec<Segment> = serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    let frames = |wav: &str| {
        let samples = audio::load(&dir.join(wav)).unwrap();
        prosody::frames(&samples, prosody::noise_floor_db(&samples))
    };
    let audio = Audio { interviewer: frames("system.wav"), you: frames("mic.wav"), separate: true };
    let convo = temperature::conversation(&segments);
    let (assessments, errors) = temperature::assess_turns(scorer, &convo);
    assert!(errors.is_empty(), "{errors:?}");
    temperature::build(&convo, &assessments, Some(&audio))
}

fn mean_temperature(signals: &[Signal]) -> f64 {
    let t: Vec<f64> = signals.iter().filter_map(|s| s.temperature).collect();
    t.iter().sum::<f64>() / t.len() as f64
}

fn show(name: &str, signals: &[Signal]) {
    let m = temperature::moments(signals);
    let at = |idx: usize| signals.iter().find(|s| s.turn_idx == idx).map_or(0.0, |s| s.start);
    println!("\n{name}: mean {:+.2}, shift {:?}", mean_temperature(signals),
             m.shift.as_ref().map(|s| (at(s.after_answer), (s.delta * 100.0).round() / 100.0)));
    let starts = |list: &[&Signal]| list.iter().map(|s| s.start).collect::<Vec<_>>();
    println!("  warmest {:?} coolest {:?} next steps {:?}", starts(&m.warmest), starts(&m.coolest), starts(&m.next_steps));
    for s in signals {
        let picks: BTreeMap<&str, &str> = s.checks.iter().map(|(k, v)| (k.as_str(), v.pick.as_str())).collect();
        match s.kind {
            Kind::Substantive => println!("  {:>6.1}s {:+.2} (line {:+.2}) {:?} {:?}", s.start, s.temperature.unwrap_or(0.0),
                                          s.smoothed.unwrap_or(0.0), temperature::cues(s), picks),
            _ => println!("  {:>6.1}s {:?} {:?}", s.start, s.kind, temperature::voice_notes(s)),
        }
    }
}

#[test]
#[ignore = "needs the proxy with a TypeSafe key: set IC_LLM_URL and IC_LLM_KEY"]
fn the_strong_interview_reads_warmer_than_the_weak_one() {
    let endpoint = LlmEndpoint { base_url: std::env::var("IC_LLM_URL").expect("IC_LLM_URL"),
                                 api_key: std::env::var("IC_LLM_KEY").expect("IC_LLM_KEY") };
    let client = jev::Client::new(endpoint);
    let scorer = JevScorer { client: &client, model: jev::DEFAULT_MODEL.into() };
    let (strong, weak) = (timeline("strong", &scorer), timeline("weak", &scorer));
    show("strong", &strong);
    show("weak", &weak);
    assert!(strong.iter().filter(|s| s.kind == Kind::Substantive).all(|s| s.voice.is_some()), "every turn is measured");
    assert!(mean_temperature(&strong) > mean_temperature(&weak) + 0.2);
    assert!(temperature::moments(&strong).next_steps.len() == 1, "the onsite is set up once, at the end");
}
