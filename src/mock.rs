//! Mock interviews: an interviewer agent asks real questions from your registry (questions.rs)
//! out loud, listens to your answers, follows up the way an interviewer would, and closes. The app
//! records it as a two-track interview (the interviewer's voice on the call track, yours on the
//! mic), so the review scores it like any other, and the registry shows your practice next to
//! your real answers.
//!
//! The agent decides each turn with a structured answer (`InterviewerTurn`). The plan's limits
//! are kept here, not left to the model: at most `MAX_FOLLOW_UPS` per question, and only the
//! planned questions, in order.

use std::path::Path;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::llm::{self, Effort, Llm, StructuredOutput};
use crate::questions::{Question, Target};

pub const MOCK_FILE: &str = "mock.json";
pub const PROMPT: &str = include_str!("../prompts/mock_interviewer_v1.md");
pub const PROMPT_VERSION: &str = "mock-interviewer-v1";
/// Follow-ups on one question before moving on.
pub const MAX_FOLLOW_UPS: usize = 2;
/// Said when the closing is the code's decision, not the model's.
const CLOSING: &str = "That's all my questions. Thanks for your time today.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Planned {
    pub text: String,
    pub kind: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockTurn {
    /// "interviewer" or "you".
    pub who: String,
    pub text: String,
}

/// A mock interview in progress (`mock.json` in its session folder).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockState {
    pub target: Target,
    pub plan: Vec<Planned>,
    pub turns: Vec<MockTurn>,
    /// The planned question being discussed.
    pub current: usize,
    /// Follow-ups on it so far.
    pub follow_ups: usize,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    FollowUp,
    NextQuestion,
    Close,
}

/// What the interviewer does next. (Doc comments are instructions to the model.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InterviewerTurn {
    /// follow_up: one more question about the answer just given. next_question: move on to the next
    /// planned question. close: end the interview (only when no planned questions are left).
    pub action: Action,
    /// What the interviewer says, read aloud word for word: one to three plain sentences, no lists or
    /// markdown. For next_question it asks the next planned question.
    pub say: String,
}

impl StructuredOutput for InterviewerTurn {
    const NAME: &'static str = "interviewer_turn";
    fn validate(&self) -> Result<(), String> {
        let say = self.say.trim();
        if say.is_empty() {
            return Err("say is empty".into());
        }
        if say.len() > 700 {
            return Err("say is too long to speak".into());
        }
        if say.contains(['*', '#', '`']) || say.lines().any(|l| l.trim_start().starts_with("- ")) {
            return Err("say must be plain spoken text, without markdown".into());
        }
        Ok(())
    }
}

