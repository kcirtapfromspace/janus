//! The shared question registry (cloud/registry): contributing your interviews' questions when
//! you opt in, reading the approved questions everyone shares (for practice interviews), and the
//! maintainer's moderation, through wrangler on their own Mac.
//!
//! Before a question leaves this Mac, your coaching model rewrites it so it names no person,
//! company or product (`SCRUB_PROMPT`). A second check (`still_identifying`) then drops anything
//! that still looks like an email, a link, a phone number, or a name the review knows about.
//! Contributions carry a random id for this install, which can withdraw them.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Settings;
use crate::db::{Db, StoredAnalysis};
use crate::llm::{self, Effort, Llm, StructuredOutput};
use crate::models::Session;
use crate::pipeline;
use crate::questions::{self, Question};

/// Where the registry runs (cloud/registry; set once it's deployed). `registry_url` overrides it.
pub const DEFAULT_URL: &str = "https://janus-registry.invalid";
pub const SCRUB_PROMPT: &str = include_str!("../prompts/registry_scrub_v1.md");
/// Approved questions are re-read at most this often.
const SHARED_MAX_AGE_S: i64 = 24 * 3600;
/// The Worker's per-request limit.
const PER_REQUEST: usize = 20;

fn registry_dir(settings: &Settings) -> PathBuf {
    settings.data_dir.join("registry")
}

/// This install's random id: it can withdraw what this Mac shared, and identifies nothing else.
pub fn install_id(settings: &Settings) -> Result<String> {
    let path = registry_dir(settings).join("install-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim().to_string();
        if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(id);
        }
    }
    std::fs::create_dir_all(registry_dir(settings))?;
    let id = format!("{:032x}", rand::random::<u128>());
    let mut file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&path)?;
    file.write_all(id.as_bytes())?;
    Ok(id)
}

// --- making questions generic -------------------------------------------------------------------

/// The scrubbed questions, one per input, in order. (Doc comments are instructions to the model.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scrubbed {
    /// One entry per input question, in the same order: the question rewritten to be generic, or an
    /// empty string when it can't be.
    pub questions: Vec<String>,
}

impl StructuredOutput for Scrubbed {
    const NAME: &'static str = "scrubbed_questions";
    fn validate(&self) -> Result<(), String> {
        if self.questions.iter().any(|q| q.len() > 400) {
            return Err("a question is longer than 400 characters".into());
        }
        Ok(())
    }
}

/// Whether text still looks like it identifies someone: an email, a link, a phone number, or one
/// of `names` (the company, the interviewers) as a whole word.
pub fn still_identifying(text: &str, names: &[String]) -> bool {
    let lower = text.to_lowercase();
    if lower.contains('@') || lower.contains("http") || lower.contains("www.") || lower.contains(".com") {
        return true;
    }
    let digits = text.chars().filter(char::is_ascii_digit).count();
    if digits >= 7 {
        return true;
    }
    let words: BTreeSet<String> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    names.iter().any(|name| {
        let parts: Vec<String> = name.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3).map(String::from).collect();
        !parts.is_empty() && parts.iter().any(|p| words.contains(p))
    })
}

/// Names a review knows: the company and each interviewer ("Daniel — head of product" gives Daniel).
fn known_names(session: &Session, report: &StoredAnalysis) -> Vec<String> {
    let mut names: Vec<String> = [session.company.clone(), report.analysis.context.company.clone()].into_iter().flatten().collect();
    for person in &report.analysis.context.interviewers {
        if let Some(name) = person.split(['—', '-', ',', '(']).next() {
            names.push(name.trim().to_string());
        }
    }
    names.retain(|n| !n.is_empty());
    names
}

// --- contributing -------------------------------------------------------------------------------

/// What's sent for one question.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Contribution {
    pub text: String,
    pub kind: String,
    pub round: Option<String>,
    pub role: Option<String>,
    pub company: Option<String>,
}

fn sent_path(settings: &Settings) -> PathBuf {
    registry_dir(settings).join("sent.json")
}

fn sent(settings: &Settings) -> BTreeSet<String> {
    std::fs::read_to_string(sent_path(settings)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn post(url: &str, body: &Value) -> Result<Value> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let response = client.post(url).json(body).send().with_context(|| format!("reaching {url}"))?;
    let status = response.status();
    let value: Value = response.json().unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("the registry said {status}: {}", value["error"].as_str().unwrap_or("no reason given"));
    }
    Ok(value)
}

