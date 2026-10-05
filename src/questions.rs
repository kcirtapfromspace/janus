//! The question registry: every interviewer question from your reviews, with the same question
//! merged across interviews, where it was asked, and how your answers went. Mock interviews draw
//! their questions from it (mock.rs). It's rebuilt from the current reviews whenever it's read,
//! so it never goes stale. Mock interviews' own questions are listed as practice, so they show
//! progress without counting as being asked.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::pipeline;

/// One time a question was asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asked {
    pub session_id: i64,
    pub title: String,
    pub company: Option<String>,
    pub role: Option<String>,
    /// The round, e.g. "Hiring manager".
    pub round: Option<String>,
    /// Where in the interview, HH:MM:SS.
    pub timestamp: String,
    /// Your answer's score in that review, 1–5.
    pub score: Option<u8>,
    pub created_at: String,
    /// Asked in a mock interview.
    pub practice: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// The wording to ask: the latest real interview's.
    pub text: String,
    /// intro, behavioral, technical, situational, motivation, role_specific, logistics, other.
    pub kind: String,
    /// Newest first.
    pub asked: Vec<Asked>,
    /// The latest review's outline of a stronger answer, to practise towards.
    pub stronger_answer: Option<String>,
    /// "yours", "shared" (the published registry) or "built-in".
    pub source: String,
    /// Shared questions: how many people contributed it.
    #[serde(default)]
    pub contributors: usize,
    /// Shared questions: the rounds and roles it was asked in.
    #[serde(default)]
    pub rounds: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

impl Question {
    /// Times asked in real interviews.
    pub fn times(&self) -> usize {
        self.asked.iter().filter(|a| !a.practice).count()
    }

    /// Your latest score on it, real or practice.
    pub fn latest_score(&self) -> Option<u8> {
        self.asked.iter().find_map(|a| a.score)
    }

    pub fn companies(&self) -> BTreeSet<String> {
        self.asked.iter().filter_map(|a| a.company.clone()).collect()
    }
}

/// Words that don't tell two questions apart.
const STOP: &[&str] = &[
    "a", "an", "the", "to", "of", "and", "or", "you", "your", "me", "i", "about", "tell", "can", "could", "would", "what",
    "how", "why", "do", "did", "does", "is", "are", "was", "were", "in", "on", "for", "with", "that", "this", "it", "so",
    "we", "us", "our", "walk", "through", "describe", "give", "example", "time", "when", "there", "have", "had", "any",
    "some", "more", "bit", "little", "just", "really", "maybe", "kind", "sort", "like", "okay", "great", "alright",
];

/// A question's distinctive words, lightly stemmed ("projects" and "project" match).
pub fn key_words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOP.contains(w))
        .map(|w| w.strip_suffix("ing").or_else(|| w.strip_suffix("es")).or_else(|| w.strip_suffix('s')).unwrap_or(w).to_string())
        .collect()
}

/// The same question, worded a little differently: most of their distinctive words are shared.
pub fn same(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a.len() <= 2 || b.len() <= 2 {
        return a == b;
    }
    let shared = a.intersection(b).count() as f64;
    shared / a.union(b).count() as f64 >= 0.6
}

/// Your registry: every question in your interviews' current reviews, merged. Most asked first.
pub fn registry(db: &Db) -> Result<Vec<Question>> {
    let mut out: Vec<(BTreeSet<String>, Question)> = vec![];
    let mut sessions = db.list_sessions()?;
    sessions.retain(|s| s.deleted_at.is_none());
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    for session in sessions {
        let Some(report) = pipeline::current_report(db, session.id)? else { continue };
        let context = &report.analysis.context;
        for q in &report.analysis.questions {
            let asked = Asked {
                session_id: session.id,
                title: session.title.clone(),
                company: session.company.clone().or_else(|| context.company.clone()),
                role: context.role_title.clone(),
                round: Some(context.stage.label().to_string()),
                timestamp: q.timestamp.clone(),
                score: q.score,
                created_at: session.created_at.clone(),
                practice: session.practice,
            };
            let words = key_words(&q.question);
            match out.iter_mut().find(|(w, _)| same(w, &words)) {
                Some((_, existing)) => {
                    // Sessions come newest first: the first real asking's wording stays.
                    if existing.asked.iter().all(|a| a.practice) && !session.practice {
                        existing.text = q.question.clone();
                    }
                    existing.asked.push(asked);
                }
                None => out.push((words, Question {
                    text: q.question.clone(),
                    kind: q.kind.as_str().to_string(),
                    asked: vec![asked],
                    stronger_answer: Some(q.stronger_answer.clone()).filter(|s| !s.trim().is_empty()),
                    source: "yours".into(),
                    contributors: 0,
                    rounds: vec![],
                    roles: vec![],
                })),
            }
        }
    }
    let mut questions: Vec<Question> = out.into_iter().map(|(_, q)| q).collect();
    questions.sort_by(|a, b| b.times().cmp(&a.times()).then(b.asked[0].created_at.cmp(&a.asked[0].created_at)));
    Ok(questions)
}