impl MockState {
    pub fn new(target: Target, plan: &[Question]) -> Result<Self> {
        if plan.is_empty() {
            bail!("A mock interview needs at least one question");
        }
        Ok(MockState {
            target,
            plan: plan.iter().map(|q| Planned { text: q.text.clone(), kind: q.kind.clone(), source: q.source.clone() }).collect(),
            turns: vec![],
            current: 0,
            follow_ups: 0,
            done: false,
        })
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MOCK_FILE);
        serde_json::from_str(&std::fs::read_to_string(&path).with_context(|| format!("{} isn't a mock interview", dir.display()))?)
            .with_context(|| format!("reading {}", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let tmp = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(&tmp, self)?;
        tmp.persist(dir.join(MOCK_FILE))?;
        Ok(())
    }

    fn say(&mut self, text: String) -> String {
        self.turns.push(MockTurn { who: "interviewer".into(), text: text.clone() });
        text
    }

    /// The interviewer's first words: a greeting, then the first question.
    pub fn opening(&mut self) -> String {
        let about = match (&self.target.role, &self.target.company) {
            (Some(role), Some(company)) => format!(" This is a practice conversation for the {role} role at {company}."),
            (Some(role), None) => format!(" This is a practice conversation for the {role} role."),
            (None, Some(company)) => format!(" This is a practice conversation for {company}."),
            (None, None) => String::new(),
        };
        let first = self.plan[0].text.clone();
        self.say(format!("Hi, thanks for making the time today.{about} Let's get started. {first}"))
    }

    /// What to say when the interviewer starts: the opening the first time; after a restart, the
    /// last thing it said, so the conversation picks up where it was.
    pub fn start(&mut self) -> String {
        match self.turns.iter().rev().find(|t| t.who == "interviewer") {
            Some(last) => last.text.clone(),
            None => self.opening(),
        }
    }

    pub fn answer(&mut self, text: &str) {
        self.turns.push(MockTurn { who: "you".into(), text: text.trim().to_string() });
    }

    /// What the interviewer may do now.
    pub fn allowed(&self) -> Vec<Action> {
        let mut out = vec![];
        if self.follow_ups < MAX_FOLLOW_UPS {
            out.push(Action::FollowUp);
        }
        if self.current + 1 < self.plan.len() {
            out.push(Action::NextQuestion);
        } else {
            out.push(Action::Close);
        }
        out
    }

    /// The agent's input: the plan with where we are, the conversation, and what's allowed.
    pub fn user_message(&self) -> String {
        let plan: Vec<String> = self
            .plan
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let status = if i < self.current { "asked" } else if i == self.current { "being discussed" } else { "upcoming" };
                format!("{}. [{status}] {}", i + 1, q.text)
            })
            .collect();
        let conversation: Vec<String> = self
            .turns
            .iter()
            .map(|t| format!("{}: {}", if t.who == "you" { "Candidate" } else { "Interviewer" }, t.text))
            .collect();
        let allowed: Vec<&str> = self
            .allowed()
            .iter()
            .map(|a| match a {
                Action::FollowUp => "follow_up",
                Action::NextQuestion => "next_question",
                Action::Close => "close",
            })
            .collect();
        let target = [self.target.role.as_deref(), self.target.company.as_deref(), self.target.round.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "<interview>{}</interview>\n\n<planned_questions>\n{}\n</planned_questions>\n\n<conversation>\n{}\n</conversation>\n\n\
             <allowed_actions>{}</allowed_actions>{}",
            if target.is_empty() { "General practice".to_string() } else { target },
            plan.join("\n"),
            conversation.join("\n\n"),
            allowed.join(", "),
            match self.plan.get(self.current + 1) {
                Some(next) => format!("\n\n<next_planned_question>{}</next_planned_question>", next.text),
                None => String::new(),
            }
        )
    }

    /// Take the agent's turn, keeping to the plan: a follow-up past the limit moves on, moving on
    /// past the last question closes, and closing early asks the next question instead. Returns
    /// what to say and whether the interview is over.
    pub fn apply(&mut self, turn: InterviewerTurn) -> (String, bool) {
        let allowed = self.allowed();
        let last = self.current + 1 >= self.plan.len();
        match turn.action {
            Action::FollowUp if allowed.contains(&Action::FollowUp) => {
                self.follow_ups += 1;
                (self.say(turn.say), false)
            }
            Action::NextQuestion if !last => {
                self.current += 1;
                self.follow_ups = 0;
                (self.say(turn.say), false)
            }
            Action::Close if last => {
                self.done = true;
                (self.say(turn.say), true)
            }
            // Off the plan: the code decides what comes next, in its own words.
            _ if last => {
                self.done = true;
                (self.say(CLOSING.into()), true)
            }
            _ => {
                self.current += 1;
                self.follow_ups = 0;
                let next = self.plan[self.current].text.clone();
                (self.say(format!("Thanks. {next}")), false)
            }
        }
    }

    /// When the answer couldn't be heard: ask again, without moving on.
    pub fn repeat_please(&mut self) -> String {
        self.say("Sorry, I didn't catch that. Could you say that again?".into())
    }
}

