use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::event::Event;

/// Returns `~/.trakr/`.
fn base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".trakr"))
}

/// Returns the path to the unified DB: `~/.trakr/trakr.db`.
fn db_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("trakr.db"))
}

/// Returns the sessions directory: `~/.trakr/sessions/`.
fn sessions_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("sessions"))
}

/// Returns the transcripts directory: `~/.trakr/transcripts/`.
fn transcripts_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("transcripts"))
}

fn jsonl_path(session_id: &str) -> Result<PathBuf> {
    Ok(sessions_dir()?.join(format!("{}.jsonl", session_id)))
}

/// Copy a Claude native session JSONL to `~/.trakr/transcripts/<session_id>.jsonl`.
///
/// No-ops silently if `source_path` does not exist (best-effort; hook path may race).
pub fn archive_transcript(session_id: &str, source_path: &std::path::Path) -> Result<()> {
    if !source_path.exists() {
        return Ok(());
    }
    let dir = transcripts_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating transcripts dir {}", dir.display()))?;
    let dest = dir.join(format!("{}.jsonl", session_id));
    fs::copy(source_path, &dest)
        .with_context(|| format!("archiving transcript to {}", dest.display()))?;
    Ok(())
}

fn open_db() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating base dir {}", parent.display()))?;
    }
    let conn = Connection::open(&path)
        .with_context(|| format!("opening SQLite db at {}", path.display()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
        .context("configuring SQLite pragmas")?;
    Ok(conn)
}

/// Create `~/.trakr/trakr.db` with the unified events table if it doesn't already exist.
/// Only needs to be called once at startup.
pub fn init_db() -> Result<()> {
    let dir = base_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating base dir {}", dir.display()))?;

    let sessions = sessions_dir()?;
    fs::create_dir_all(&sessions)
        .with_context(|| format!("creating sessions dir {}", sessions.display()))?;

    let transcripts = transcripts_dir()?;
    fs::create_dir_all(&transcripts)
        .with_context(|| format!("creating transcripts dir {}", transcripts.display()))?;

    let conn = open_db()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT    NOT NULL,
            timestamp  TEXT    NOT NULL,
            event_type TEXT    NOT NULL,
            payload    TEXT    NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            session_id   TEXT PRIMARY KEY,
            project_path TEXT,
            started_at   TEXT,
            ended_at     TEXT,
            model        TEXT,
            source       TEXT
        );",
    )
    .context("creating tables")?;

    run_migrations(&conn)?;

    Ok(())
}

/// Apply any pending schema migrations in order.
///
/// Each migration is guarded by a version check — safe to call repeatedly.
fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT    NOT NULL
        );"
    ).context("creating schema_migrations table")?;

    let applied: std::collections::HashSet<i64> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    // v1 — baseline: events + sessions (created above by CREATE TABLE IF NOT EXISTS).
    if !applied.contains(&1) {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
            rusqlite::params![Utc::now().to_rfc3339()],
        ).context("recording migration v1")?;
    }

    // v2 — add title, summary, last_prompt, generated_summary to sessions.
    if !applied.contains(&2) {
        let existing_cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
            let cols = stmt.query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            cols
        };
        for col in &["title", "summary", "last_prompt", "generated_summary"] {
            if !existing_cols.contains(*col) {
                conn.execute_batch(&format!("ALTER TABLE sessions ADD COLUMN {} TEXT;", col))
                    .with_context(|| format!("migration v2: adding column {}", col))?;
            }
        }
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?1)",
            rusqlite::params![Utc::now().to_rfc3339()],
        ).context("recording migration v2")?;
    }

    Ok(())
}

/// Insert an event into the unified SQLite DB and append a JSON line to the session's JSONL backup file.
pub fn insert_event(session_id: &str, event: &Event, timestamp: DateTime<Utc>) -> Result<()> {
    // Ensure DB and schema exist.
    init_db()?;

    let payload = serde_json::to_string(event).context("serializing event")?;
    let ts = timestamp.to_rfc3339();
    let event_type = event.event_type_label();

    // SQLite insert.
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO events (session_id, timestamp, event_type, payload) VALUES (?1, ?2, ?3, ?4)",
        params![session_id, ts, event_type, payload],
    )
    .context("inserting event into SQLite")?;

    // JSONL append — sessions dir may not exist yet.
    let sessions = sessions_dir()?;
    fs::create_dir_all(&sessions)
        .with_context(|| format!("creating sessions dir {}", sessions.display()))?;

    let jsonl = jsonl_path(session_id)?;
    let line = serde_json::json!({
        "session_id": session_id,
        "timestamp": ts,
        "event_type": event_type,
        "payload": serde_json::from_str::<serde_json::Value>(&payload)?
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl)
        .with_context(|| format!("opening JSONL file {}", jsonl.display()))?;
    writeln!(file, "{}", serde_json::to_string(&line)?)
        .context("writing to JSONL file")?;

    Ok(())
}