/// Share one interview's questions, when you've opted in: rewritten to be generic, checked again,
/// and sent once each. Practice interviews aren't shared (their questions came from the registry).
/// Returns how many were accepted.
pub fn contribute_session(settings: &Settings, db: &Db, llm: &dyn Llm, model: &str, id: i64) -> Result<usize> {
    if !settings.share_questions {
        return Ok(0);
    }
    let session = db.get_session(id)?;
    if session.practice {
        return Ok(0);
    }
    let Some(report) = pipeline::current_report(db, id)? else { return Ok(0) };
    let already = sent(settings);
    let key = |timestamp: &str| format!("{id}:{timestamp}");
    let fresh: Vec<_> = report
        .analysis
        .questions
        .iter()
        .filter(|q| q.kind.as_str() != "logistics" && !already.contains(&key(&q.timestamp)))
        .collect();
    if fresh.is_empty() {
        return Ok(0);
    }
    let numbered: Vec<String> = fresh.iter().enumerate().map(|(i, q)| format!("{}. {}", i + 1, q.question)).collect();
    let scrubbed: Scrubbed = llm::generate(llm, model, SCRUB_PROMPT, &format!("<questions>\n{}\n</questions>", numbered.join("\n")),
                                           Effort::Low, &mut |_| {})?;
    if scrubbed.questions.len() != fresh.len() {
        bail!("the questions came back {} for {}, so none were shared", scrubbed.questions.len(), fresh.len());
    }
    let names = known_names(&session, &report);
    let context = &report.analysis.context;
    let company = settings.share_company.then(|| session.company.clone().or_else(|| context.company.clone())).flatten();
    let contributions: Vec<Contribution> = fresh
        .iter()
        .zip(&scrubbed.questions)
        .filter(|(_, text)| text.trim().len() >= 12 && !still_identifying(text, &names))
        .map(|(q, text)| Contribution {
            text: text.trim().to_string(),
            kind: q.kind.as_str().to_string(),
            round: Some(context.stage.label().to_string()),
            role: context.role_title.clone(),
            company: company.clone(),
        })
        .collect();
    let install = install_id(settings)?;
    let mut accepted = 0;
    for chunk in contributions.chunks(PER_REQUEST) {
        let answer = post(&format!("{}/v1/contributions", settings.registry_url), &json!({
            "install": install, "app_version": env!("CARGO_PKG_VERSION"), "questions": chunk,
        }))?;
        accepted += answer["accepted"].as_u64().unwrap_or(0) as usize;
    }
    // Every question looked at is done with, sent or dropped as identifying.
    let mut done = already;
    done.extend(fresh.iter().map(|q| key(&q.timestamp)));
    std::fs::create_dir_all(registry_dir(settings))?;
    std::fs::write(sent_path(settings), serde_json::to_string(&done)?)?;
    Ok(accepted)
}

/// Withdraw everything this install shared (approved questions stay: they're generic, and others
/// may have shared them too). Returns how many were withdrawn.
pub fn withdraw(settings: &Settings) -> Result<usize> {
    let install = install_id(settings)?;
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let url = format!("{}/v1/contributions", settings.registry_url);
    let response = client.delete(&url).json(&json!({"install": install})).send().with_context(|| format!("reaching {url}"))?;
    let status = response.status();
    let value: Value = response.json().unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("the registry said {status}: {}", value["error"].as_str().unwrap_or("no reason given"));
    }
    let _ = std::fs::remove_file(sent_path(settings));
    Ok(value["withdrawn"].as_u64().unwrap_or(0) as usize)
}

// --- reading ------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SharedQuestion {
    text: String,
    kind: String,
    #[serde(default)]
    rounds: Vec<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    contributors: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    fetched_at: i64,
    questions: Vec<Question>,
}

fn cache_path(settings: &Settings) -> PathBuf {
    registry_dir(settings).join("shared.json")
}

fn to_question(q: SharedQuestion) -> Question {
    Question {
        text: q.text,
        kind: q.kind,
        asked: vec![],
        stronger_answer: None,
        source: "shared".into(),
        contributors: q.contributors,
        rounds: q.rounds,
        roles: q.roles,
    }
}

