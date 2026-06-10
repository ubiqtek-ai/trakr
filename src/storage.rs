use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::event::Event;

/// Returns `~/.ctx-trakr/`.
fn base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".ctx-trakr"))
}

/// Returns the path to the unified DB: `~/.ctx-trakr/ctx-trakr.db`.
fn db_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("ctx-trakr.db"))
}

/// Returns the sessions directory: `~/.ctx-trakr/sessions/`.
fn sessions_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("sessions"))
}

fn jsonl_path(session_id: &str) -> Result<PathBuf> {
    Ok(sessions_dir()?.join(format!("{}.jsonl", session_id)))
}

fn open_db() -> Result<Connection> {
    let path = db_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating base dir {}", parent.display()))?;
    }
    let conn = Connection::open(&path)
        .with_context(|| format!("opening SQLite db at {}", path.display()))?;
    Ok(conn)
}

/// Create `~/.ctx-trakr/ctx-trakr.db` with the unified events table if it doesn't already exist.
/// Only needs to be called once at startup.
pub fn init_db() -> Result<()> {
    let dir = base_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating base dir {}", dir.display()))?;

    // Also ensure sessions/ exists for JSONL backups.
    let sessions = sessions_dir()?;
    fs::create_dir_all(&sessions)
        .with_context(|| format!("creating sessions dir {}", sessions.display()))?;

    let conn = open_db()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT    NOT NULL,
            timestamp  TEXT    NOT NULL,
            event_type TEXT    NOT NULL,
            payload    TEXT    NOT NULL
        );",
    )
    .context("creating events table")?;

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
        let _guard = HOME_LOCK.lock().unwrap();
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
                .join(".ctx-trakr")
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
            let expected_db = tmp_path.join(".ctx-trakr").join("ctx-trakr.db");
            assert!(expected_db.exists(), "unified DB should exist at {:?}", expected_db);
            Ok(())
        })
    }
}
