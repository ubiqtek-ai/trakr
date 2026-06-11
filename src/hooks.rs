use anyhow::{Context, Result};
use chrono::Utc;
use std::io::{self, Read};

use crate::backfill;
use crate::event::Event;
use crate::storage::{self, insert_event};

/// Read all of stdin into a string.
fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
}

/// Extract `session_id` from the parsed JSON input.
fn session_id_from_input(input: &serde_json::Value) -> Result<String> {
    input
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("missing 'session_id' in hook input")
}

/// Handle the `tool-use` hook.
///
/// Tool-use data is now captured from the full session log at SessionEnd.
/// This handler remains registered for backward compatibility but writes nothing.
pub fn handle_tool_use() -> Result<()> {
    let _ = read_stdin(); // drain stdin so the pipe doesn't block Claude
    Ok(())
}

/// Handle the `session-start` hook.
///
/// Expected stdin JSON fields:
/// - `session_id` (required)
/// - `model` (optional)
/// - `source` (optional)
pub fn handle_session_start() -> Result<()> {
    let raw = read_stdin()?;
    let input: serde_json::Value =
        serde_json::from_str(&raw).context("parsing hook input JSON")?;

    let session_id = session_id_from_input(&input)?;

    let model = input
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let source = input
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-code")
        .to_string();

    let event = Event::SessionStart { model, source };
    insert_event(&session_id, &event, Utc::now())?;
    Ok(())
}