/// Download the approved questions and keep them for offline use.
pub fn pull(settings: &Settings, timeout: Duration) -> Result<Vec<Question>> {
    let client = reqwest::blocking::Client::builder().timeout(timeout).build()?;
    let url = format!("{}/v1/questions", settings.registry_url);
    let response = client.get(&url).send().with_context(|| format!("reaching {url}"))?;
    if !response.status().is_success() {
        bail!("the registry said {}", response.status());
    }
    let body: Value = response.json()?;
    let shared: Vec<SharedQuestion> = serde_json::from_value(body["questions"].clone()).context("the registry's answer")?;
    let questions: Vec<Question> = shared.into_iter().map(to_question).collect();
    std::fs::create_dir_all(registry_dir(settings))?;
    std::fs::write(cache_path(settings), serde_json::to_string(&Cache { fetched_at: chrono::Utc::now().timestamp(), questions: questions.clone() })?)?;
    Ok(questions)
}

/// The shared questions for planning: the copy kept here when it's recent, otherwise a quick
/// download, otherwise whatever copy there is (or none: practice works without them).
pub fn shared(settings: &Settings) -> Vec<Question> {
    let cached: Option<Cache> = std::fs::read_to_string(cache_path(settings)).ok().and_then(|t| serde_json::from_str(&t).ok());
    if let Some(cache) = &cached
        && chrono::Utc::now().timestamp() - cache.fetched_at < SHARED_MAX_AGE_S
    {
        return cache.questions.clone();
    }
    pull(settings, Duration::from_secs(4)).unwrap_or_else(|_| cached.map(|c| c.questions).unwrap_or_default())
}

/// Shared questions you haven't been asked yourself, to plan from alongside your own.
pub fn merge(own: Vec<Question>, shared: Vec<Question>) -> Vec<Question> {
    let mut all = own;
    let keys: Vec<BTreeSet<String>> = all.iter().map(|q| questions::key_words(&q.text)).collect();
    for q in shared {
        let words = questions::key_words(&q.text);
        if !keys.iter().any(|k| questions::same(k, &words)) {
            all.push(q);
        }
    }
    all
}

// --- moderating (the maintainer's Mac) ----------------------------------------------------------

/// A contribution waiting for review.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Pending {
    pub id: i64,
    pub text: String,
    pub kind: String,
    pub round: Option<String>,
    pub role: Option<String>,
    pub company: Option<String>,
}