/// Classic questions, so a mock interview works before you've recorded any.
pub fn built_in() -> Vec<Question> {
    [
        ("intro", "Tell me a bit about yourself and what you're looking for next."),
        ("motivation", "What draws you to this role?"),
        ("behavioral", "Tell me about a time you had to make a tough prioritization call."),
        ("behavioral", "Tell me about a project you're proud of, and what your part in it was."),
        ("behavioral", "Tell me about a time you disagreed with a teammate. How did you resolve it?"),
        ("behavioral", "Tell me about a mistake you made, and what you changed afterwards."),
        ("situational", "What would you do if you realised a deadline you'd committed to couldn't be met?"),
        ("behavioral", "Tell me about a time you influenced a decision without having authority over it."),
        ("role_specific", "What would you want to accomplish in your first ninety days?"),
        ("technical", "Walk me through the hardest technical problem you've solved recently."),
        ("situational", "How do you decide what not to build?"),
        ("behavioral", "Tell me about a time you got critical feedback. What did you do with it?"),
    ]
    .into_iter()
    .map(|(kind, text)| Question {
        text: text.into(),
        kind: kind.into(),
        asked: vec![],
        stronger_answer: None,
        source: "built-in".into(),
        contributors: 0,
        rounds: vec![],
        roles: vec![],
    })
    .collect()
}

/// What a mock interview is for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub company: Option<String>,
    pub role: Option<String>,
    /// A round's label, e.g. "Hiring manager".
    pub round: Option<String>,
}

fn matches(field: &Option<String>, want: &Option<String>) -> bool {
    match (field, want) {
        (Some(have), Some(want)) => {
            let (have, want) = (have.to_lowercase(), want.to_lowercase());
            have.contains(&want) || want.contains(&have)
        }
        _ => false,
    }
}

/// How well a question suits a mock interview for `target`: the same company, role and round count
/// most, then a weak answer last time (worth practising), then how often it comes up (asked of
/// you, or shared by others).
pub fn relevance(q: &Question, target: &Target) -> f64 {
    let any = |f: &dyn Fn(&Asked) -> bool| q.asked.iter().any(f);
    let listed = |list: &[String], want: &Option<String>| list.iter().any(|item| matches(&Some(item.clone()), want));
    let mut score = 0.0;
    if any(&|a| matches(&a.company, &target.company)) {
        score += 3.0;
    }
    if any(&|a| matches(&a.role, &target.role)) || listed(&q.roles, &target.role) {
        score += 2.0;
    }
    if any(&|a| matches(&a.round, &target.round)) || listed(&q.rounds, &target.round) {
        score += 1.0;
    }
    if q.latest_score().is_some_and(|s| s <= 3) {
        score += 2.0;
    }
    score + (1.0 + q.times() as f64).ln() * 0.5 + (1.0 + q.contributors as f64).ln() * 0.4
}

/// How many of one kind a mock interview asks at most.
fn cap(kind: &str) -> usize {
    match kind {
        "intro" | "motivation" => 1,
        "behavioral" => 3,
        _ => 2,
    }
}

