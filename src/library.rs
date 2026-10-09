//! Managing interviews: filing them under companies and roles, archiving, Recently Deleted, and
//! search.
//!
//! - **Grouping:** a company groups its roles, and a role groups its interviews (the rounds).
//! - **Filing:** a new report files its interview under the company and role it inferred, once. After
//!   that the role is yours to change, and later reports (other models infer other titles) never
//!   re-file it.
//! - **Company:** the company you enter on an interview is never overwritten. It's an input to the
//!   report, so a model's guess there would change the next report.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::config::Settings;
use crate::db::{Db, Role};
use crate::models::{Session, Status};
use crate::pipeline;
use crate::steps::{self, StageStatus};

/// Days an interview stays in Recently Deleted before it's erased.
pub const DELETED_DAYS: i64 = 30;

/// Groups spellings of one company together: "Agility ", "agility" → "agility".
pub fn company_key(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The company an interview shows under: the one you entered, its role's, or its report's guess.
pub fn display_company(session: &Session, role: Option<&Role>, inferred: Option<&str>) -> Option<String> {
    session.company.clone()
        .or_else(|| role.and_then(|r| r.company.clone()))
        .or_else(|| inferred.map(String::from))
        .filter(|c| !c.trim().is_empty())
}

/// File an interview under the company and role its current report inferred, unless its role has
/// already been set (by an earlier filing or by you). Returns the role it was filed under.
pub fn file_from_report(db: &Db, session_id: i64) -> Result<Option<i64>> {
    match pipeline::current_report(db, session_id)? {
        Some(report) => file_from(db, session_id, &report),
        None => Ok(None),
    }
}

/// File an interview from a report just made (its stage hasn't finished, so it isn't current yet).
pub fn file_from(db: &Db, session_id: i64, report: &crate::db::StoredAnalysis) -> Result<Option<i64>> {
    let session = db.get_session(session_id)?;
    if session.role_set || session.role_id.is_some() {
        return Ok(None);
    }
    let context = &report.analysis.context;
    let Some(title) = context.role_title.as_deref().map(str::trim).filter(|t| !t.is_empty()) else { return Ok(None) };
    let company = session.company.clone().or_else(|| context.company.clone());
    let role = db.find_or_create_role(company.as_deref(), title)?;
    db.set_session_role(session_id, Some(role.id))?;
    Ok(Some(role.id))
}

/// Why an interview can't be archived or deleted right now, if it can't.
pub fn busy(db: &Db, session: &Session) -> Result<Option<String>> {
    if matches!(session.status, Status::Recording | Status::Transcribing | Status::Analyzing) {
        return Ok(Some(format!("“{}” is {} right now", session.title, session.status.as_str())));
    }
    if steps::flow(db, session.id)?.iter().any(|s| s.status == StageStatus::Running) {
        return Ok(Some(format!("a stage of “{}” is running right now", session.title)));
    }
    Ok(None)
}

fn refuse_if_busy(db: &Db, id: i64) -> Result<Session> {
    let session = db.get_session(id)?;
    if let Some(why) = busy(db, &session)? {
        bail!("Can't do that while {why}. Try again when it's finished.");
    }
    Ok(session)
}

pub fn archive(db: &Db, id: i64, archived: bool) -> Result<()> {
    refuse_if_busy(db, id)?;
    db.set_archived(id, archived)
}

/// Move to Recently Deleted (restorable for `DELETED_DAYS` days).
pub fn delete(db: &Db, id: i64) -> Result<()> {
    refuse_if_busy(db, id)?;
    db.set_deleted(id, true)
}

/// Bring an interview back from Recently Deleted, and its role if that was deleted with it.
pub fn restore(db: &Db, id: i64) -> Result<()> {
    db.set_deleted(id, false)?;
    if let Some(role) = db.get_session(id)?.role_id.map(|r| db.role(r)).transpose()?
        && role.deleted_at.is_some()
    {
        db.set_role_deleted(role.id, false)?;
    }
    Ok(())
}

/// Days left before a deleted interview is erased (0 = due).
pub fn days_left(deleted_at: &str, now: chrono::DateTime<chrono::Utc>) -> i64 {
    chrono::DateTime::parse_from_rfc3339(deleted_at)
        .map(|d| (DELETED_DAYS - (now - d.with_timezone(&chrono::Utc)).num_days()).max(0))
        .unwrap_or(0)
}

/// Whether `dir` is a session folder inside the data folder's `sessions` (and so safe to erase).
fn is_session_folder(dir: &Path, sessions_dir: &Path) -> bool {
    match (dir.canonicalize(), sessions_dir.canonicalize()) {
        (Ok(dir), Ok(root)) => dir.starts_with(&root) && dir != root,
        _ => false,
    }
}

/// Erase a deleted interview for good: its data, and its folder when that's inside the data folder.
/// The original file of an import (elsewhere on your Mac) is never touched.
pub fn erase(db: &Db, settings: &Settings, id: i64) -> Result<()> {
    // A deleted interview still opens, so a stage may have been started on it since.
    let session = refuse_if_busy(db, id)?;
    if session.deleted_at.is_none() {
        bail!("“{}” isn't in Recently Deleted. Delete it first.", session.title);
    }
    let dir = PathBuf::from(&session.dir);
    if dir.exists() && is_session_folder(&dir, &settings.sessions_dir()) {
        std::fs::remove_dir_all(&dir)?;
    }
    db.erase_session(id)?;
    // A deleted role goes for good with the last of its interviews.
    if let Some(role) = session.role_id {
        db.remove_role_if_unused(role)?;
    }
    Ok(())
}

/// Erase interviews deleted at least `older_than_days` ago (0: all of Recently Deleted). Returns their ids.
pub fn empty_deleted(db: &Db, settings: &Settings, older_than_days: i64) -> Result<Vec<i64>> {
    let now = chrono::Utc::now();
    let mut erased = vec![];
    for s in db.list_sessions()? {
        let Some(deleted_at) = &s.deleted_at else { continue };
        let age = DELETED_DAYS - days_left(deleted_at, now);
        // One that's busy waits for the next sweep.
        if age >= older_than_days && busy(db, &s)?.is_none() {
            erase(db, settings, s.id)?;
            erased.push(s.id);
        }
    }
    Ok(erased)
}

/// Move a role and its interviews to Recently Deleted. Restoring any of them brings the role back;
/// it's erased with the last of them. Returns how many interviews went with it.
pub fn delete_role(db: &Db, id: i64) -> Result<usize> {
    let role = db.role(id)?;
    let sessions: Vec<Session> =
        db.list_sessions()?.into_iter().filter(|s| s.role_id == Some(id) && s.deleted_at.is_none()).collect();
    // All or nothing: refuse before deleting any of them.
    for s in &sessions {
        if let Some(why) = busy(db, s)? {
            bail!("Can't delete {} while {why}.", role.title);
        }
    }
    for s in &sessions {
        db.set_deleted(s.id, true)?;
    }
    db.set_role_deleted(id, true)?;
    // With nothing to restore it by, it goes now.
    db.remove_role_if_unused(id)?;
    Ok(sessions.len())
}

/// Move a company to Recently Deleted: its roles (as `delete_role`) and every interview the app
/// shows under it. Returns how many interviews went.
pub fn delete_company(db: &Db, name: &str) -> Result<usize> {
    let key = company_key(name);
    let roles = db.roles()?;
    let inferred = db.latest_companies()?;
    // Where the app shows an interview: under its role's company, else its own.
    let shown_under = |s: &Session| {
        let role = s.role_id.and_then(|id| roles.iter().find(|r| r.id == id));
        role.and_then(|r| r.company.as_deref())
            .map(company_key)
            .or_else(|| display_company(s, role, inferred.get(&s.id).map(String::as_str)).map(|c| company_key(&c)))
    };
    let going: Vec<Session> = db
        .list_sessions()?
        .into_iter()
        .filter(|s| s.deleted_at.is_none() && shown_under(s).as_deref() == Some(key.as_str()))
        .collect();
    // All or nothing: refuse before deleting any of them.
    for s in &going {
        if let Some(why) = busy(db, s)? {
            bail!("Can't delete {name} while {why}.");
        }
    }
    for role in roles
        .iter()
        .filter(|r| r.deleted_at.is_none() && r.company.as_deref().map(company_key).as_deref() == Some(key.as_str()))
    {
        delete_role(db, role.id)?;
    }
    for s in &going {
        db.set_deleted(s.id, true)?;
    }
    Ok(going.len())
}

/// Archive (or bring back) a company: its roles, and its interviews that aren't under a role.
pub fn archive_company(db: &Db, name: &str, archived: bool) -> Result<usize> {
    let key = company_key(name);
    let mut n = 0;
    let roles = db.roles()?;
    for role in roles
        .iter()
        .filter(|r| r.deleted_at.is_none() && r.company.as_deref().map(company_key) == Some(key.clone()))
    {
        db.set_role_archived(role.id, archived)?;
        n += 1;
    }
    let inferred = db.latest_companies()?;
    for s in db.list_sessions()? {
        let role = s.role_id.and_then(|id| roles.iter().find(|r| r.id == id));
        if role.is_none() && display_company(&s, None, inferred.get(&s.id).map(String::as_str)).map(|c| company_key(&c)) == Some(key.clone())
        {
            if let Some(why) = busy(db, &s)? {
                bail!("Can't archive {name} while {why}.");
            }
            db.set_archived(s.id, archived)?;
            n += 1;
        }
    }
    Ok(n)
}

/// One interview, as the app lists it.
#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub created_at: String,
    pub title: String,
    /// The company it shows under, and the key that groups its spellings.
    pub company: Option<String>,
    pub company_key: Option<String>,
    /// The company you entered (the report's input), if any.
    pub entered_company: Option<String>,
    pub stage: Option<&'static str>,
    pub stage_id: Option<&'static str>,
    pub status: &'static str,
    pub mode: &'static str,
    pub duration_s: Option<f64>,
    pub dir: String,
    pub verdict: Option<&'static str>,
    pub verdict_label: Option<&'static str>,
    pub outcome: Option<&'static str>,
    pub outcome_label: Option<&'static str>,
    pub report_path: Option<String>,
    pub transcript_path: Option<String>,
    pub error: Option<String>,
    pub role_id: Option<i64>,
    /// Archived itself, or through its role.
    pub archived: bool,
    /// In Recently Deleted: days until it's erased.
    pub deleted_days_left: Option<i64>,
    /// A mock interview, for practice.
    pub practice: bool,
    /// Its place among its role's rounds (or its company's other interviews), once you've arranged them.
    pub position: Option<i64>,
}

