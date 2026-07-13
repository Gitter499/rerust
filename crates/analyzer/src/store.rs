//! SQLite persistence for detected projects.
//!
//! One `projects` table keyed by repository URL. Upserts preserve
//! `first_detected` while refreshing other fields.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::types::{Project, RewritePr, Signal};

const PROJECT_COLS: &str = r#"
    name, repo_url, description, stars, original_language,
    rust_percentage, confidence, signals, source_url,
    first_detected, last_seen, open_issues, open_prs, forks,
    rewrite_pr_title, rewrite_pr_url, unsafe_percentage, project_kind,
    named_origin, lines_added, lines_removed, rewrite_velocity,
    ai_assist_score, rewrite_duration_days, commit_count,
    history_from_language, history_rust_before, history_rust_after,
    transition_magnitude, total_commits_analyzed,
    history_status, history_error, history_attempted_at, history_attempts,
    rewrite_prs, ai_agents
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open db at {path}"))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                repo_url          TEXT PRIMARY KEY,
                name              TEXT NOT NULL,
                description       TEXT,
                stars             INTEGER NOT NULL DEFAULT 0,
                original_language TEXT,
                rust_percentage   REAL NOT NULL DEFAULT 0,
                confidence        REAL NOT NULL DEFAULT 0,
                signals           TEXT NOT NULL DEFAULT '[]',
                source_url        TEXT NOT NULL DEFAULT '',
                first_detected    TEXT NOT NULL,
                last_seen         TEXT NOT NULL,
                open_issues       INTEGER NOT NULL DEFAULT 0,
                open_prs          INTEGER NOT NULL DEFAULT 0,
                forks             INTEGER NOT NULL DEFAULT 0,
                rewrite_pr_title  TEXT,
                rewrite_pr_url    TEXT,
                rewrite_prs       TEXT NOT NULL DEFAULT '[]',
                unsafe_percentage REAL,
                project_kind      TEXT NOT NULL DEFAULT 'replacement',
                named_origin      TEXT,
                lines_added       INTEGER,
                lines_removed     INTEGER,
                rewrite_velocity  REAL,
                ai_assist_score   REAL,
                rewrite_duration_days INTEGER,
                commit_count      INTEGER,
                history_from_language TEXT,
                history_rust_before REAL,
                history_rust_after REAL,
                transition_magnitude REAL,
                total_commits_analyzed INTEGER,
                history_status TEXT,
                history_error TEXT,
                history_attempted_at TEXT,
                history_attempts INTEGER,
                ai_agents TEXT NOT NULL DEFAULT '[]'
            );
            CREATE INDEX IF NOT EXISTS idx_projects_confidence ON projects(confidence DESC);
            CREATE INDEX IF NOT EXISTS idx_projects_stars ON projects(stars DESC);
            "#,
        )
        .context("initialize schema")?;

        // Older DBs: ADD COLUMN is a no-op when the column already exists.
        for column in [
            "open_issues INTEGER NOT NULL DEFAULT 0",
            "open_prs INTEGER NOT NULL DEFAULT 0",
            "forks INTEGER NOT NULL DEFAULT 0",
            "rewrite_pr_title TEXT",
            "rewrite_pr_url TEXT",
            "rewrite_prs TEXT NOT NULL DEFAULT '[]'",
            "unsafe_percentage REAL",
            "project_kind TEXT NOT NULL DEFAULT 'replacement'",
            "named_origin TEXT",
            "lines_added INTEGER",
            "lines_removed INTEGER",
            "rewrite_velocity REAL",
            "ai_assist_score REAL",
            "rewrite_duration_days INTEGER",
            "commit_count INTEGER",
            "history_from_language TEXT",
            "history_rust_before REAL",
            "history_rust_after REAL",
            "transition_magnitude REAL",
            "total_commits_analyzed INTEGER",
            "history_status TEXT",
            "history_error TEXT",
            "history_attempted_at TEXT",
            "history_attempts INTEGER",
            "ai_agents TEXT NOT NULL DEFAULT '[]'",
        ] {
            add_column_if_missing(&conn, "projects", column)?;
        }

        Ok(Self { conn })
    }

    pub fn upsert(&self, project: &Project) -> Result<()> {
        let signals_json = serde_json::to_string(&project.signals)?;
        let rewrite_prs = project.effective_rewrite_prs();
        let rewrite_prs_json = serde_json::to_string(&rewrite_prs)?;
        let ai_agents_json = serde_json::to_string(&project.ai_agents)?;
        let rewrite_pr_title = rewrite_prs.first().map(|r| r.title.as_str());
        let rewrite_pr_url = rewrite_prs.first().map(|r| r.url.as_str());
        self.conn
            .execute(
                r#"
                INSERT INTO projects (
                    repo_url, name, description, stars, original_language,
                    rust_percentage, confidence, signals, source_url,
                    first_detected, last_seen, open_issues, open_prs, forks,
                    rewrite_pr_title, rewrite_pr_url, rewrite_prs, unsafe_percentage, project_kind,
                    named_origin, lines_added, lines_removed, rewrite_velocity,
                    ai_assist_score, rewrite_duration_days, commit_count,
                    history_from_language, history_rust_before, history_rust_after,
                    transition_magnitude, total_commits_analyzed,
                    history_status, history_error, history_attempted_at, history_attempts,
                    ai_agents
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36)
                ON CONFLICT(repo_url) DO UPDATE SET
                    name              = excluded.name,
                    description       = excluded.description,
                    stars             = excluded.stars,
                    original_language = excluded.original_language,
                    rust_percentage   = excluded.rust_percentage,
                    confidence        = excluded.confidence,
                    signals           = excluded.signals,
                    source_url        = excluded.source_url,
                    last_seen         = excluded.last_seen,
                    open_issues       = excluded.open_issues,
                    open_prs          = excluded.open_prs,
                    forks             = excluded.forks,
                    rewrite_pr_title  = excluded.rewrite_pr_title,
                    rewrite_pr_url    = excluded.rewrite_pr_url,
                    rewrite_prs       = excluded.rewrite_prs,
                    unsafe_percentage = COALESCE(excluded.unsafe_percentage, projects.unsafe_percentage),
                    project_kind      = excluded.project_kind,
                    named_origin      = excluded.named_origin,
                    lines_added       = COALESCE(excluded.lines_added, projects.lines_added),
                    lines_removed     = COALESCE(excluded.lines_removed, projects.lines_removed),
                    rewrite_velocity  = COALESCE(excluded.rewrite_velocity, projects.rewrite_velocity),
                    ai_assist_score   = COALESCE(excluded.ai_assist_score, projects.ai_assist_score),
                    rewrite_duration_days = COALESCE(excluded.rewrite_duration_days, projects.rewrite_duration_days),
                    commit_count      = COALESCE(excluded.commit_count, projects.commit_count),
                    history_from_language = excluded.history_from_language,
                    history_rust_before = excluded.history_rust_before,
                    history_rust_after = excluded.history_rust_after,
                    transition_magnitude = excluded.transition_magnitude,
                    total_commits_analyzed = COALESCE(excluded.total_commits_analyzed, projects.total_commits_analyzed),
                    history_status = COALESCE(excluded.history_status, projects.history_status),
                    history_error = COALESCE(excluded.history_error, projects.history_error),
                    history_attempted_at = COALESCE(excluded.history_attempted_at, projects.history_attempted_at),
                    history_attempts = COALESCE(excluded.history_attempts, projects.history_attempts),
                    ai_agents = CASE
                        WHEN excluded.ai_agents IS NOT NULL AND excluded.ai_agents != '[]'
                        THEN excluded.ai_agents
                        ELSE projects.ai_agents
                    END
                "#,
                rusqlite::params![
                    project.repo_url,
                    project.name,
                    project.description,
                    project.stars,
                    project.original_language,
                    project.rust_percentage,
                    project.confidence,
                    signals_json,
                    project.source_url,
                    project.first_detected,
                    project.last_seen,
                    project.open_issues,
                    project.open_prs,
                    project.forks,
                    rewrite_pr_title,
                    rewrite_pr_url,
                    rewrite_prs_json,
                    project.unsafe_percentage,
                    project.project_kind,
                    project.named_origin,
                    project.lines_added,
                    project.lines_removed,
                    project.rewrite_velocity,
                    project.ai_assist_score,
                    project.rewrite_duration_days,
                    project.commit_count,
                    project.history_from_language,
                    project.history_rust_before,
                    project.history_rust_after,
                    project.transition_magnitude,
                    project.total_commits_analyzed,
                    project.history_status,
                    project.history_error,
                    project.history_attempted_at,
                    project.history_attempts,
                    ai_agents_json,
                ],
            )
            .with_context(|| format!("upsert {}", project.repo_url))?;
        Ok(())
    }

    pub fn mark_history_attempt(
        &self,
        repo_url: &str,
        status: &str,
        error: Option<&str>,
        attempted_at: &str,
        attempts: u32,
    ) -> Result<()> {
        self.conn
            .execute(
                r#"
                UPDATE projects SET
                    history_status = ?2,
                    history_error = ?3,
                    history_attempted_at = ?4,
                    history_attempts = ?5
                WHERE repo_url = ?1
                "#,
                rusqlite::params![repo_url, status, error, attempted_at, attempts],
            )
            .with_context(|| format!("mark history attempt for {repo_url}"))?;
        Ok(())
    }

    pub fn delete(&self, repo_url: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM projects WHERE repo_url = ?1", [repo_url])
            .with_context(|| format!("delete {repo_url}"))?;
        Ok(())
    }

    pub fn first_detected(&self, repo_url: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT first_detected FROM projects WHERE repo_url = ?1",
                [repo_url],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }

    pub fn get(&self, repo_url: &str) -> Result<Option<Project>> {
        let sql = format!("SELECT {PROJECT_COLS} FROM projects WHERE repo_url = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt
            .query_row([repo_url], |row| self.row_to_project(row))
            .optional()?;
        Ok(row)
    }

    pub fn all(&self) -> Result<Vec<Project>> {
        let sql = format!(
            "SELECT {PROJECT_COLS} FROM projects ORDER BY confidence DESC, stars DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| self.row_to_project(row))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn row_to_project(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
        let signals_json: String = row.get(7)?;
        let signals: Vec<Signal> = serde_json::from_str(&signals_json).unwrap_or_default();
        let rewrite_pr_title: Option<String> = row.get(14)?;
        let rewrite_pr_url: Option<String> = row.get(15)?;
        let rewrite_prs_json: String =
            row.get::<_, Option<String>>(34)?.unwrap_or_else(|| "[]".into());
        let mut rewrite_prs: Vec<RewritePr> =
            serde_json::from_str(&rewrite_prs_json).unwrap_or_default();
        if rewrite_prs.is_empty() {
            if let (Some(title), Some(url)) = (rewrite_pr_title, rewrite_pr_url) {
                rewrite_prs.push(RewritePr { title, url });
            }
        }
        let rewrite_pr = rewrite_prs.first().cloned();
        let ai_agents_json: String =
            row.get::<_, Option<String>>(35)?.unwrap_or_else(|| "[]".into());
        let ai_agents: Vec<String> = serde_json::from_str(&ai_agents_json).unwrap_or_default();
        Ok(Project {
            name: row.get(0)?,
            repo_url: row.get(1)?,
            description: row.get(2)?,
            stars: row.get(3)?,
            original_language: row.get(4)?,
            rust_percentage: row.get(5)?,
            confidence: row.get(6)?,
            signals,
            rewrite_prs,
            rewrite_pr,
            unsafe_percentage: row.get(16)?,
            project_kind: row.get(17)?,
            named_origin: row.get(18)?,
            lines_added: row.get(19)?,
            lines_removed: row.get(20)?,
            rewrite_velocity: row.get(21)?,
            ai_assist_score: row.get(22)?,
            rewrite_duration_days: row.get(23)?,
            commit_count: row.get(24)?,
            history_from_language: row.get(25)?,
            history_rust_before: row.get(26)?,
            history_rust_after: row.get(27)?,
            transition_magnitude: row.get(28)?,
            total_commits_analyzed: row.get(29)?,
            history_status: row.get(30)?,
            history_error: row.get(31)?,
            history_attempted_at: row.get(32)?,
            history_attempts: row.get(33)?,
            ai_agents,
            source_url: row.get(8)?,
            first_detected: row.get(9)?,
            last_seen: row.get(10)?,
            open_issues: row.get(11)?,
            open_prs: row.get(12)?,
            forks: row.get(13)?,
            exemplar: false,
        })
    }
}

fn add_column_if_missing(conn: &Connection, table: &str, column_def: &str) -> Result<()> {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column_def}");
    match conn.execute(&sql, []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e).with_context(|| format!("add column: {column_def}")),
    }
}
