//! SQLite index. Audio and exports live in per-session folders; the database ties them together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::metrics::TalkMetrics;
use crate::models::{
    Mode, NextSteps, OutcomeResult, RoleStatus, RunStatus, Segment, Session, SessionAnalysis,
    Source, Stage, Status, Step, Verdict, Word,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS roles (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    title TEXT NOT NULL,
    level TEXT,
    company TEXT,
    jd_text TEXT,
    profile_json TEXT,
    -- Version 7: where the application stands, and whether it's archived.
    status TEXT NOT NULL DEFAULT 'interviewing',
    archived_at TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    title TEXT NOT NULL,
    company TEXT,
    stage TEXT,
    role_id INTEGER REFERENCES roles(id) ON DELETE SET NULL,
    source TEXT NOT NULL CHECK (source IN ('upload', 'recording')),
    mode TEXT NOT NULL CHECK (mode IN ('single', 'dual')),
    source_path TEXT,
    dir TEXT NOT NULL,
    duration_s REAL,
    num_speakers INTEGER,
    consent INTEGER,
    status TEXT NOT NULL,
    error TEXT,
    -- Version 7: archived, in Recently Deleted, and whether its role was set (by filing or by you).
    archived_at TEXT,
    deleted_at TEXT,
    role_set INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS segments (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    start_s REAL NOT NULL,
    end_s REAL NOT NULL,
    speaker TEXT NOT NULL,
    text TEXT NOT NULL,
    words_json TEXT
);
CREATE INDEX IF NOT EXISTS segments_by_session ON segments(session_id, idx);

-- Every analysis run is kept; the newest one per session is the current one.
CREATE TABLE IF NOT EXISTS analyses (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    verdict TEXT NOT NULL,
    analysis_json TEXT NOT NULL,
    metrics_json TEXT NOT NULL,
    unverified_quotes_json TEXT NOT NULL,
    -- Version 6: what the report was built from (versions.rs), so an unchanged rerun reuses it.
    inputs_key TEXT,
    inputs_json TEXT,
    parent_id INTEGER
);
CREATE INDEX IF NOT EXISTS analyses_by_session ON analyses(session_id, id);

-- What actually happened, entered by you once you hear back.
CREATE TABLE IF NOT EXISTS outcomes (
    session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    result TEXT NOT NULL,
    notes TEXT,
    updated_at TEXT NOT NULL
);

-- One row per attempt at a stage (recording, transcript, report, next). input_run_id is the upstream
-- run it was built from, which is how a stage knows it's out of date; output_id points at the
-- analyses / next_steps row it produced. progress + message are live status for the app.
CREATE TABLE IF NOT EXISTS step_runs (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    step TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    params_json TEXT NOT NULL DEFAULT '{}',
    input_run_id INTEGER,
    output_id INTEGER,
    error TEXT,
    progress REAL,
    message TEXT,
    pid INTEGER,
    warnings_json TEXT
);
CREATE INDEX IF NOT EXISTS step_runs_by_session ON step_runs(session_id, id);

-- "What to do next": every run is kept, like analyses.
CREATE TABLE IF NOT EXISTS next_steps (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    analysis_id INTEGER,
    plan_json TEXT NOT NULL,
    unverified_quotes_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS next_steps_by_session ON next_steps(session_id, id);

-- One row per check per answer in an analysed interview (see scoring.rs), e.g. whether answer 3
-- led with its point. Kept per analysis run, with the scorer that judged it.
CREATE TABLE IF NOT EXISTS answer_checks (
    id INTEGER PRIMARY KEY,
    analysis_id INTEGER NOT NULL REFERENCES analyses(id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    answer_idx INTEGER NOT NULL,
    answer_start REAL NOT NULL,
    question TEXT NOT NULL,
    check_id TEXT NOT NULL,
    scorer TEXT NOT NULL,
    pick TEXT NOT NULL,
    value REAL NOT NULL,
    confidence REAL,
    verdict TEXT NOT NULL CHECK (verdict IN ('pass', 'fail', 'unclear'))
);
CREATE INDEX IF NOT EXISTS answer_checks_by_analysis ON answer_checks(analysis_id, answer_idx);

-- The room's temperature timeline (temperature.rs): one row per conversation turn in an analysed
-- interview: substantive interviewer turns, their backchannels, and your answers. Jev's verdicts go in
-- checks_json; voice features, z-scores, listening cues, video cues (answers, when the call's video
-- was recorded) and the smoothed line in features_json.
CREATE TABLE IF NOT EXISTS turn_signals (
    id INTEGER PRIMARY KEY,
    analysis_id INTEGER NOT NULL REFERENCES analyses(id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_idx INTEGER NOT NULL,
    speaker TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('substantive', 'backchannel', 'answer')),
    start_s REAL NOT NULL,
    end_s REAL NOT NULL,
    excerpt TEXT NOT NULL,
    temperature REAL,
    scorer TEXT,
    method TEXT NOT NULL,
    checks_json TEXT NOT NULL,
    features_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS turn_signals_by_analysis ON turn_signals(analysis_id, turn_idx);

-- Every transcript a report could be built on, kept when a newer one (e.g. a speaker swap)
-- replaces the live segments, so each report version's exact transcript stays known.
CREATE TABLE IF NOT EXISTS transcript_revisions (
    run_id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL,
    created_at TEXT NOT NULL,
    segments_json TEXT NOT NULL
);

-- Scorer verdicts (Jev, or Claude checks) stored under their exact inputs, so the same turn or
-- answer always gets the same verdict, and reruns are free.
CREATE TABLE IF NOT EXISTS judgments (
    key TEXT PRIMARY KEY,
    scorer TEXT NOT NULL,
    created_at TEXT NOT NULL,
    assessment_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS coaching_plans (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    role_id INTEGER REFERENCES roles(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    session_ids_json TEXT NOT NULL,
    plan_json TEXT NOT NULL
);
"#;

pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

/// Fields for a new session; everything else starts empty.
pub struct NewSession {
    pub title: String,
    pub company: Option<String>,
    pub source: Source,
    pub mode: Mode,
    pub source_path: Option<String>,
    pub num_speakers: Option<i64>,
    pub consent: Option<bool>,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredAnalysis {
    pub id: i64,
    pub session_id: i64,
    pub created_at: String,
    pub model: String,
    pub prompt_version: String,
    pub analysis: SessionAnalysis,
    pub metrics: TalkMetrics,
    pub unverified_quotes: Vec<String>,
    /// Per-answer checks for this run (empty when no scorer was available).
    pub answer_checks: Vec<AnswerCheck>,
    /// The temperature timeline for this run (empty for runs before it existed).
    pub turn_signals: Vec<crate::temperature::Signal>,
    /// Which scorer judged the timeline's turns, e.g. `typesafe/jev-1.13.0` (None: voice only).
    pub timeline_scorer: Option<String>,
    /// What the report was built from (None for reports made before versions were recorded).
    pub inputs: Option<crate::versions::Manifest>,
    /// The version this one was re-run from.
    pub parent_id: Option<i64>,
}

/// One check of one answer (see scoring.rs).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnswerCheck {
    pub answer_idx: i64,
    /// Seconds into the recording where the answer's question was asked.
    pub answer_start: f64,
    pub question: String,
    pub check_id: String,
    /// The model that judged it, e.g. `typesafe/jev-1.13.0`.
    pub scorer: String,
    pub pick: String,
    pub value: f64,
    pub confidence: Option<f64>,
    /// pass, fail, or unclear.
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredNextSteps {
    pub id: i64,
    pub session_id: i64,
    pub created_at: String,
    pub model: String,
    pub prompt_version: String,
    pub analysis_id: Option<i64>,
    pub plan: NextSteps,
    pub unverified_quotes: Vec<String>,
}

/// A next-steps run's id, when it was made, its model, and the report it was planned from.
pub type NextStepsEntry = (i64, String, String, Option<i64>);

/// One attempt at one stage of one session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepRun {
    pub id: i64,
    pub session_id: i64,
    pub step: Step,
    pub status: RunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub params: serde_json::Value,
    pub input_run_id: Option<i64>,
    pub output_id: Option<i64>,
    pub error: Option<String>,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub pid: Option<i64>,
    /// Problems worth showing with a successful run (e.g. "found 1 voice but 2 were expected").
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub session_id: i64,
    pub result: OutcomeResult,
    pub notes: Option<String>,
    pub updated_at: String,
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
}

/// Schema version; `migrate` brings older databases up to it.
const SCHEMA_VERSION: i64 = 7;

fn parse<T: std::str::FromStr<Err = String>>(s: String) -> rusqlite::Result<T> {
    s.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })
}

/// A role you're interviewing for, at a company: the interviews for it are its rounds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Role {
    pub id: i64,
    pub created_at: String,
    pub title: String,
    pub company: Option<String>,
    pub status: RoleStatus,
    pub archived_at: Option<String>,
}

fn role_from_row(r: &Row) -> rusqlite::Result<Role> {
    Ok(Role {
        id: r.get("id")?,
        created_at: r.get("created_at")?,
        title: r.get("title")?,
        company: r.get("company")?,
        status: parse(r.get("status")?)?,
        archived_at: r.get("archived_at")?,
    })
}

fn session_from_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get("id")?,
        created_at: r.get("created_at")?,
        title: r.get("title")?,
        company: r.get("company")?,
        stage: r
            .get::<_, Option<String>>("stage")?
            .map(parse::<Stage>)
            .transpose()?,
        role_id: r.get("role_id")?,
        source: parse(r.get("source")?)?,
        mode: parse(r.get("mode")?)?,
        source_path: r.get("source_path")?,
        dir: r.get("dir")?,
        duration_s: r.get("duration_s")?,
        num_speakers: r.get("num_speakers")?,
        consent: r.get("consent")?,
        status: parse(r.get("status")?)?,
        error: r.get("error")?,
        archived_at: r.get("archived_at")?,
        deleted_at: r.get("deleted_at")?,
        role_set: r.get("role_set")?,
    })
}

/// id, session_id, created_at, model, prompt_version, then the three JSON columns.
type AnalysisRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
);