/// Query events from the unified DB.
/// If `session_id` is Some, returns only events for that session.
/// If None, returns all events ordered by insertion id.
pub fn get_events(session_id: Option<&str>) -> Result<Vec<(String, DateTime<Utc>, Event)>> {
    let conn = open_db()?;

    let (sql, params_vec): (&str, Vec<String>) = match session_id {
        Some(id) => (
            "SELECT session_id, timestamp, payload FROM events WHERE session_id = ?1 ORDER BY id ASC",
            vec![id.to_string()],
        ),
        None => (
            "SELECT session_id, timestamp, payload FROM events ORDER BY id ASC",
            vec![],
        ),
    };

    let mut stmt = conn.prepare(sql).context("preparing SELECT")?;

    let rows: Vec<(String, String, String)> = if params_vec.is_empty() {
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .context("querying events")?
        .collect::<Result<_, _>>()
        .context("reading rows")?
    } else {
        stmt.query_map(params![params_vec[0]], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .context("querying events")?
        .collect::<Result<_, _>>()
        .context("reading rows")?
    };

    let mut results = Vec::new();
    for (sid, ts, payload) in rows {
        let timestamp = DateTime::parse_from_rfc3339(&ts)
            .with_context(|| format!("parsing timestamp '{}'", ts))?
            .with_timezone(&Utc);
        let event: Event =
            serde_json::from_str(&payload).with_context(|| format!("parsing payload: {}", payload))?;
        results.push((sid, timestamp, event));
    }

    Ok(results)
}

/// Returns the set of session IDs that have a `session_start` event recorded.
pub fn get_started_session_ids() -> Result<std::collections::HashSet<String>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT session_id FROM events WHERE event_type = 'session_start'")
        .context("preparing started sessions query")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .context("querying started sessions")?
        .collect::<Result<std::collections::HashSet<_>, _>>()
        .context("reading started session ids")?;
    Ok(ids)
}

/// Returns the set of session IDs that have a `session_end` event recorded.
pub fn get_completed_session_ids() -> Result<std::collections::HashSet<String>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT session_id FROM events WHERE event_type = 'session_end'")
        .context("preparing completed sessions query")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .context("querying completed sessions")?
        .collect::<Result<std::collections::HashSet<_>, _>>()
        .context("reading completed session ids")?;
    Ok(ids)
}

/// Compute the total estimated spend in USD for sessions that ended in the given year-month.
///
/// Uses the LAST `token_usage` event per session (cumulative counts — not a sum of all events).
/// Returns `(total_usd, session_count)`.
pub fn get_monthly_spend_usd(year_month: &str) -> Result<(f64, usize)> {
    use crate::cost::compute_cost_usd;

    let conn = open_db()?;

    // Find sessions that ended this month.
    let completed_this_month: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT session_id FROM events \
             WHERE event_type = 'session_end' \
               AND strftime('%Y-%m', timestamp) = ?1",
        )?;
        let rows = stmt
            .query_map(params![year_month], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .context("reading completed sessions this month")?;
        rows
    };

    if completed_this_month.is_empty() {
        return Ok((0.0, 0));
    }

    let mut total_usd = 0.0;

    for session_id in &completed_this_month {
        // Get the last token_usage payload for this session.
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM events \
                 WHERE session_id = ?1 AND event_type = 'token_usage' \
                 ORDER BY id DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .ok();

        let Some(payload) = payload else { continue };

        let Ok(event) = serde_json::from_str::<crate::event::Event>(&payload) else { continue };

        if let crate::event::Event::TokenUsage {
            model,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            ..
        } = event
        {
            total_usd += compute_cost_usd(
                &model,
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            );
        }
    }

    Ok((total_usd, completed_this_month.len()))
}