/// A role, as the app lists it.
#[derive(Debug, Serialize)]
pub struct RoleEntry {
    pub id: i64,
    pub title: String,
    pub company: Option<String>,
    pub company_key: Option<String>,
    pub status: &'static str,
    pub status_label: &'static str,
    pub archived: bool,
    /// Its place among its company's roles, once you've arranged them.
    pub position: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Library {
    pub sessions: Vec<Entry>,
    /// Every role but those in Recently Deleted.
    pub roles: Vec<RoleEntry>,
    /// The companies you've arranged, top first (the rest go by date).
    pub company_order: Vec<CompanyPlace>,
}

/// A company's place in the sidebar, once you've arranged it.
#[derive(Debug, Serialize)]
pub struct CompanyPlace {
    pub key: String,
    pub position: i64,
}

fn existing(dir: &str, name: &str) -> Option<String> {
    let path = Path::new(dir).join(name);
    path.exists().then(|| path.display().to_string())
}

/// Every interview (archived and deleted ones flagged) and every role.
pub fn library(db: &Db) -> Result<Library> {
    let (verdicts, outcomes, inferred) = (db.latest_verdicts()?, db.all_outcomes()?, db.latest_companies()?);
    let roles = db.roles()?;
    let now = chrono::Utc::now();
    let sessions = db
        .list_sessions()?
        .into_iter()
        .map(|s| {
            let role = s.role_id.and_then(|id| roles.iter().find(|r| r.id == id));
            let company = display_company(&s, role, inferred.get(&s.id).map(String::as_str));
            Entry {
                id: s.id,
                company_key: company.as_deref().map(company_key),
                company,
                entered_company: s.company.clone(),
                stage: s.stage.map(|st| st.label()),
                stage_id: s.stage.map(|st| st.as_str()),
                status: s.status.as_str(),
                mode: s.mode.as_str(),
                duration_s: s.duration_s,
                verdict: verdicts.get(&s.id).map(|v| v.as_str()),
                verdict_label: verdicts.get(&s.id).map(|v| v.label()),
                outcome: outcomes.get(&s.id).map(|o| o.result.as_str()),
                outcome_label: outcomes.get(&s.id).map(|o| o.result.label()),
                report_path: existing(&s.dir, "report.html"),
                transcript_path: existing(&s.dir, "transcript.md"),
                role_id: s.role_id,
                archived: s.archived_at.is_some() || role.is_some_and(|r| r.archived_at.is_some()),
                deleted_days_left: s.deleted_at.as_deref().map(|d| days_left(d, now)),
                practice: s.practice,
                position: s.position,
                created_at: s.created_at,
                title: s.title,
                dir: s.dir,
                error: s.error,
            }
        })
        .collect();
    let roles = roles
        .into_iter()
        .filter(|r| r.deleted_at.is_none())
        .map(|r| RoleEntry {
            id: r.id,
            company_key: r.company.as_deref().map(company_key),
            title: r.title,
            company: r.company,
            status: r.status.as_str(),
            status_label: r.status.label(),
            archived: r.archived_at.is_some(),
            position: r.position,
        })
        .collect();
    let company_order = db.company_order()?.into_iter().map(|(key, position)| CompanyPlace { key, position }).collect();
    Ok(Library { sessions, roles, company_order })
}

/// Something that matched a search.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub session_id: i64,
    /// "title", "company", "role" or "transcript".
    pub kind: &'static str,
    /// Where in the recording, for transcript hits.
    pub at: Option<f64>,
    pub text: String,
}