/// A string as an SQL literal: contributions are untrusted text.
pub fn lit(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Run SQL on the registry's database through wrangler. `IC_REGISTRY_D1` replaces `--remote`
/// (e.g. `--local --config cloud/registry/wrangler.toml`, for testing).
fn d1(sql: &str) -> Result<Vec<Value>> {
    let target = std::env::var("IC_REGISTRY_D1").unwrap_or_else(|_| "--remote".into());
    let output = Command::new("wrangler")
        .args(["d1", "execute", "janus-registry", "--json", "--command", sql])
        .args(target.split_whitespace())
        .output()
        .map_err(|_| anyhow::anyhow!("This is a maintainer command: it needs wrangler, signed in to the Cloudflare account that hosts the registry."))?;
    if !output.status.success() {
        bail!(
            "This is a maintainer command: wrangler couldn't reach the registry's database (signed in to the right Cloudflare account?):\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let parsed: Value = serde_json::from_slice(&output.stdout).context("wrangler's answer")?;
    Ok(parsed[0]["results"].as_array().cloned().unwrap_or_default())
}

pub fn pending() -> Result<Vec<Pending>> {
    d1("SELECT id, text, kind, round, role, company FROM contributions WHERE status = 'pending' ORDER BY id LIMIT 500")?
        .into_iter()
        .map(|row| serde_json::from_value(row).context("a pending row"))
        .collect()
}

fn ids_sql(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

fn add_to_list(list: &str, item: &Option<String>) -> String {
    let mut items: Vec<String> = serde_json::from_str(list).unwrap_or_default();
    if let Some(item) = item
        && !items.iter().any(|i| i.eq_ignore_ascii_case(item))
    {
        items.push(item.clone());
    }
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
}

/// Publish contributions: each joins the approved question it's the same as, or becomes a new one
/// (worded `as_text`, when given). Companies are never published. Returns a line per contribution.
pub fn approve(ids: &[i64], as_text: Option<&str>) -> Result<Vec<String>> {
    if ids.is_empty() {
        bail!("Which contributions? (ic registry pending lists them)");
    }
    let rows: Vec<Pending> = d1(&format!("SELECT id, text, kind, round, role, company FROM contributions WHERE id IN ({})", ids_sql(ids)))?
        .into_iter()
        .map(|row| serde_json::from_value(row).context("a contribution"))
        .collect::<Result<_>>()?;
    let mut out = vec![];
    for row in rows {
        let text = as_text.unwrap_or(&row.text).trim().to_string();
        let approved = d1("SELECT id, text, rounds, roles FROM questions")?;
        let words = questions::key_words(&text);
        let same = approved.iter().find(|q| q["text"].as_str().is_some_and(|t| questions::same(&questions::key_words(t), &words)));
        let question_id = match same {
            Some(q) => {
                let id = q["id"].as_i64().context("a question's id")?;
                d1(&format!(
                    "UPDATE questions SET contributors = contributors + 1, rounds = {}, roles = {} WHERE id = {id}",
                    lit(&add_to_list(q["rounds"].as_str().unwrap_or("[]"), &row.round)),
                    lit(&add_to_list(q["roles"].as_str().unwrap_or("[]"), &row.role)),
                ))?;
                out.push(format!("#{}: joined approved question {id} (\"{}\")", row.id, q["text"].as_str().unwrap_or_default()));
                id
            }
            None => {
                let created = d1(&format!(
                    "INSERT INTO questions (text, kind, rounds, roles) VALUES ({}, {}, {}, {}) RETURNING id",
                    lit(&text), lit(&row.kind), lit(&add_to_list("[]", &row.round)), lit(&add_to_list("[]", &row.role)),
                ))?;
                let id = created.first().and_then(|r| r["id"].as_i64()).context("the new question's id")?;
                out.push(format!("#{}: published as question {id}", row.id));
                id
            }
        };
        d1(&format!("UPDATE contributions SET status = 'approved', question_id = {question_id} WHERE id = {}", row.id))?;
    }
    Ok(out)
}

pub fn reject(ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        bail!("Which contributions? (ic registry pending lists them)");
    }
    d1(&format!("UPDATE contributions SET status = 'rejected' WHERE id IN ({}) AND status = 'pending'", ids_sql(ids)))?;
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifying_details_are_caught_after_the_model() {
        let names = vec!["Globex".to_string(), "Daniel".to_string()];
        assert!(still_identifying("Tell me about your time at Globex.", &names));
        assert!(still_identifying("What did Daniel ask you last time?", &names));
        assert!(still_identifying("Email me at sam@example.org about it.", &names));
        assert!(still_identifying("See https://example.org for details.", &names));
        assert!(still_identifying("Call 415 555 0100 when you can.", &names));
        assert!(!still_identifying("Tell me about a migration you led.", &names));
        assert!(!still_identifying("How did you grow revenue in 2023?", &names), "a year alone isn't identifying");
        assert!(!still_identifying("What draws you to this role?", &[]), "no names known");
    }

    #[test]
    fn sql_literals_survive_quotes() {
        assert_eq!(lit("What's your take?"), "'What''s your take?'");
        assert_eq!(lit("'); DROP TABLE questions; --"), "'''); DROP TABLE questions; --'", "the quote can't close the literal");
    }

    #[test]
    fn rounds_and_roles_are_kept_once_each() {
        assert_eq!(add_to_list(r#"["Hiring manager"]"#, &Some("hiring manager".into())), r#"["Hiring manager"]"#);
        assert_eq!(add_to_list("[]", &Some("Technical".into())), r#"["Technical"]"#);
        assert_eq!(add_to_list("[]", &None), "[]");
    }

    #[test]
    fn shared_questions_you_were_asked_yourself_are_not_added_again() {
        let own = vec![questions::built_in()[2].clone()];
        let shared = vec![
            to_question(SharedQuestion { text: "Can you walk me through a tough prioritization call you had to make?".into(), kind: "behavioral".into(),
                                         rounds: vec![], roles: vec![], contributors: 4 }),
            to_question(SharedQuestion { text: "How do you handle a stakeholder who disagrees with your roadmap?".into(), kind: "situational".into(),
                                         rounds: vec!["Hiring manager".into()], roles: vec![], contributors: 2 }),
        ];
        let merged = merge(own, shared);
        assert_eq!(merged.len(), 2, "the rewording of your own question isn't added");
        assert_eq!(merged[1].source, "shared");
        assert_eq!(merged[1].contributors, 2);
    }

    #[test]
    fn the_install_id_is_made_once() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = Settings { data_dir: tmp.path().to_path_buf(), ..Settings::load().unwrap() };
        let id = install_id(&settings).unwrap();
        assert_eq!(id.len(), 32);
        assert_eq!(install_id(&settings).unwrap(), id);
    }
}