/// Upsert project/timing/model/summary metadata for a session into the `sessions` table.
///
/// Uses COALESCE so partial updates don't overwrite existing values with NULL.
pub fn upsert_session_meta(
    session_id: &str,
    project_path: Option<&str>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    model: Option<&str>,
    source: Option<&str>,
    title: Option<&str>,
    summary: Option<&str>,
    last_prompt: Option<&str>,
) -> Result<()> {
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO sessions (session_id, project_path, started_at, ended_at, model, source, title, summary, last_prompt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(session_id) DO UPDATE SET
             project_path = COALESCE(excluded.project_path, sessions.project_path),
             started_at   = COALESCE(excluded.started_at,   sessions.started_at),
             ended_at     = COALESCE(excluded.ended_at,     sessions.ended_at),
             model        = COALESCE(excluded.model,        sessions.model),
             source       = COALESCE(excluded.source,       sessions.source),
             title        = COALESCE(excluded.title,        sessions.title),
             summary      = COALESCE(excluded.summary,      sessions.summary),
             last_prompt  = COALESCE(excluded.last_prompt,  sessions.last_prompt)",
        params![
            session_id,
            project_path,
            started_at.map(|t| t.to_rfc3339()),
            ended_at.map(|t| t.to_rfc3339()),
            model,
            source,
            title,
            summary,
            last_prompt,
        ],
    )
    .context("upserting session metadata")?;
    Ok(())
}