/// The interviewer's next turn after your latest answer.
pub fn next(llm: &dyn Llm, model: &str, state: &mut MockState) -> Result<(String, bool)> {
    let turn: InterviewerTurn = llm::generate(llm, model, PROMPT, &state.user_message(), Effort::Low, &mut |_| {})?;
    Ok(state.apply(turn))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::built_in;

    fn state(n: usize) -> MockState {
        let target = Target { company: Some("Acme".into()), role: Some("Product manager".into()), round: None };
        MockState::new(target, &built_in()[..n]).unwrap()
    }

    fn turn(action: Action, say: &str) -> InterviewerTurn {
        InterviewerTurn { action, say: say.into() }
    }

    #[test]
    fn it_opens_with_a_greeting_and_the_first_question() {
        let mut s = state(3);
        let opening = s.opening();
        assert!(opening.starts_with("Hi, thanks for making the time today. This is a practice conversation for the Product manager role at Acme."));
        assert!(opening.ends_with(&s.plan[0].text));
        assert_eq!(s.turns.len(), 1);
    }

    #[test]
    fn a_restart_picks_up_where_it_was() {
        let mut s = state(3);
        let opening = s.start();
        assert_eq!(s.start(), opening, "said again, not added again");
        assert_eq!(s.turns.len(), 1);
        s.answer("I lead product.");
        s.apply(turn(Action::NextQuestion, "Thanks. Why this role?"));
        assert_eq!(s.start(), "Thanks. Why this role?");
        assert_eq!(s.turns.len(), 3);
    }

    #[test]
    fn follow_ups_are_limited_and_the_plan_is_kept() {
        let mut s = state(2);
        s.opening();
        s.answer("I lead product at a logistics company.");
        assert_eq!(s.apply(turn(Action::FollowUp, "What does leading mean there?")), ("What does leading mean there?".into(), false));
        assert!(!s.apply(turn(Action::FollowUp, "And before that?")).1);
        assert_eq!(s.allowed(), [Action::NextQuestion], "two follow-ups: time to move on");
        let (say, done) = s.apply(turn(Action::FollowUp, "One more thing?"));
        assert_eq!(say, format!("Thanks. {}", s.plan[1].text), "a third follow-up becomes the next question");
        assert!(!done);
        assert_eq!((s.current, s.follow_ups), (1, 0));
        let (say, done) = s.apply(turn(Action::NextQuestion, "Next?"));
        assert_eq!((say.as_str(), done), (CLOSING, true), "past the last question, it closes");
        assert!(s.done);
    }

    #[test]
    fn closing_early_asks_the_next_question_instead() {
        let mut s = state(3);
        s.opening();
        let (say, done) = s.apply(turn(Action::Close, "Thanks, bye!"));
        assert!(!done);
        assert_eq!(say, format!("Thanks. {}", s.plan[1].text));
        s.current = 2;
        assert_eq!(s.apply(turn(Action::Close, "That's all my questions, thank you.")), ("That's all my questions, thank you.".into(), true));
    }

    #[test]
    fn the_agent_sees_the_plan_where_it_is_and_what_it_may_do() {
        let mut s = state(3);
        s.opening();
        s.answer("I lead product.");
        let message = s.user_message();
        assert!(message.contains("1. [being discussed]"));
        assert!(message.contains("2. [upcoming]"));
        assert!(message.contains("Candidate: I lead product."));
        assert!(message.contains("<allowed_actions>follow_up, next_question</allowed_actions>"));
        assert!(message.contains(&format!("<next_planned_question>{}</next_planned_question>", s.plan[1].text)));
        assert!(message.contains("<interview>Product manager, Acme</interview>"));
    }

    #[test]
    fn spoken_turns_must_be_plain_text() {
        assert!(turn(Action::FollowUp, "What was your part in it?").validate().is_ok());
        assert!(turn(Action::FollowUp, "**Great!** Next:").validate().is_err());
        assert!(turn(Action::FollowUp, "Two things:\n- one\n- two").validate().is_err());
        assert!(turn(Action::FollowUp, "  ").validate().is_err());
    }

    #[test]
    fn state_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = state(2);
        s.opening();
        s.save(tmp.path()).unwrap();
        assert_eq!(MockState::load(tmp.path()).unwrap(), s);
    }

    /// The real agent, through the fake model the tests use elsewhere.
    #[test]
    fn next_asks_the_model_and_keeps_its_answer_in_bounds() {
        struct Fixed(&'static str);
        impl Llm for Fixed {
            fn structured(&self, _: &llm::StructuredRequest, _: &mut dyn FnMut(usize)) -> Result<String, llm::LlmError> {
                Ok(self.0.into())
            }
        }
        let mut s = state(2);
        s.opening();
        s.answer("Um.");
        let (say, done) = next(&Fixed(r#"{"action":"follow_up","say":"Could you tell me a bit more?"}"#), "m", &mut s).unwrap();
        assert_eq!((say.as_str(), done), ("Could you tell me a bit more?", false));
        assert!(next(&Fixed(r#"{"action":"follow_up","say":"**Nice**"}"#), "m", &mut s).is_err(), "markdown is refused, then retried by generate");
    }
}