/// Handle the `session-end` hook.
///
/// Parses the full session transcript log and writes it atomically via `replace_session`,
/// giving accurate summed token counts across all turns. Falls back to a minimal
/// `session_end` event if the transcript is unavailable.
///
/// Expected stdin JSON fields:
/// - `session_id` (required)
/// - `transcript_path` (optional) — full JSONL session log to parse
/// - `cwd` (optional) — project directory, stored in the sessions table
pub fn handle_session_end() -> Result<()> {
    let raw = read_stdin()?;
    let input: serde_json::Value =
        serde_json::from_str(&raw).context("parsing hook input JSON")?;

    let session_id = session_id_from_input(&input)?;
    let project_path = input.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string());

    if let Some(transcript_path) = input.get("transcript_path").and_then(|v| v.as_str()) {
        match backfill::parse_session_log(std::path::Path::new(transcript_path)) {
            Ok(Some(session)) => {
                let started_at = session.events.first().map(|(ts, _)| *ts);
                let ended_at   = session.events.last().map(|(ts, _)| *ts);
                let model = session.events.iter().find_map(|(_, e)| {
                    if let Event::SessionStart { model, .. } = e {
                        if model != "unknown" { Some(model.clone()) } else { None }
                    } else {
                        None
                    }
                });

                storage::replace_session(&session_id, &session.events)?;
                if let Err(e) = storage::upsert_session_meta(
                    &session_id,
                    project_path.as_deref(),
                    started_at,
                    ended_at,
                    model.as_deref(),
                    Some("hook"),
                    session.title.as_deref(),
                    session.summary.as_deref(),
                    session.last_prompt.as_deref(),
                ) {
                    eprintln!("trakr: failed to write session metadata: {}", e);
                }
                if let Err(e) = storage::archive_transcript(&session_id, &session.source_path) {
                    eprintln!("trakr: failed to archive transcript: {}", e);
                }
                return Ok(());
            }
            Ok(None) => {
                eprintln!("trakr: transcript empty or no sessionId found — writing minimal session_end");
            }
            Err(e) => {
                eprintln!("trakr: failed to parse transcript: {} — writing minimal session_end", e);
            }
        }
    }

    // Fallback: record a bare session_end so the session isn't left open.
    insert_event(&session_id, &Event::SessionEnd, Utc::now())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{get_events, insert_event};
    use tempfile::TempDir;

    /// Parse a tool-use JSON payload and create the event manually (mirrors handle_tool_use logic).
    fn parse_tool_use(json: &str) -> Result<(String, Event)> {
        let input: serde_json::Value =
            serde_json::from_str(json).context("parsing JSON")?;
        let session_id = session_id_from_input(&input)?;
        let tool_name = input
            .get("tool_name")
            .or_else(|| input.get("tool"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let status = input
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let duration_ms = input.get("duration_ms").and_then(|v| v.as_u64());
        let error = input
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok((
            session_id,
            Event::ToolUse {
                tool_name,
                status,
                duration_ms,
                error,
            },
        ))
    }

    fn parse_session_start(json: &str) -> Result<(String, Event)> {
        let input: serde_json::Value = serde_json::from_str(json)?;
        let session_id = session_id_from_input(&input)?;
        let model = input
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let source = input
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("claude-code")
            .to_string();
        Ok((session_id, Event::SessionStart { model, source }))
    }

    fn parse_session_end(json: &str) -> Result<(String, Event)> {
        let input: serde_json::Value = serde_json::from_str(json)?;
        let session_id = session_id_from_input(&input)?;
        Ok((session_id, Event::SessionEnd))
    }

    #[test]
    fn parse_tool_use_full() -> Result<()> {
        let json = r#"{
            "session_id": "abc123",
            "tool_name": "bash",
            "status": "success",
            "duration_ms": 123,
            "error": null
        }"#;
        let (sid, event) = parse_tool_use(json)?;
        assert_eq!(sid, "abc123");
        match event {
            Event::ToolUse { tool_name, status, duration_ms, error } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(status, "success");
                assert_eq!(duration_ms, Some(123));
                assert!(error.is_none());
            }
            _ => panic!("wrong variant"),
        }
        Ok(())
    }

    #[test]
    fn parse_tool_use_with_error() -> Result<()> {
        let json = r#"{
            "session_id": "sid1",
            "tool_name": "grep",
            "status": "error",
            "duration_ms": 5,
            "error": "command not found"
        }"#;
        let (_, event) = parse_tool_use(json)?;
        match event {
            Event::ToolUse { error, status, .. } => {
                assert_eq!(status, "error");
                assert_eq!(error.as_deref(), Some("command not found"));
            }
            _ => panic!("wrong variant"),
        }
        Ok(())
    }

    #[test]
    fn parse_tool_use_minimal() -> Result<()> {
        // Only session_id and tool_name; all optional fields absent.
        let json = r#"{"session_id": "s1", "tool_name": "read"}"#;
        let (_, event) = parse_tool_use(json)?;
        match event {
            Event::ToolUse { tool_name, status, duration_ms, error } => {
                assert_eq!(tool_name, "read");
                assert_eq!(status, "unknown");
                assert!(duration_ms.is_none());
                assert!(error.is_none());
            }
            _ => panic!("wrong variant"),
        }
        Ok(())
    }

    #[test]
    fn parse_session_start_full() -> Result<()> {
        let json = r#"{
            "session_id": "sess42",
            "model": "claude-opus-4",
            "source": "claude-code"
        }"#;
        let (sid, event) = parse_session_start(json)?;
        assert_eq!(sid, "sess42");
        match event {
            Event::SessionStart { model, source } => {
                assert_eq!(model, "claude-opus-4");
                assert_eq!(source, "claude-code");
            }
            _ => panic!("wrong variant"),
        }
        Ok(())
    }

    #[test]
    fn parse_session_end_minimal() -> Result<()> {
        let json = r#"{"session_id": "end_sess"}"#;
        let (sid, event) = parse_session_end(json)?;
        assert_eq!(sid, "end_sess");
        assert!(matches!(event, Event::SessionEnd));
        Ok(())
    }

    #[test]
    fn missing_session_id_returns_error() {
        let json = r#"{"tool_name": "bash", "status": "ok"}"#;
        let result = parse_tool_use(json);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("session_id"), "error message: {}", msg);
    }

    #[test]
    fn tool_use_storage_integration() -> Result<()> {
        use crate::test_support::HOME_LOCK;

        let tmp = TempDir::new()?;
        let _guard = HOME_LOCK.lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());

        let result = (|| -> Result<()> {
            let session_id = "hook_integration_test";
            let event = Event::ToolUse {
                tool_name: "write".to_string(),
                status: "success".to_string(),
                duration_ms: Some(7),
                error: None,
            };
            insert_event(session_id, &event, Utc::now())?;
            let events = get_events(Some(session_id))?;
            assert_eq!(events.len(), 1);
            Ok(())
        })();

        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn handle_tool_use_is_noop() -> Result<()> {
        use crate::test_support::HOME_LOCK;

        let tmp = TempDir::new()?;
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());

        let result = (|| -> Result<()> {
            // handle_tool_use drains stdin and writes nothing.
            insert_event("noop_session", &Event::SessionStart {
                model: "m".to_string(), source: "test".to_string(),
            }, Utc::now())?;
            let events = get_events(Some("noop_session"))?;
            assert_eq!(events.len(), 1, "handle_tool_use should not add events");
            Ok(())
        })();

        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }
}