fn run_from_row(r: &Row) -> rusqlite::Result<StepRun> {
    let params: String = r.get("params_json")?;
    Ok(StepRun {
        id: r.get("id")?,
        session_id: r.get("session_id")?,
        step: parse(r.get("step")?)?,
        status: parse(r.get("status")?)?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        params: serde_json::from_str(&params).unwrap_or_default(),
        input_run_id: r.get("input_run_id")?,
        output_id: r.get("output_id")?,
        error: r.get("error")?,
        progress: r.get("progress")?,
        message: r.get("message")?,
        pid: r.get("pid")?,
        warnings: r
            .get::<_, Option<String>>("warnings_json")?
            .and_then(|w| serde_json::from_str(&w).ok())
            .unwrap_or_default(),
    })
}

fn analysis_from_row(r: &Row) -> rusqlite::Result<AnalysisRow> {
    Ok((
        r.get("id")?,
        r.get("session_id")?,
        r.get("created_at")?,
        r.get("model")?,
        r.get("prompt_version")?,
        r.get("analysis_json")?,
        r.get("metrics_json")?,
        r.get("unverified_quotes_json")?,
        r.get("inputs_json")?,
        r.get("parent_id")?,
    ))
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // WAL + a busy timeout: the app reads (polling progress) while ic writes.
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        let db = Db {
            conn,
            path: path.to_path_buf(),
        };
        db.migrate()?;
        db.ensure_indexes()?;
        Ok(db)
    }

    /// Another connection to the same database (for writing progress while a stage runs).
    pub fn reopen(&self) -> Result<Db> {
        Db::open(&self.path)
    }

    /// Version 2 added stage runs: sessions created before it get runs synthesized from what
    /// they already have, once, so their stages show correctly. Version 3 added run warnings,
    /// version 4 answer checks and version 5 the temperature timeline (new tables, from SCHEMA).
    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        // A v2 database's step_runs predates the column; a new or pre-v2 one just got it from SCHEMA.
        let has_warnings: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('step_runs') WHERE name = 'warnings_json'",
            [],
            |r| r.get(0),
        )?;
        if !has_warnings {
            self.conn
                .execute_batch("ALTER TABLE step_runs ADD COLUMN warnings_json TEXT;")?;
        }
        if version < 2 {
            for session in self.list_sessions()? {
                crate::steps::backfill(self, &session)?;
            }
        }
        // Version 7: archive, Recently Deleted, filing, and where each application stands.
        for (table, column) in [
            ("sessions", "archived_at TEXT"),
            ("sessions", "deleted_at TEXT"),
            ("sessions", "role_set INTEGER NOT NULL DEFAULT 0"),
            ("roles", "status TEXT NOT NULL DEFAULT 'interviewing'"),
            ("roles", "archived_at TEXT"),
        ] {
            let name = column.split(' ').next().expect("named");
            let has: bool = self.conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                [name],
                |r| r.get(0),
            )?;
            if !has {
                self.conn
                    .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column};"))?;
            }
        }
        // Version 6: report versions record their inputs; transcripts are kept per revision.
        for column in ["inputs_key TEXT", "inputs_json TEXT", "parent_id INTEGER"] {
            let name = column.split(' ').next().expect("named");
            let has: bool = self.conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('analyses') WHERE name = ?1",
                [name],
                |r| r.get(0),
            )?;
            if !has {
                self.conn
                    .execute_batch(&format!("ALTER TABLE analyses ADD COLUMN {column};"))?;
            }
        }
        if version < 6 {
            // The live transcript is the current transcript run's; earlier ones weren't kept.
            for session in self.list_sessions()? {
                let current = self
                    .runs(session.id)?
                    .into_iter()
                    .rfind(|r| r.step == Step::Transcript && r.status == RunStatus::Succeeded);
                let segments = self.get_segments(session.id)?;
                if let (Some(run), false) = (current, segments.is_empty()) {
                    self.save_transcript_revision(run.id, session.id, &segments)?;
                }
            }
        }
        if version < 7 {
            // File each analysed interview under the company and role its current report inferred.
            for session in self.list_sessions()? {
                crate::library::file_from_report(self, session.id)?;
            }
        }
        self.conn
            .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        Ok(())
    }

    fn ensure_indexes(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS analyses_by_inputs ON analyses(session_id, inputs_key);",
        )?;
        Ok(())
    }

    // --- sessions ---------------------------------------------------------------------------

    pub fn create_session(&self, new: NewSession) -> Result<Session> {
        self.conn.execute(
            "INSERT INTO sessions (created_at, title, company, source, mode, source_path, dir, num_speakers, consent, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?8, ?9)",
            params![now_iso(), new.title, new.company, new.source.as_str(), new.mode.as_str(), new.source_path,
                    new.num_speakers, new.consent, new.status.as_str()],
        )?;
        self.get_session(self.conn.last_insert_rowid())
    }

    pub fn get_session(&self, id: i64) -> Result<Session> {
        self.conn
            .query_row(
                "SELECT * FROM sessions WHERE id = ?1",
                [id],
                session_from_row,
            )
            .optional()?
            .ok_or_else(|| anyhow!("No session with id {id}. See: ic list"))
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions ORDER BY created_at DESC, id DESC")?;
        let rows = stmt.query_map([], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Write every mutable field of `s` back to the database.
    /// Saves the pipeline's own fields. The title, company, role, round, archive and delete state
    /// are yours, changed only through their setters, so a long-running stage holding an older copy
    /// never overwrites an edit made meanwhile.
    pub fn save_session(&self, s: &Session) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET source_path = ?2, dir = ?3, duration_s = ?4, num_speakers = ?5, consent = ?6,
             status = ?7, error = ?8 WHERE id = ?1",
            params![s.id, s.source_path, s.dir, s.duration_s, s.num_speakers, s.consent, s.status.as_str(), s.error],
        )?;
        Ok(())
    }

    pub fn set_title(&self, id: i64, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(())
    }

    pub fn set_company(&self, id: i64, company: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET company = ?2 WHERE id = ?1",
            params![id, company],
        )?;
        Ok(())
    }

    pub fn set_stage(&self, id: i64, stage: Option<Stage>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET stage = ?2 WHERE id = ?1",
            params![id, stage.map(|s| s.as_str())],
        )?;
        Ok(())
    }

    /// The round a report detected, kept only when none is set (yours, or an earlier report's).
    pub fn set_stage_if_unset(&self, id: i64, stage: Stage) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET stage = ?2 WHERE id = ?1 AND stage IS NULL",
            params![id, stage.as_str()],
        )?;
        Ok(())
    }

    /// File an interview under a role (or none). Either way it's settled: reports won't re-file it.
    pub fn set_session_role(&self, id: i64, role_id: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET role_id = ?2, role_set = 1 WHERE id = ?1",
            params![id, role_id],
        )?;
        Ok(())
    }

    pub fn set_archived(&self, id: i64, archived: bool) -> Result<()> {
        self.conn.execute("UPDATE sessions SET archived_at = CASE WHEN ?2 THEN coalesce(archived_at, ?3) END WHERE id = ?1",
                          params![id, archived, now_iso()])?;
        Ok(())
    }

    pub fn set_deleted(&self, id: i64, deleted: bool) -> Result<()> {
        self.conn.execute("UPDATE sessions SET deleted_at = CASE WHEN ?2 THEN coalesce(deleted_at, ?3) END WHERE id = ?1",
                          params![id, deleted, now_iso()])?;
        Ok(())
    }

    /// Remove an interview and everything stored with it (its folder is the caller's to remove).
    pub fn erase_session(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    /// (session id, start, text) of transcript lines containing `query` (lowercase), at most `limit`.
    pub fn search_segments(&self, query: &str, limit: i64) -> Result<Vec<(i64, f64, String)>> {
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let mut stmt = self.conn.prepare(
            "SELECT session_id, start_s, text FROM segments WHERE lower(text) LIKE '%' || ?1 || '%' ESCAPE '\\'
             ORDER BY session_id, idx LIMIT ?2")?;
        let rows = stmt.query_map(params![escaped, limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- roles ---------------------------------------------------------------------------------

    pub fn roles(&self) -> Result<Vec<Role>> {
        let mut stmt = self.conn.prepare("SELECT * FROM roles ORDER BY id")?;
        let rows = stmt.query_map([], role_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn role(&self, id: i64) -> Result<Role> {
        self.conn
            .query_row("SELECT * FROM roles WHERE id = ?1", [id], role_from_row)
            .optional()?
            .ok_or_else(|| anyhow!("No role with id {id}. See: ic role list"))
    }

    /// The role with this title at this company (ignoring case and spacing), created if it's new.
    pub fn find_or_create_role(&self, company: Option<&str>, title: &str) -> Result<Role> {
        let key = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let company = company.map(str::trim).filter(|c| !c.is_empty());
        if let Some(found) = self.roles()?.into_iter().find(|r| {
            key(&r.title) == key(title) && r.company.as_deref().map(key) == company.map(key)
        }) {
            return Ok(found);
        }
        self.conn.execute(
            "INSERT INTO roles (created_at, title, company) VALUES (?1, ?2, ?3)",
            params![now_iso(), title.trim(), company],
        )?;
        self.role(self.conn.last_insert_rowid())
    }

    pub fn set_role_status(&self, id: i64, status: RoleStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE roles SET status = ?2 WHERE id = ?1",
            params![id, status.as_str()],
        )?;
        Ok(())
    }

    pub fn rename_role(&self, id: i64, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE roles SET title = ?2 WHERE id = ?1",
            params![id, title.trim()],
        )?;
        Ok(())
    }

    pub fn set_role_archived(&self, id: i64, archived: bool) -> Result<()> {
        self.conn.execute("UPDATE roles SET archived_at = CASE WHEN ?2 THEN coalesce(archived_at, ?3) END WHERE id = ?1",
                          params![id, archived, now_iso()])?;
        Ok(())
    }

    /// Move every interview of one role to another, then remove the empty role.
    pub fn merge_role(&self, from: i64, into: i64) -> Result<()> {
        if from == into {
            return Ok(());
        }
        self.role(into)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE sessions SET role_id = ?2, role_set = 1 WHERE role_id = ?1",
            params![from, into],
        )?;
        tx.execute("DELETE FROM roles WHERE id = ?1", [from])?;
        tx.commit()?;
        Ok(())
    }

    /// Rename a company everywhere you entered it: its roles and its interviews.
    pub fn rename_company(&self, old: &str, new: &str) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let same = "lower(trim(company)) = lower(trim(?1))";
        let n = tx.execute(
            &format!("UPDATE roles SET company = ?2 WHERE {same}"),
            params![old, new.trim()],
        )? + tx.execute(
            &format!("UPDATE sessions SET company = ?2 WHERE {same}"),
            params![old, new.trim()],
        )?;
        tx.commit()?;
        Ok(n)
    }

    pub fn set_status(&self, id: i64, status: Status, error: Option<String>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?2, error = ?3 WHERE id = ?1",
            params![id, status.as_str(), error],
        )?;
        Ok(())
    }

    // --- segments ---------------------------------------------------------------------------

    pub fn replace_segments(&mut self, session_id: i64, segments: &[Segment]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM segments WHERE session_id = ?1", [session_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO segments (session_id, idx, start_s, end_s, speaker, text, words_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, s) in segments.iter().enumerate() {
                let words = (!s.words.is_empty())
                    .then(|| serde_json::to_string(&s.words))
                    .transpose()?;
                stmt.execute(params![
                    session_id, i as i64, s.start, s.end, s.speaker, s.text, words
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_segments(&self, session_id: i64) -> Result<Vec<Segment>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM segments WHERE session_id = ?1 ORDER BY idx")?;
        let rows = stmt.query_map([session_id], |r| {
            Ok((
                r.get("start_s")?,
                r.get("end_s")?,
                r.get("speaker")?,
                r.get("text")?,
                r.get::<_, Option<String>>("words_json")?,
            ))
        })?;
        rows.map(|row| {
            let (start, end, speaker, text, words): (f64, f64, String, String, Option<String>) =
                row?;
            let words: Vec<Word> = words
                .map(|w| serde_json::from_str(&w))
                .transpose()?
                .unwrap_or_default();
            Ok(Segment {
                start,
                end,
                speaker,
                text,
                words,
            })
        })
        .collect()
    }

    // --- analyses ---------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_analysis(
        &self,
        session_id: i64,
        analysis: &SessionAnalysis,
        metrics: &TalkMetrics,
        model: &str,
        prompt_version: &str,
        unverified_quotes: &[String],
        inputs: Option<&crate::versions::Manifest>,
        parent_id: Option<i64>,
    ) -> Result<StoredAnalysis> {
        self.conn.execute(
            "INSERT INTO analyses (session_id, created_at, model, prompt_version, verdict, analysis_json, metrics_json,
             unverified_quotes_json, inputs_key, inputs_json, parent_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![session_id, now_iso(), model, prompt_version, analysis.outlook.verdict.as_str(),
                    serde_json::to_string(analysis)?, serde_json::to_string(metrics)?,
                    serde_json::to_string(unverified_quotes)?, inputs.map(|m| m.key.clone()),
                    inputs.map(serde_json::to_string).transpose()?, parent_id],
        )?;
        let id = self.conn.last_insert_rowid();
        self.analysis_where("id = ?1", id)?
            .ok_or_else(|| anyhow!("analysis {id} vanished"))
    }

    pub fn set_analysis_inputs(&self, id: i64, inputs: &crate::versions::Manifest) -> Result<()> {
        self.conn.execute(
            "UPDATE analyses SET inputs_key = ?2, inputs_json = ?3 WHERE id = ?1",
            params![id, inputs.key, serde_json::to_string(inputs)?],
        )?;
        Ok(())
    }

    /// This session's report built from exactly these inputs, if there is one.
    pub fn analysis_by_key(&self, session_id: i64, key: &str) -> Result<Option<StoredAnalysis>> {
        let id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM analyses WHERE session_id = ?1 AND inputs_key = ?2 ORDER BY id LIMIT 1",
                       params![session_id, key], |r| r.get(0))
            .optional()?;
        id.map_or(Ok(None), |id| self.analysis_by_id(id))
    }

    /// Record the transcript a run produced (a no-op if it's already recorded).
    pub fn save_transcript_revision(
        &self,
        run_id: i64,
        session_id: i64,
        segments: &[Segment],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO transcript_revisions (run_id, session_id, sha256, created_at, segments_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, session_id, crate::versions::transcript_sha(segments), now_iso(), serde_json::to_string(segments)?],
        )?;
        Ok(())
    }

    /// (transcript run id, sha256) for every kept transcript of a session, oldest first.
    pub fn transcript_revisions(&self, session_id: i64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, sha256 FROM transcript_revisions WHERE session_id = ?1 ORDER BY run_id",
        )?;
        let rows = stmt.query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn judgment(&self, key: &str) -> Result<Option<crate::scoring::Assessment>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT assessment_json FROM judgments WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    pub fn save_judgment(
        &self,
        key: &str,
        scorer: &str,
        assessment: &crate::scoring::Assessment,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO judgments (key, scorer, created_at, assessment_json) VALUES (?1, ?2, ?3, ?4)",
            params![key, scorer, now_iso(), serde_json::to_string(assessment)?],
        )?;
        Ok(())
    }

    pub fn latest_analysis(&self, session_id: i64) -> Result<Option<StoredAnalysis>> {
        self.analysis_where("session_id = ?1 ORDER BY id DESC LIMIT 1", session_id)
    }

    fn analysis_where(&self, clause: &str, arg: i64) -> Result<Option<StoredAnalysis>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT * FROM analyses WHERE {clause}"),
                [arg],
                analysis_from_row,
            )
            .optional()?;
        let Some((
            id,
            session_id,
            created_at,
            model,
            prompt_version,
            analysis,
            metrics,
            unverified,
            inputs,
            parent_id,
        )) = row
        else {
            return Ok(None);
        };
        let (turn_signals, timeline_scorer) = self.turn_signals(id)?;
        Ok(Some(StoredAnalysis {
            id,
            session_id,
            created_at,
            model,
            prompt_version,
            analysis: serde_json::from_str(&analysis)?,
            metrics: serde_json::from_str(&metrics)?,
            unverified_quotes: serde_json::from_str(&unverified)?,
            answer_checks: self.answer_checks(id)?,
            turn_signals,
            timeline_scorer,
            inputs: inputs.as_deref().and_then(|j| serde_json::from_str(j).ok()),
            parent_id,
        }))
    }

    /// Store a report's timeline, replacing any earlier one (excerpts are capped; the transcript
    /// has the full text).
    pub fn set_turn_signals(
        &self,
        session_id: i64,
        analysis_id: i64,
        signals: &[crate::temperature::Signal],
        scorer: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM turn_signals WHERE analysis_id = ?1",
            [analysis_id],
        )?;
        for s in signals {
            let mut features = serde_json::json!({
                "voice": s.voice, "z": s.z, "backchannel_rate": s.backchannel_rate, "latency_s": s.latency_s,
                "smoothed": s.smoothed,
            });
            if let Some(video) = &s.video {
                features["video"] = serde_json::to_value(video)?;
                features["video_method"] = crate::video::METHOD.into();
            }
            let excerpt: String = s.text.chars().take(600).collect();
            tx.execute(
                "INSERT INTO turn_signals (analysis_id, session_id, turn_idx, speaker, kind, start_s, end_s, excerpt,
                 temperature, scorer, method, checks_json, features_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![analysis_id, session_id, s.turn_idx as i64, s.speaker, serde_json::to_value(s.kind)?.as_str(),
                        s.start, s.end, excerpt, s.temperature, scorer, crate::temperature::METHOD,
                        serde_json::to_string(&s.checks)?, features.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A run's timeline in turn order, and the scorer that judged it.
    pub fn turn_signals(
        &self,
        analysis_id: i64,
    ) -> Result<(Vec<crate::temperature::Signal>, Option<String>)> {
        let mut stmt = self.conn.prepare(
            "SELECT turn_idx, speaker, kind, start_s, end_s, excerpt, temperature, scorer, checks_json, features_json
             FROM turn_signals WHERE analysis_id = ?1 ORDER BY turn_idx",
        )?;
        let mut scorer = None;
        let rows = stmt.query_map([analysis_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<f64>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (idx, speaker, kind, start, end, text, temperature, row_scorer, checks, features) =
                row?;
            if scorer.is_none() {
                scorer = row_scorer;
            }
            let f: serde_json::Value = serde_json::from_str(&features)?;
            out.push(crate::temperature::Signal {
                turn_idx: idx as usize,
                kind: serde_json::from_value(serde_json::Value::String(kind))?,
                speaker,
                start,
                end,
                text,
                checks: serde_json::from_str(&checks)?,
                voice: serde_json::from_value(f["voice"].clone()).unwrap_or(None),
                z: serde_json::from_value(f["z"].clone()).unwrap_or_default(),
                backchannel_rate: f["backchannel_rate"].as_f64(),
                latency_s: f["latency_s"].as_f64(),
                video: serde_json::from_value(f["video"].clone()).unwrap_or(None),
                temperature,
                smoothed: f["smoothed"].as_f64(),
            });
        }
        Ok((out, scorer))
    }

    pub fn add_answer_checks(
        &self,
        session_id: i64,
        analysis_id: i64,
        checks: &[AnswerCheck],
    ) -> Result<()> {
        self.write_answer_checks(session_id, analysis_id, checks, false)
    }

    pub fn replace_answer_checks(
        &self,
        session_id: i64,
        analysis_id: i64,
        checks: &[AnswerCheck],
    ) -> Result<()> {
        self.write_answer_checks(session_id, analysis_id, checks, true)
    }

    fn write_answer_checks(
        &self,
        session_id: i64,
        analysis_id: i64,
        checks: &[AnswerCheck],
        replace: bool,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if replace {
            tx.execute(
                "DELETE FROM answer_checks WHERE analysis_id = ?1",
                [analysis_id],
            )?;
        }
        let now = now_iso();
        for c in checks {
            tx.execute(
                "INSERT INTO answer_checks (analysis_id, session_id, created_at, answer_idx, answer_start, question, check_id,
                 scorer, pick, value, confidence, verdict) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![analysis_id, session_id, now, c.answer_idx, c.answer_start, c.question, c.check_id, c.scorer, c.pick,
                        c.value, c.confidence, c.verdict],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn answer_checks(&self, analysis_id: i64) -> Result<Vec<AnswerCheck>> {
        let mut stmt = self.conn.prepare(
            "SELECT answer_idx, answer_start, question, check_id, scorer, pick, value, confidence, verdict FROM answer_checks
             WHERE analysis_id = ?1 ORDER BY answer_idx, id",
        )?;
        let rows = stmt.query_map([analysis_id], |r| {
            Ok(AnswerCheck {
                answer_idx: r.get(0)?,
                answer_start: r.get(1)?,
                question: r.get(2)?,
                check_id: r.get(3)?,
                scorer: r.get(4)?,
                pick: r.get(5)?,
                value: r.get(6)?,
                confidence: r.get(7)?,
                verdict: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The company each session's latest report inferred (shown when you didn't enter one).
    pub fn latest_companies(&self) -> Result<HashMap<i64, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, json_extract(analysis_json, '$.context.company') FROM analyses
             WHERE id IN (SELECT MAX(id) FROM analyses GROUP BY session_id)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        Ok(rows
            .filter_map(|r| r.map(|(id, c)| c.map(|c| (id, c))).transpose())
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn latest_verdicts(&self) -> Result<HashMap<i64, Verdict>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, verdict FROM analyses WHERE id IN (SELECT MAX(id) FROM analyses GROUP BY session_id)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, parse::<Verdict>(r.get(1)?)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn analyses(&self, session_id: i64) -> Result<Vec<StoredAnalysis>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM analyses WHERE session_id = ?1 ORDER BY id DESC")?;
        let rows = stmt
            .query_map([session_id], analysis_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(
                    id,
                    session_id,
                    created_at,
                    model,
                    prompt_version,
                    analysis,
                    metrics,
                    unverified,
                    inputs,
                    parent_id,
                )| {
                    let (turn_signals, timeline_scorer) = self.turn_signals(id)?;
                    Ok(StoredAnalysis {
                        id,
                        session_id,
                        created_at,
                        model,
                        prompt_version,
                        analysis: serde_json::from_str(&analysis)?,
                        metrics: serde_json::from_str(&metrics)?,
                        unverified_quotes: serde_json::from_str(&unverified)?,
                        answer_checks: self.answer_checks(id)?,
                        turn_signals,
                        timeline_scorer,
                        inputs: inputs.as_deref().and_then(|j| serde_json::from_str(j).ok()),
                        parent_id,
                    })
                },
            )
            .collect()
    }

    pub fn analysis_by_id(&self, id: i64) -> Result<Option<StoredAnalysis>> {
        self.analysis_where("id = ?1", id)
    }

    // --- stage runs -------------------------------------------------------------------------

    pub fn start_run(
        &self,
        session_id: i64,
        step: Step,
        params: &serde_json::Value,
        input_run_id: Option<i64>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO step_runs (session_id, step, status, started_at, params_json, input_run_id, pid)
             VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6)",
            params![session_id, step.as_str(), now_iso(), params.to_string(), input_run_id, std::process::id() as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// A finished run from history (used to backfill older sessions). One argument per column.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_finished_run(
        &self,
        session_id: i64,
        step: Step,
        status: RunStatus,
        at: &str,
        params: &serde_json::Value,
        input_run_id: Option<i64>,
        output_id: Option<i64>,
        error: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO step_runs (session_id, step, status, started_at, finished_at, params_json, input_run_id,
             output_id, error) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8)",
            params![session_id, step.as_str(), status.as_str(), at, params.to_string(), input_run_id, output_id, error],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(&self, id: i64, output_id: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE step_runs SET status = 'succeeded', finished_at = ?2, output_id = ?3, progress = NULL WHERE id = ?1",
            params![id, now_iso(), output_id],
        )?;
        Ok(())
    }

    pub fn fail_run(&self, id: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE step_runs SET status = 'failed', finished_at = ?2, error = ?3, progress = NULL WHERE id = ?1",
            params![id, now_iso(), error],
        )?;
        Ok(())
    }

    pub fn set_run_input(&self, id: i64, input_run_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE step_runs SET input_run_id = ?2 WHERE id = ?1",
            params![id, input_run_id],
        )?;
        Ok(())
    }

    pub fn set_run_warnings(&self, id: i64, warnings: &[String]) -> Result<()> {
        let json = (!warnings.is_empty())
            .then(|| serde_json::to_string(warnings).expect("strings serialize"));
        self.conn.execute(
            "UPDATE step_runs SET warnings_json = ?2 WHERE id = ?1",
            params![id, json],
        )?;
        Ok(())
    }

    pub fn set_run_progress(&self, id: i64, progress: Option<f64>, message: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE step_runs SET progress = ?2, message = ?3 WHERE id = ?1",
            params![id, progress, message],
        )?;
        Ok(())
    }

    /// Every run for a session, oldest first.
    pub fn runs(&self, session_id: i64) -> Result<Vec<StepRun>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM step_runs WHERE session_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([session_id], run_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- next steps -------------------------------------------------------------------------

    pub fn add_next_steps(
        &self,
        session_id: i64,
        plan: &NextSteps,
        model: &str,
        prompt_version: &str,
        analysis_id: Option<i64>,
        unverified_quotes: &[String],
    ) -> Result<StoredNextSteps> {
        self.conn.execute(
            "INSERT INTO next_steps (session_id, created_at, model, prompt_version, analysis_id, plan_json,
             unverified_quotes_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, now_iso(), model, prompt_version, analysis_id, serde_json::to_string(plan)?,
                    serde_json::to_string(unverified_quotes)?],
        )?;
        let id = self.conn.last_insert_rowid();
        self.next_steps_by_id(id)?
            .ok_or_else(|| anyhow!("next steps {id} vanished"))
    }

    /// (id, created_at, model, the report it was planned from) for every next-steps run, oldest first.
    pub fn next_steps_index(&self, session_id: i64) -> Result<Vec<NextStepsEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, model, analysis_id FROM next_steps WHERE session_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([session_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn next_steps_by_id(&self, id: i64) -> Result<Option<StoredNextSteps>> {
        self.next_steps_where("id = ?1", id)
    }

    pub fn latest_next_steps(&self, session_id: i64) -> Result<Option<StoredNextSteps>> {
        self.next_steps_where("session_id = ?1 ORDER BY id DESC LIMIT 1", session_id)
    }

    fn next_steps_where(&self, clause: &str, arg: i64) -> Result<Option<StoredNextSteps>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT * FROM next_steps WHERE {clause}"),
                [arg],
                |r| {
                    Ok((
                        r.get::<_, i64>("id")?,
                        r.get::<_, i64>("session_id")?,
                        r.get::<_, String>("created_at")?,
                        r.get::<_, String>("model")?,
                        r.get::<_, String>("prompt_version")?,
                        r.get::<_, Option<i64>>("analysis_id")?,
                        r.get::<_, String>("plan_json")?,
                        r.get::<_, String>("unverified_quotes_json")?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            session_id,
            created_at,
            model,
            prompt_version,
            analysis_id,
            plan,
            unverified,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(StoredNextSteps {
            id,
            session_id,
            created_at,
            model,
            prompt_version,
            analysis_id,
            plan: serde_json::from_str(&plan)?,
            unverified_quotes: serde_json::from_str(&unverified)?,
        }))
    }

    // --- outcomes ---------------------------------------------------------------------------

    pub fn set_outcome(
        &self,
        session_id: i64,
        result: OutcomeResult,
        notes: Option<&str>,
    ) -> Result<Outcome> {
        self.get_session(session_id)?;
        self.conn.execute(
            "INSERT INTO outcomes (session_id, result, notes, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET result = excluded.result,
             notes = COALESCE(excluded.notes, outcomes.notes), updated_at = excluded.updated_at",
            params![session_id, result.as_str(), notes, now_iso()],
        )?;
        Ok(self.get_outcome(session_id)?.expect("just written"))
    }

    pub fn get_outcome(&self, session_id: i64) -> Result<Option<Outcome>> {
        Ok(self.all_outcomes()?.remove(&session_id))
    }

    pub fn all_outcomes(&self) -> Result<HashMap<i64, Outcome>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id, result, notes, updated_at FROM outcomes")?;
        let rows = stmt.query_map([], |r| {
            let o = Outcome {
                session_id: r.get(0)?,
                result: parse(r.get(1)?)?,
                notes: r.get(2)?,
                updated_at: r.get(3)?,
            };
            Ok((o.session_id, o))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn new_session(mode: Mode) -> NewSession {
        NewSession {
            title: "Acme screen".into(),
            company: Some("Acme".into()),
            source: Source::Upload,
            mode,
            source_path: None,
            num_speakers: Some(2),
            consent: None,
            status: Status::New,
        }
    }

    #[test]
    fn session_and_segment_roundtrip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut db = Db::open(&dir.path().join("coach.db"))?;
        let mut s = db.create_session(new_session(Mode::Single))?;
        assert_eq!((s.id, s.status), (1, Status::New));
        s.dir = dir.path().display().to_string();
        s.duration_s = Some(61.5);
        s.stage = Some(Stage::HiringManager);
        db.save_session(&s)?;
        db.set_stage(1, s.stage)?; // the round is yours: saved by its own setter, never by save_session
        assert_eq!(db.get_session(1)?, s);

        let segs = vec![
            Segment {
                words: vec![Word {
                    start: 0.0,
                    end: 0.5,
                    text: " Hi".into(),
                }],
                ..Segment::new(0.0, 1.5, "Hi there.", "interviewer")
            },
            Segment::new(2.0, 3.0, "Hello!", "you"),
        ];
        db.replace_segments(s.id, &segs)?;
        assert_eq!(db.get_segments(s.id)?, segs);
        db.replace_segments(s.id, &segs[..1])?;
        assert_eq!(db.get_segments(s.id)?.len(), 1);
        assert_eq!(
            db.list_sessions()?.iter().map(|s| s.id).collect::<Vec<_>>(),
            [1]
        );
        Ok(())
    }

    /// A version-2 database (stage runs, no run warnings) gains the column and keeps its runs.
    #[test]
    fn version_2_databases_gain_run_warnings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("coach.db");
        {
            let db = Db::open(&path)?;
            let s = db.create_session(new_session(Mode::Dual))?;
            db.start_run(s.id, Step::Recording, &serde_json::json!({}), None)?;
            db.conn.execute_batch(
                "ALTER TABLE step_runs DROP COLUMN warnings_json; PRAGMA user_version = 2;",
            )?;
        }
        let db = Db::open(&path)?;
        let run = db.runs(1)?.pop().expect("the run survives");
        assert!(run.warnings.is_empty());
        db.set_run_warnings(run.id, &["Speaker detection found 1 voice".into()])?;
        assert_eq!(db.runs(1)?[0].warnings, ["Speaker detection found 1 voice"]);
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            SCHEMA_VERSION
        );
        Ok(())
    }

    /// A version-4 database gains the timeline table; its analyses load with an empty timeline.
    #[test]
    fn version_4_databases_gain_the_timeline() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("coach.db");
        {
            let db = Db::open(&path)?;
            db.create_session(new_session(Mode::Dual))?;
            db.conn.execute_batch("DROP INDEX turn_signals_by_analysis; DROP TABLE turn_signals; PRAGMA user_version = 4;")?;
        }
        let db = Db::open(&path)?;
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            SCHEMA_VERSION
        );
        assert_eq!(db.turn_signals(1)?, (vec![], None));
        assert!(db.get_session(1).is_ok(), "the session survives");
        Ok(())
    }

    /// A version-5 database gains report inputs and transcript revisions; the live transcript is
    /// kept as its transcript run's revision.
    #[test]
    fn version_5_databases_gain_versions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("coach.db");
        {
            let mut db = Db::open(&path)?;
            let s = db.create_session(new_session(Mode::Dual))?;
            db.replace_segments(
                s.id,
                &[Segment::new(
                    0.0,
                    2.0,
                    "Tell me about yourself.",
                    crate::models::INTERVIEWER,
                )],
            )?;
            db.insert_finished_run(
                s.id,
                Step::Transcript,
                RunStatus::Succeeded,
                "t",
                &serde_json::json!({}),
                None,
                None,
                None,
            )?;
            db.conn.execute_batch(
                "DROP INDEX analyses_by_inputs; DROP TABLE transcript_revisions; DROP TABLE judgments;
                 ALTER TABLE analyses DROP COLUMN inputs_key; ALTER TABLE analyses DROP COLUMN inputs_json;
                 ALTER TABLE analyses DROP COLUMN parent_id; PRAGMA user_version = 5;")?;
        }
        let db = Db::open(&path)?;
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            SCHEMA_VERSION
        );
        let revisions = db.transcript_revisions(1)?;
        assert_eq!(revisions.len(), 1, "the live transcript is kept");
        assert_eq!(
            revisions[0].1,
            crate::versions::transcript_sha(&db.get_segments(1)?)
        );
        assert_eq!(
            db.analysis_by_key(1, "none")?.map(|a| a.id),
            None,
            "the new columns are queryable"
        );
        Ok(())
    }

    #[test]
    fn outcome_upsert_keeps_notes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Db::open(&dir.path().join("coach.db"))?;
        let s = db.create_session(new_session(Mode::Dual))?;
        db.set_outcome(
            s.id,
            OutcomeResult::Pending,
            Some("Recruiter said next week"),
        )?;
        let o = db.set_outcome(s.id, OutcomeResult::Advanced, None)?;
        assert_eq!(o.result, OutcomeResult::Advanced);
        assert_eq!(o.notes.as_deref(), Some("Recruiter said next week"));
        Ok(())
    }
}
