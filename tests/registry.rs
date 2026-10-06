//! Sharing an interview's questions with the registry: only when opted in, made generic first,
//! checked again for names, sent once, never logistics, never from practice interviews.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use common::fake::*;
use interview_coach::config::Settings;
use interview_coach::models::Mode;
use interview_coach::pipeline::analyze_session;
use interview_coach::progress::Quiet;
use interview_coach::registry::contribute_session;
use serde_json::{Value, json};

/// A stand-in registry that takes one request and hands its body back.
fn registry() -> (String, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        tx.send(serde_json::from_slice(&body).unwrap()).unwrap();
        let reply = r#"{"accepted":1,"duplicates":0}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}", reply.len()).unwrap();
    });
    (url, rx)
}

/// The sample review, with three questions: one naming the company, one logistics, one naming
/// an interviewer.
fn review_with_questions() -> String {
    let mut analysis: Value = serde_json::from_str(&sample(false, GOOD_QUOTE)).unwrap();
    let question = |ts: &str, text: &str, kind: &str| json!({"timestamp": ts, "question": text, "type": kind, "answer_summary": "x",
                                                               "score": 3, "what_worked": "x", "what_was_missing": "x", "stronger_answer": "x"});
    analysis["questions"] = json!([
        question("00:00:00", "Tell me about the Atlas migration you led at Globex.", "behavioral"),
        question("00:00:20", "When could you start?", "logistics"),
        question("00:00:31", "How would you handle pushback from Daniel's team at Northwind?", "situational"),
    ]);
    analysis.to_string()
}

#[test]
fn questions_are_shared_generic_once_and_only_when_opted_in() {
    let (tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![
        Ok(review_with_questions()),
        // The scrubbing model: the first made generic, the second still naming the company.
        Ok(json!({"questions": ["Tell me about a migration you led.", "How would you handle pushback from Northwind's team?"]}).to_string()),
    ]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    let base = Settings { data_dir: tmp.path().to_path_buf(), ..Settings::load().unwrap() };

    let off = Settings { share_questions: false, ..base.clone() };
    interview_coach::privacy::mark_seen(&base).unwrap();
    assert_eq!(contribute_session(&off, &db, &llm, "m", id).unwrap(), 0, "turned off: nothing is sent");
    std::fs::remove_file(tmp.path().join("privacy-notice-seen")).unwrap();
    assert!(base.share_questions, "on by default…");
    assert_eq!(contribute_session(&base, &db, &llm, "m", id).unwrap(), 0, "…but nothing is sent before the notice is shown");
    interview_coach::privacy::mark_seen(&base).unwrap();

    let (url, sent) = registry();
    let settings = Settings { registry_url: url, ..base };
    assert_eq!(contribute_session(&settings, &db, &llm, "m", id).unwrap(), 1);
    let body = sent.recv().unwrap();
    assert_eq!(body["install"].as_str().unwrap().len(), 32);
    let shared = body["questions"].as_array().unwrap();
    assert_eq!(shared.len(), 1, "logistics isn't sent, and a question still naming the company is dropped: {body}");
    assert_eq!(shared[0]["text"], "Tell me about a migration you led.");
    assert_eq!(shared[0]["kind"], "behavioral");
    assert_eq!(shared[0]["round"], "Hiring manager");
    assert_eq!(shared[0]["role"], "Senior PM");
    assert_eq!(shared[0]["company"], Value::Null, "the company only goes with share_company");

    // What the scrubbing model was asked: the questions, never the transcript.
    let (_, request) = llm.requests.borrow().last().cloned().unwrap();
    let asked = request.to_string();
    assert!(asked.contains("Atlas migration") && !asked.contains("onboarding redesign"), "only the questions go to the model");

    // Sent once: a second run has nothing new, and makes no request (the stand-in takes one).
    assert_eq!(contribute_session(&settings, &db, &llm, "m", id).unwrap(), 0);

    // Practice interviews' questions came from the registry, so they're never sent back.
    db.set_practice(id).unwrap();
    std::fs::remove_file(tmp.path().join("registry/sent.json")).unwrap();
    assert_eq!(contribute_session(&settings, &db, &llm, "m", id).unwrap(), 0);
}
