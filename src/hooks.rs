use anyhow::{Context, Result};
use chrono::Utc;
use std::io::{self, Read};

use crate::event::Event;
use crate::storage::insert_event;
use crate::transcript;

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
/// Expected stdin JSON fields:
/// - `session_id` (required)
/// - `tool_name` (required)
/// - `status` (required, e.g. "success" / "error")
/// - `duration_ms` (optional, integer)
/// - `error` (optional, string)
pub fn handle_tool_use() -> Result<()> {
    let raw = read_stdin()?;
    let input: serde_json::Value =
        serde_json::from_str(&raw).context("parsing hook input JSON")?;

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

    let duration_ms = input
        .get("duration_ms")
        .and_then(|v| v.as_u64());

    let error = input
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let event = Event::ToolUse {
        tool_name,
        status,
        duration_ms,
        error,
    };

    insert_event(&session_id, &event, Utc::now())?;

    // Read transcript to capture token usage alongside this tool-use event.
    if let Some(transcript_path) = input.get("transcript_path").and_then(|v| v.as_str()) {
        match transcript::parse_transcript(transcript_path) {
            Ok(Some(usage)) => {
                let total_tokens = usage.input_tokens
                    + usage.output_tokens
                    + usage.cache_creation_input_tokens
                    + usage.cache_read_input_tokens;
                let token_event = Event::TokenUsage {
                    model: usage.model,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_creation_input_tokens: usage.cache_creation_input_tokens,
                    cache_read_input_tokens: usage.cache_read_input_tokens,
                    total_tokens,
                };
                if let Err(e) = insert_event(&session_id, &token_event, Utc::now()) {
                    eprintln!("ctx-trakr: failed to insert token usage event: {}", e);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("ctx-trakr: failed to parse transcript for token usage: {}", e);
            }
        }
    }

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
/// Expected stdin JSON fields:
/// - `session_id` (required)
/// - `transcript_path` (optional) — parsed to capture final token usage
pub fn handle_session_end() -> Result<()> {
    let raw = read_stdin()?;
    let input: serde_json::Value =
        serde_json::from_str(&raw).context("parsing hook input JSON")?;

    let session_id = session_id_from_input(&input)?;
    insert_event(&session_id, &Event::SessionEnd, Utc::now())?;

    if let Some(transcript_path) = input.get("transcript_path").and_then(|v| v.as_str()) {
        match transcript::parse_transcript(transcript_path) {
            Ok(Some(usage)) => {
                let total_tokens = usage.input_tokens
                    + usage.output_tokens
                    + usage.cache_creation_input_tokens
                    + usage.cache_read_input_tokens;
                let token_event = Event::TokenUsage {
                    model: usage.model,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_creation_input_tokens: usage.cache_creation_input_tokens,
                    cache_read_input_tokens: usage.cache_read_input_tokens,
                    total_tokens,
                };
                if let Err(e) = insert_event(&session_id, &token_event, Utc::now()) {
                    eprintln!("ctx-trakr: failed to insert token usage event: {}", e);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("ctx-trakr: failed to parse transcript for token usage: {}", e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::get_events;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

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

    /// Mirror the tool_use + transcript token-tracking logic from handle_tool_use(),
    /// exercising it directly without stdin.
    fn run_tool_use_with_transcript(
        session_id: &str,
        transcript_path: &str,
    ) -> Result<()> {
        let event = Event::ToolUse {
            tool_name: "bash".to_string(),
            status: "success".to_string(),
            duration_ms: Some(10),
            error: None,
        };
        insert_event(session_id, &event, Utc::now())?;

        if let Ok(Some(usage)) = transcript::parse_transcript(transcript_path) {
            let total_tokens = usage.input_tokens
                + usage.output_tokens
                + usage.cache_creation_input_tokens
                + usage.cache_read_input_tokens;
            let token_event = Event::TokenUsage {
                model: usage.model,
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
                total_tokens,
            };
            insert_event(session_id, &token_event, Utc::now())?;
        }

        Ok(())
    }

    #[test]
    fn token_usage_inserted_alongside_tool_use() -> Result<()> {
        use crate::test_support::HOME_LOCK;

        // Write a mock transcript file.
        let transcript_content = r#"{"type":"user","content":"hi"}
{"type":"assistant","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":300,"output_tokens":120,"cache_creation_input_tokens":2000,"cache_read_input_tokens":800}}}
"#;
        let mut transcript_file = NamedTempFile::new()?;
        transcript_file.write_all(transcript_content.as_bytes())?;
        let transcript_path = transcript_file.path().to_str().unwrap().to_string();

        let tmp = TempDir::new()?;
        let _guard = HOME_LOCK.lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());

        let result = (|| -> Result<()> {
            let session_id = "token_tracking_test";
            run_tool_use_with_transcript(session_id, &transcript_path)?;

            let events = get_events(Some(session_id))?;
            // Expect: 1 ToolUse + 1 TokenUsage
            assert_eq!(events.len(), 2, "expected ToolUse + TokenUsage events");

            let token_event = events
                .iter()
                .find(|(_, _, e)| matches!(e, Event::TokenUsage { .. }))
                .map(|(_, _, e)| e)
                .expect("TokenUsage event should be present");

            match token_event {
                Event::TokenUsage {
                    model,
                    input_tokens,
                    output_tokens,
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
                    total_tokens,
                } => {
                    assert_eq!(model, "claude-sonnet-4-6");
                    assert_eq!(*input_tokens, 300);
                    assert_eq!(*output_tokens, 120);
                    assert_eq!(*cache_creation_input_tokens, 2000);
                    assert_eq!(*cache_read_input_tokens, 800);
                    assert_eq!(*total_tokens, 3220);
                }
                _ => panic!("wrong variant"),
            }

            Ok(())
        })();

        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn no_token_event_when_transcript_absent() -> Result<()> {
        use crate::test_support::HOME_LOCK;

        let tmp = TempDir::new()?;
        let _guard = HOME_LOCK.lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());

        let result = (|| -> Result<()> {
            let session_id = "no_transcript_test";
            run_tool_use_with_transcript(session_id, "/nonexistent/transcript.jsonl")?;

            let events = get_events(Some(session_id))?;
            // Only the ToolUse event; no TokenUsage because transcript doesn't exist.
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0].2, Event::ToolUse { .. }));
            Ok(())
        })();

        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }
}