/// Interviews whose title, company, role or transcript contains `query` (ignoring case).
pub fn search(db: &Db, query: &str) -> Result<Vec<Hit>> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let lib = library(db)?;
    let mut hits = vec![];
    for s in lib.sessions.iter().filter(|s| s.deleted_days_left.is_none()) {
        let role = s.role_id.and_then(|id| lib.roles.iter().find(|r| r.id == id));
        for (kind, text) in [("title", Some(&s.title)), ("company", s.company.as_ref()), ("role", role.map(|r| &r.title))] {
            if let Some(text) = text.filter(|t| t.to_lowercase().contains(&q)) {
                hits.push(Hit { session_id: s.id, kind, at: None, text: text.clone() });
            }
        }
    }
    for (session_id, at, text) in db.search_segments(&q, 200)? {
        if lib.sessions.iter().any(|s| s.id == session_id && s.deleted_days_left.is_none()) {
            hits.push(Hit { session_id, kind: "transcript", at: Some(at), text });
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_spellings_group_together() {
        assert_eq!(company_key(" Agility  Robotics "), "agility robotics");
        assert_eq!(company_key("agility robotics"), company_key("Agility Robotics"));
    }

    #[test]
    fn days_left_count_down_from_thirty() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-31T12:00:00+00:00").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(days_left("2026-10-31T11:00:00+00:00", now), 30);
        assert_eq!(days_left("2026-10-24T12:00:00+00:00", now), 23);
        assert_eq!(days_left("2026-09-01T12:00:00+00:00", now), 0);
    }

    #[test]
    fn only_folders_inside_the_sessions_folder_can_be_erased() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let inside = sessions.join("0001-x");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = root.path().join("Documents");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(is_session_folder(&inside, &sessions));
        assert!(!is_session_folder(&sessions, &sessions), "never the sessions folder itself");
        assert!(!is_session_folder(&outside, &sessions));
        assert!(!is_session_folder(&sessions.join("0001-x/../../Documents"), &sessions), "no escaping with ..");
        assert!(!is_session_folder(&root.path().join("missing"), &sessions));
    }
}