/// The questions for a mock interview: the most relevant first, an introduction question to open
/// if there is one, no logistics, a mix of kinds (`cap`), then built-in ones to fill up.
pub fn plan(candidates: &[Question], target: &Target, count: usize) -> Vec<Question> {
    let mut ranked: Vec<&Question> = candidates.iter().filter(|q| q.kind != "logistics").collect();
    ranked.sort_by(|a, b| relevance(b, target).total_cmp(&relevance(a, target)));
    let fallback = built_in();
    let mut picked: Vec<Question> = vec![];
    let mut seen: Vec<BTreeSet<String>> = vec![];
    for q in ranked.into_iter().chain(fallback.iter()) {
        if picked.len() >= count {
            break;
        }
        let words = key_words(&q.text);
        let kind_count = picked.iter().filter(|p| p.kind == q.kind).count();
        if seen.iter().any(|w| same(w, &words)) || kind_count >= cap(&q.kind) {
            continue;
        }
        seen.push(words);
        picked.push(q.clone());
    }
    if let Some(intro) = picked.iter().position(|q| q.kind == "intro") {
        let q = picked.remove(intro);
        picked.insert(0, q);
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(kind: &str, text: &str, asked: Vec<Asked>) -> Question {
        Question { text: text.into(), kind: kind.into(), asked, stronger_answer: None, source: "yours".into(), contributors: 0,
                   rounds: vec![], roles: vec![] }
    }

    fn asked(company: &str, score: Option<u8>, practice: bool) -> Asked {
        Asked { session_id: 1, title: "t".into(), company: Some(company.into()), role: Some("Product manager".into()),
                round: Some("Hiring manager".into()), timestamp: "00:01:00".into(), score, created_at: "2026-10-01".into(), practice }
    }

    #[test]
    fn rewordings_of_one_question_are_the_same_question() {
        let a = key_words("Tell me about a time you had to make a tough prioritization call.");
        let b = key_words("Can you walk me through a tough prioritization call you had to make?");
        let c = key_words("Tell me about a time you disagreed with your manager.");
        assert!(same(&a, &b), "{a:?} {b:?}");
        assert!(!same(&a, &c));
        assert!(!same(&key_words("Why us?"), &key_words("Why now?")), "short questions must match exactly");
    }

    #[test]
    fn practice_shows_progress_without_counting_as_asked() {
        let question = q("behavioral", "Tough call?", vec![asked("Acme", Some(4), true), asked("Acme", Some(2), false)]);
        assert_eq!(question.times(), 1);
        assert_eq!(question.latest_score(), Some(4), "your latest answer, from practice");
    }

    #[test]
    fn a_plan_puts_the_target_and_weak_spots_first_and_fills_up_with_classics() {
        let target = Target { company: Some("acme".into()), role: None, round: None };
        let candidates = vec![
            q("behavioral", "Tell me about leading a migration across three teams.", vec![asked("Globex", Some(5), false)]),
            q("technical", "How would you design the Acme billing pipeline?", vec![asked("Acme Corp", Some(4), false)]),
            q("behavioral", "Tell me about a launch that slipped and how you handled it.", vec![asked("Globex", Some(2), false)]),
            q("logistics", "When could you start?", vec![asked("Acme Corp", None, false)]),
            q("intro", "Walk me through your background.", vec![asked("Globex", Some(4), false)]),
        ];
        let plan = plan(&candidates, &target, 6);
        let texts: Vec<&str> = plan.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(texts[0], "Walk me through your background.", "an introduction opens");
        assert_eq!(texts[1], "How would you design the Acme billing pipeline?", "then the target company's");
        assert_eq!(texts[2], "Tell me about a launch that slipped and how you handled it.", "then a weak spot");
        assert!(!texts.contains(&"When could you start?"), "no logistics");
        assert_eq!(plan.len(), 6, "built-in classics fill up");
        assert!(plan.iter().any(|q| q.source == "built-in"));
        assert!(plan.iter().filter(|q| q.kind == "intro").count() <= 1, "the built-in intro isn't added twice-over");
    }

    #[test]
    fn an_empty_registry_still_gives_a_mock_interview() {
        let plan = plan(&[], &Target::default(), 5);
        assert_eq!(plan.len(), 5);
        assert_eq!(plan[0].kind, "intro");
    }
}