/// Returns (earliest_timestamp, latest_timestamp, total_session_count) from the DB.
/// Returns Ok(None) if the DB is empty or does not exist yet.
pub fn get_db_summary() -> Result<Option<(DateTime<Utc>, DateTime<Utc>, usize)>> {
    let path = db_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let conn = open_db()?;
    let row: Option<(String, String, usize)> = conn
        .query_row(
            "SELECT MIN(timestamp), MAX(timestamp), COUNT(DISTINCT session_id) FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();
    let Some((min_ts, max_ts, count)) = row else {
        return Ok(None);
    };
    if count == 0 {
        return Ok(None);
    }
    let earliest = DateTime::parse_from_rfc3339(&min_ts)
        .context("parsing earliest timestamp")?
        .with_timezone(&Utc);
    let latest = DateTime::parse_from_rfc3339(&max_ts)
        .context("parsing latest timestamp")?
        .with_timezone(&Utc);
    Ok(Some((earliest, latest, count)))
}

/// Atomically replace all events for a session with a new set.
///
/// Deletes existing rows and inserts the new events in a single transaction, so a crash cannot
/// leave the session in a half-written state. Also removes the JSONL backup file (best-effort).
pub fn replace_session(
    session_id: &str,
    events: &[(chrono::DateTime<Utc>, crate::event::Event)],
) -> Result<()> {
    let path = db_path()?;
    let conn = Connection::open(&path)
        .with_context(|| format!("opening SQLite db at {}", path.display()))?;

    conn.execute_batch("BEGIN;")?;

    conn.execute("DELETE FROM events WHERE session_id = ?1", params![session_id])
        .context("deleting existing events in transaction")?;

    for (ts, event) in events {
        let payload = serde_json::to_string(event).context("serialising event")?;
        let ts_str = ts.to_rfc3339();
        let event_type = event.event_type_label();
        conn.execute(
            "INSERT INTO events (session_id, timestamp, event_type, payload) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, ts_str, event_type, payload],
        )
        .context("inserting event in transaction")?;
    }

    conn.execute_batch("COMMIT;")?;

    // Best-effort: remove the JSONL backup file if it exists.
    if let Ok(p) = jsonl_path(session_id) {
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    Ok(())
}

/// Deletes all events for the given session from the DB and removes the JSONL backup if present.
pub fn delete_events_for_session(session_id: &str) -> Result<()> {
    let conn = open_db()?;
    conn.execute(
        "DELETE FROM events WHERE session_id = ?1",
        params![session_id],
    )
    .context("deleting events for session")?;

    // Best-effort: remove the JSONL backup file if it exists.
    if let Ok(path) = jsonl_path(session_id) {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }

    Ok(())
}

/// Returns a list of all session_ids with their event counts.
pub fn get_sessions() -> Result<Vec<(String, usize)>> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT session_id, COUNT(*) as event_count FROM events GROUP BY session_id ORDER BY session_id ASC")
        .context("preparing session query")?;

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })
        .context("querying sessions")?
        .collect::<Result<Vec<_>, _>>()
        .context("reading session rows")?;

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HOME_LOCK;
    use tempfile::TempDir;

    /// Run `f` with $HOME temporarily set to `tmp`.
    fn with_home<F: FnOnce() -> Result<()>>(tmp: &TempDir, f: F) -> Result<()> {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let result = f();
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn insert_and_read_round_trip() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            let session_id = "test_session_roundtrip";
            let event = Event::ToolUse {
                tool_name: "bash".to_string(),
                status: "success".to_string(),
                duration_ms: Some(100),
                error: None,
            };
            insert_event(session_id, &event, Utc::now())?;
            let events = get_events(Some(session_id))?;
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].0, session_id);
            match &events[0].2 {
                Event::ToolUse { tool_name, status, duration_ms, error } => {
                    assert_eq!(tool_name, "bash");
                    assert_eq!(status, "success");
                    assert_eq!(*duration_ms, Some(100));
                    assert!(error.is_none());
                }
                _ => panic!("wrong variant"),
            }
            Ok(())
        })
    }

    #[test]
    fn insert_multiple_events() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            let session_id = "test_session_multi";
            let events_to_insert: Vec<Event> = vec![
                Event::SessionStart {
                    model: "claude-sonnet-4-6".to_string(),
                    source: "claude-code".to_string(),
                },
                Event::ToolUse {
                    tool_name: "grep".to_string(),
                    status: "success".to_string(),
                    duration_ms: Some(5),
                    error: None,
                },
                Event::SessionEnd,
            ];
            let ts = Utc::now();
            for e in &events_to_insert {
                insert_event(session_id, e, ts)?;
            }
            let retrieved = get_events(Some(session_id))?;
            assert_eq!(retrieved.len(), 3);
            assert!(matches!(retrieved[0].2, Event::SessionStart { .. }));
            assert!(matches!(retrieved[1].2, Event::ToolUse { .. }));
            assert!(matches!(retrieved[2].2, Event::SessionEnd));
            Ok(())
        })
    }

    #[test]
    fn jsonl_file_is_written() -> Result<()> {
        let tmp = TempDir::new()?;
        let tmp_path = tmp.path().to_path_buf();
        with_home(&tmp, || {
            let session_id = "test_session_jsonl";
            insert_event(session_id, &Event::SessionEnd, Utc::now())?;
            let jsonl = tmp_path
                .join(".trakr")
                .join("sessions")
                .join(format!("{}.jsonl", session_id));
            assert!(jsonl.exists(), "JSONL file should exist");
            let contents = std::fs::read_to_string(&jsonl)?;
            assert!(!contents.trim().is_empty());
            for line in contents.lines() {
                serde_json::from_str::<serde_json::Value>(line)
                    .with_context(|| format!("invalid JSON line: {}", line))?;
            }
            Ok(())
        })
    }

    #[test]
    fn get_events_no_filter_returns_all() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            insert_event("session_a", &Event::SessionEnd, Utc::now())?;
            insert_event("session_b", &Event::SessionStart {
                model: "claude-opus-4".to_string(),
                source: "test".to_string(),
            }, Utc::now())?;
            insert_event("session_a", &Event::ToolUse {
                tool_name: "bash".to_string(),
                status: "success".to_string(),
                duration_ms: None,
                error: None,
            }, Utc::now())?;

            let all = get_events(None)?;
            assert_eq!(all.len(), 3);

            let for_a = get_events(Some("session_a"))?;
            assert_eq!(for_a.len(), 2);
            assert!(for_a.iter().all(|(sid, _, _)| sid == "session_a"));

            let for_b = get_events(Some("session_b"))?;
            assert_eq!(for_b.len(), 1);
            Ok(())
        })
    }

    #[test]
    fn get_sessions_returns_counts() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            insert_event("sess1", &Event::SessionEnd, Utc::now())?;
            insert_event("sess1", &Event::SessionEnd, Utc::now())?;
            insert_event("sess2", &Event::SessionEnd, Utc::now())?;

            let sessions = get_sessions()?;
            assert_eq!(sessions.len(), 2);
            // Ordered by session_id ascending: sess1, sess2
            assert_eq!(sessions[0].0, "sess1");
            assert_eq!(sessions[0].1, 2);
            assert_eq!(sessions[1].0, "sess2");
            assert_eq!(sessions[1].1, 1);
            Ok(())
        })
    }

    #[test]
    fn unified_db_path() -> Result<()> {
        let tmp = TempDir::new()?;
        let tmp_path = tmp.path().to_path_buf();
        with_home(&tmp, || {
            insert_event("any_session", &Event::SessionEnd, Utc::now())?;
            let expected_db = tmp_path.join(".trakr").join("trakr.db");
            assert!(expected_db.exists(), "unified DB should exist at {:?}", expected_db);
            Ok(())
        })
    }
}
