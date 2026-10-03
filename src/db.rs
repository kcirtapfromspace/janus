//! SQLite index. Audio and exports live in per-session folders; the database ties them together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::metrics::TalkMetrics;
use crate::models::{Mode, NextSteps, OutcomeResult, RunStatus, Segment, Session, SessionAnalysis, Source, Stage, Status,
                    Step, Verdict, Word};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS roles (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    title TEXT NOT NULL,
    level TEXT,
    company TEXT,
    jd_text TEXT,
    profile_json TEXT
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
    error TEXT
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
    unverified_quotes_json TEXT NOT NULL
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
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
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
const SCHEMA_VERSION: i64 = 4;

fn parse<T: std::str::FromStr<Err = String>>(s: String) -> rusqlite::Result<T> {
    s.parse().map_err(|e: String| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into()))
}

fn session_from_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get("id")?,
        created_at: r.get("created_at")?,
        title: r.get("title")?,
        company: r.get("company")?,
        stage: r.get::<_, Option<String>>("stage")?.map(parse::<Stage>).transpose()?,
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
    })
}

/// id, session_id, created_at, model, prompt_version, then the three JSON columns.
type AnalysisRow = (i64, i64, String, String, String, String, String, String);

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
        let db = Db { conn, path: path.to_path_buf() };
        db.migrate()?;
        Ok(db)
    }

    /// Another connection to the same database (for writing progress while a stage runs).
    pub fn reopen(&self) -> Result<Db> {
        Db::open(&self.path)
    }

    /// Version 2 added stage runs: sessions created before it get runs synthesized from what
    /// they already have, once, so their stages show correctly. Version 3 added run warnings.
    fn migrate(&self) -> Result<()> {
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        // A v2 database's step_runs predates the column; a new or pre-v2 one just got it from SCHEMA.
        let has_warnings: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('step_runs') WHERE name = 'warnings_json'", [], |r| r.get(0))?;
        if !has_warnings {
            self.conn.execute_batch("ALTER TABLE step_runs ADD COLUMN warnings_json TEXT;")?;
        }
        if version < 2 {
            for session in self.list_sessions()? {
                crate::steps::backfill(self, &session)?;
            }
        }
        self.conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
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
            .query_row("SELECT * FROM sessions WHERE id = ?1", [id], session_from_row)
            .optional()?
            .ok_or_else(|| anyhow!("No session with id {id}. See: ic list"))
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare("SELECT * FROM sessions ORDER BY created_at DESC, id DESC")?;
        let rows = stmt.query_map([], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Write every mutable field of `s` back to the database.
    pub fn save_session(&self, s: &Session) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2, company = ?3, stage = ?4, role_id = ?5, source_path = ?6, dir = ?7,
             duration_s = ?8, num_speakers = ?9, consent = ?10, status = ?11, error = ?12 WHERE id = ?1",
            params![s.id, s.title, s.company, s.stage.map(|x| x.as_str()), s.role_id, s.source_path, s.dir,
                    s.duration_s, s.num_speakers, s.consent, s.status.as_str(), s.error],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: i64, status: Status, error: Option<String>) -> Result<()> {
        self.conn.execute("UPDATE sessions SET status = ?2, error = ?3 WHERE id = ?1", params![id, status.as_str(), error])?;
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
                let words = (!s.words.is_empty()).then(|| serde_json::to_string(&s.words)).transpose()?;
                stmt.execute(params![session_id, i as i64, s.start, s.end, s.speaker, s.text, words])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_segments(&self, session_id: i64) -> Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare("SELECT * FROM segments WHERE session_id = ?1 ORDER BY idx")?;
        let rows = stmt.query_map([session_id], |r| {
            Ok((r.get("start_s")?, r.get("end_s")?, r.get("speaker")?, r.get("text")?, r.get::<_, Option<String>>("words_json")?))
        })?;
        rows.map(|row| {
            let (start, end, speaker, text, words): (f64, f64, String, String, Option<String>) = row?;
            let words: Vec<Word> = words.map(|w| serde_json::from_str(&w)).transpose()?.unwrap_or_default();
            Ok(Segment { start, end, speaker, text, words })
        })
        .collect()
    }

    // --- analyses ---------------------------------------------------------------------------

    pub fn add_analysis(&self, session_id: i64, analysis: &SessionAnalysis, metrics: &TalkMetrics, model: &str,
                        prompt_version: &str, unverified_quotes: &[String]) -> Result<StoredAnalysis> {
        self.conn.execute(
            "INSERT INTO analyses (session_id, created_at, model, prompt_version, verdict, analysis_json, metrics_json,
             unverified_quotes_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![session_id, now_iso(), model, prompt_version, analysis.outlook.verdict.as_str(),
                    serde_json::to_string(analysis)?, serde_json::to_string(metrics)?,
                    serde_json::to_string(unverified_quotes)?],
        )?;
        let id = self.conn.last_insert_rowid();
        self.analysis_where("id = ?1", id)?.ok_or_else(|| anyhow!("analysis {id} vanished"))
    }

    pub fn latest_analysis(&self, session_id: i64) -> Result<Option<StoredAnalysis>> {
        self.analysis_where("session_id = ?1 ORDER BY id DESC LIMIT 1", session_id)
    }

    fn analysis_where(&self, clause: &str, arg: i64) -> Result<Option<StoredAnalysis>> {
        let row = self
            .conn
            .query_row(&format!("SELECT * FROM analyses WHERE {clause}"), [arg], analysis_from_row)
            .optional()?;
        let Some((id, session_id, created_at, model, prompt_version, analysis, metrics, unverified)) = row else {
            return Ok(None);
        };
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
        }))
    }

    pub fn add_answer_checks(&self, session_id: i64, analysis_id: i64, checks: &[AnswerCheck]) -> Result<()> {
        let now = now_iso();
        for c in checks {
            self.conn.execute(
                "INSERT INTO answer_checks (analysis_id, session_id, created_at, answer_idx, answer_start, question, check_id,
                 scorer, pick, value, confidence, verdict) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![analysis_id, session_id, now, c.answer_idx, c.answer_start, c.question, c.check_id, c.scorer, c.pick,
                        c.value, c.confidence, c.verdict],
            )?;
        }
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

    pub fn latest_verdicts(&self) -> Result<HashMap<i64, Verdict>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, verdict FROM analyses WHERE id IN (SELECT MAX(id) FROM analyses GROUP BY session_id)",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, parse::<Verdict>(r.get(1)?)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn analyses(&self, session_id: i64) -> Result<Vec<StoredAnalysis>> {
        let mut stmt = self.conn.prepare("SELECT * FROM analyses WHERE session_id = ?1 ORDER BY id DESC")?;
        let rows = stmt.query_map([session_id], analysis_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, session_id, created_at, model, prompt_version, analysis, metrics, unverified)| {
                Ok(StoredAnalysis {
                    id, session_id, created_at, model, prompt_version,
                    analysis: serde_json::from_str(&analysis)?,
                    metrics: serde_json::from_str(&metrics)?,
                    unverified_quotes: serde_json::from_str(&unverified)?,
                    answer_checks: self.answer_checks(id)?,
                })
            })
            .collect()
    }

    pub fn analysis_by_id(&self, id: i64) -> Result<Option<StoredAnalysis>> {
        self.analysis_where("id = ?1", id)
    }

    // --- stage runs -------------------------------------------------------------------------

    pub fn start_run(&self, session_id: i64, step: Step, params: &serde_json::Value, input_run_id: Option<i64>)
        -> Result<i64> {
        self.conn.execute(
            "INSERT INTO step_runs (session_id, step, status, started_at, params_json, input_run_id, pid)
             VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6)",
            params![session_id, step.as_str(), now_iso(), params.to_string(), input_run_id, std::process::id() as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// A finished run from history (used to backfill older sessions). One argument per column.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_finished_run(&self, session_id: i64, step: Step, status: RunStatus, at: &str,
                               params: &serde_json::Value, input_run_id: Option<i64>, output_id: Option<i64>,
                               error: Option<&str>) -> Result<i64> {
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
        self.conn.execute("UPDATE step_runs SET input_run_id = ?2 WHERE id = ?1", params![id, input_run_id])?;
        Ok(())
    }

    pub fn set_run_warnings(&self, id: i64, warnings: &[String]) -> Result<()> {
        let json = (!warnings.is_empty()).then(|| serde_json::to_string(warnings).expect("strings serialize"));
        self.conn.execute("UPDATE step_runs SET warnings_json = ?2 WHERE id = ?1", params![id, json])?;
        Ok(())
    }

    pub fn set_run_progress(&self, id: i64, progress: Option<f64>, message: &str) -> Result<()> {
        self.conn.execute("UPDATE step_runs SET progress = ?2, message = ?3 WHERE id = ?1", params![id, progress, message])?;
        Ok(())
    }

    /// Every run for a session, oldest first.
    pub fn runs(&self, session_id: i64) -> Result<Vec<StepRun>> {
        let mut stmt = self.conn.prepare("SELECT * FROM step_runs WHERE session_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([session_id], run_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- next steps -------------------------------------------------------------------------

    pub fn add_next_steps(&self, session_id: i64, plan: &NextSteps, model: &str, prompt_version: &str,
                          analysis_id: Option<i64>, unverified_quotes: &[String]) -> Result<StoredNextSteps> {
        self.conn.execute(
            "INSERT INTO next_steps (session_id, created_at, model, prompt_version, analysis_id, plan_json,
             unverified_quotes_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, now_iso(), model, prompt_version, analysis_id, serde_json::to_string(plan)?,
                    serde_json::to_string(unverified_quotes)?],
        )?;
        let id = self.conn.last_insert_rowid();
        self.next_steps_by_id(id)?.ok_or_else(|| anyhow!("next steps {id} vanished"))
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
            .query_row(&format!("SELECT * FROM next_steps WHERE {clause}"), [arg], |r| {
                Ok((r.get::<_, i64>("id")?, r.get::<_, i64>("session_id")?, r.get::<_, String>("created_at")?,
                    r.get::<_, String>("model")?, r.get::<_, String>("prompt_version")?,
                    r.get::<_, Option<i64>>("analysis_id")?, r.get::<_, String>("plan_json")?,
                    r.get::<_, String>("unverified_quotes_json")?))
            })
            .optional()?;
        let Some((id, session_id, created_at, model, prompt_version, analysis_id, plan, unverified)) = row else {
            return Ok(None);
        };
        Ok(Some(StoredNextSteps {
            id, session_id, created_at, model, prompt_version, analysis_id,
            plan: serde_json::from_str(&plan)?,
            unverified_quotes: serde_json::from_str(&unverified)?,
        }))
    }

    // --- outcomes ---------------------------------------------------------------------------

    pub fn set_outcome(&self, session_id: i64, result: OutcomeResult, notes: Option<&str>) -> Result<Outcome> {
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
        let mut stmt = self.conn.prepare("SELECT session_id, result, notes, updated_at FROM outcomes")?;
        let rows = stmt.query_map([], |r| {
            let o = Outcome { session_id: r.get(0)?, result: parse(r.get(1)?)?, notes: r.get(2)?, updated_at: r.get(3)? };
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
        assert_eq!(db.get_session(1)?, s);

        let segs = vec![
            Segment { words: vec![Word { start: 0.0, end: 0.5, text: " Hi".into() }], ..Segment::new(0.0, 1.5, "Hi there.", "interviewer") },
            Segment::new(2.0, 3.0, "Hello!", "you"),
        ];
        db.replace_segments(s.id, &segs)?;
        assert_eq!(db.get_segments(s.id)?, segs);
        db.replace_segments(s.id, &segs[..1])?;
        assert_eq!(db.get_segments(s.id)?.len(), 1);
        assert_eq!(db.list_sessions()?.iter().map(|s| s.id).collect::<Vec<_>>(), [1]);
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
            db.conn.execute_batch("ALTER TABLE step_runs DROP COLUMN warnings_json; PRAGMA user_version = 2;")?;
        }
        let db = Db::open(&path)?;
        let run = db.runs(1)?.pop().expect("the run survives");
        assert!(run.warnings.is_empty());
        db.set_run_warnings(run.id, &["Speaker detection found 1 voice".into()])?;
        assert_eq!(db.runs(1)?[0].warnings, ["Speaker detection found 1 voice"]);
        assert_eq!(db.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?, SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn outcome_upsert_keeps_notes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Db::open(&dir.path().join("coach.db"))?;
        let s = db.create_session(new_session(Mode::Dual))?;
        db.set_outcome(s.id, OutcomeResult::Pending, Some("Recruiter said next week"))?;
        let o = db.set_outcome(s.id, OutcomeResult::Advanced, None)?;
        assert_eq!(o.result, OutcomeResult::Advanced);
        assert_eq!(o.notes.as_deref(), Some("Recruiter said next week"));
        Ok(())
    }
}
