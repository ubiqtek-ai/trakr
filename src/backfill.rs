use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use std::path::{Path, PathBuf};

use crate::event::Event;
use crate::storage;

/// A session reconstructed from a Claude Code native session log.
pub struct BackfilledSession {
    pub session_id: String,
    pub project_path: Option<String>,
    pub events: Vec<(DateTime<Utc>, Event)>,
}

/// Outcome of attempting to backfill a single session.
pub enum BackfillAction {
    /// A `session_end` was already present in the DB — nothing changed.
    Skipped,
    /// No prior data existed; all events were inserted.
    Inserted,
    /// Partial data existed (no `session_end`); it was deleted and re-inserted.
    Replaced,
}

/// Walk `projects_dir` and collect `.jsonl` files at depth 1 only.
///
/// - `project_filter`: if Some, only include files whose parent directory name contains the
///   substring (case-sensitive).
/// - `since`: if Some, skip files whose last-modified time is before that date.
pub fn discover_sessions(
    projects_dir: &Path,
    project_filter: Option<&str>,
    since: Option<NaiveDate>,
) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = Vec::new();

    let dir_entries = std::fs::read_dir(projects_dir)
        .with_context(|| format!("reading projects dir {}", projects_dir.display()))?;

    for entry in dir_entries {
        let entry = entry.context("reading directory entry")?;
        let project_dir = entry.path();

        if !project_dir.is_dir() {
            continue;
        }

        let dir_name = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if let Some(filter) = project_filter {
            if !dir_name.contains(filter) {
                continue;
            }
        }

        let sub_entries = match std::fs::read_dir(&project_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for sub in sub_entries {
            let sub = match sub {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = sub.path();

            // Depth 1 only — skip subdirectories.
            if !path.is_file() {
                continue;
            }

            if path.extension().map_or(true, |e| e != "jsonl") {
                continue;
            }

            if let Some(cutoff) = since {
                let mtime = path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        let dt: DateTime<Utc> = t.into();
                        dt.date_naive()
                    });

                if let Some(date) = mtime {
                    if date < cutoff {
                        continue;
                    }
                }
            }

            paths.push(path);
        }
    }

    paths.sort();
    Ok(paths)
}

/// Parse a Claude Code native session log and reconstruct a `BackfilledSession`.
///
/// Returns `Ok(None)` if the file is empty or no `sessionId` is found.
/// Malformed JSON lines are silently skipped.
pub fn parse_session_log(path: &Path) -> Result<Option<BackfilledSession>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading session log {}", path.display()))?;

    let lines: Vec<serde_json::Value> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    if lines.is_empty() {
        return Ok(None);
    }

    // Extract sessionId from the first line that has it.
    let session_id = lines
        .iter()
        .find_map(|entry| {
            entry.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string())
        });

    let Some(session_id) = session_id else {
        return Ok(None);
    };

    // Extract cwd from any log entry; fall back to the encoded project dir name from the path.
    let project_path: Option<String> = lines
        .iter()
        .find_map(|entry| entry.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        });

    // Determine the first entry's timestamp (used for SessionStart).
    let first_ts = parse_timestamp(lines.first().unwrap());

    // Collect events.
    let mut events: Vec<(DateTime<Utc>, Event)> = Vec::new();

    // Accumulated token totals across all assistant entries.
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_creation: u64 = 0;
    let mut total_cache_read: u64 = 0;

    // Model from the first assistant entry that has one.
    let mut model: Option<String> = None;

    let mut last_ts = first_ts;

    // Tool-use events collected during the pass; we emit them after SessionStart.
    let mut tool_use_events: Vec<(DateTime<Utc>, Event)> = Vec::new();

    for entry in &lines {
        let ts = parse_timestamp(entry);
        last_ts = ts;

        if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }

        let message = match entry.get("message") {
            Some(m) => m,
            None => continue,
        };

        // Capture model from the first assistant entry that provides it.
        if model.is_none() {
            if let Some(m) = message.get("model").and_then(|v| v.as_str()) {
                if !m.is_empty() {
                    model = Some(m.to_string());
                }
            }
        }

        // Accumulate token usage.
        if let Some(usage) = message.get("usage") {
            total_input += usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            total_output += usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            total_cache_creation += usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            total_cache_read += usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        }

        // Extract tool_use blocks from message.content[].
        if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
            for block in content {
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    let tool_name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    tool_use_events.push((
                        ts,
                        Event::ToolUse {
                            tool_name,
                            status: "unknown".to_string(),
                            duration_ms: None,
                            error: None,
                        },
                    ));
                }
            }
        }
    }

    let final_model = model.unwrap_or_else(|| "unknown".to_string());

    // SessionStart at the first entry's timestamp.
    events.push((
        first_ts,
        Event::SessionStart {
            model: final_model.clone(),
            source: "backfill".to_string(),
        },
    ));

    // Tool uses in order they were encountered.
    events.extend(tool_use_events);

    // TokenUsage and SessionEnd at the last entry's timestamp.
    let total_tokens = total_input + total_output + total_cache_creation + total_cache_read;
    events.push((
        last_ts,
        Event::TokenUsage {
            model: final_model,
            input_tokens: total_input,
            output_tokens: total_output,
            cache_creation_input_tokens: total_cache_creation,
            cache_read_input_tokens: total_cache_read,
            total_tokens,
        },
    ));
    events.push((last_ts, Event::SessionEnd));

    Ok(Some(BackfilledSession { session_id, project_path, events }))
}

/// Decide whether to skip, insert, or replace a session, then act accordingly.
///
/// A session is skipped only if it is fully tracked (has both session_start and session_end).
/// Partial sessions (e.g. hooks installed mid-session) are replaced with the full log-derived stream.
/// The delete+insert is wrapped in a transaction so a crash cannot leave the DB in a partial state.
pub fn backfill_session(session: &BackfilledSession, dry_run: bool) -> Result<BackfillAction> {
    storage::init_db()?;

    let started = storage::get_started_session_ids()?;
    let completed = storage::get_completed_session_ids()?;

    if started.contains(&session.session_id) && completed.contains(&session.session_id) {
        return Ok(BackfillAction::Skipped);
    }

    let existing = storage::get_events(Some(&session.session_id))?;
    let action = if existing.is_empty() {
        BackfillAction::Inserted
    } else {
        BackfillAction::Replaced
    };

    if !dry_run {
        storage::replace_session(&session.session_id, &session.events)?;
        storage::upsert_session_meta(
            &session.session_id,
            session.project_path.as_deref(),
            session.events.first().map(|(ts, _)| *ts),
            session.events.last().map(|(ts, _)| *ts),
            session.events.iter().find_map(|(_, e)| {
                if let Event::SessionStart { model, .. } = e {
                    if model != "unknown" { Some(model.as_str()) } else { None }
                } else {
                    None
                }
            }),
            Some("backfill"),
        )?;
    }

    Ok(action)
}

/// How well a Claude log session is represented in the ctx-trakr DB.
#[derive(Debug, PartialEq)]
pub enum TrackingStatus {
    /// No events in the DB for this session.
    Missing,
    /// Some events exist but the session lacks either a `session_start` or `session_end` —
    /// e.g. hooks were installed mid-session, or the session ended abnormally.
    Partial,
    /// Has both `session_start` and `session_end` — fully tracked.
    Complete,
}

/// A summary of one session as seen in Claude Code's native logs.
pub struct SessionSummary {
    pub session_id: String,
    pub project: String,
    pub first_ts: Option<DateTime<Utc>>,
    pub last_ts: Option<DateTime<Utc>>,
    pub assistant_turns: usize,
    pub tool_uses: usize,
    pub model: Option<String>,
    pub tracking: TrackingStatus,
}

/// Read all Claude Code native session logs under `projects_dir` and return a summary of each.
///
/// Does not write anything. `project_filter` and `since` work the same as in `discover_sessions`.
pub fn inspect_logs(
    projects_dir: &Path,
    project_filter: Option<&str>,
    since: Option<NaiveDate>,
) -> Result<Vec<SessionSummary>> {
    let completed = storage::get_completed_session_ids().unwrap_or_default();
    let started = storage::get_started_session_ids().unwrap_or_default();
    let db_session_ids: std::collections::HashSet<String> = storage::get_sessions()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    let paths = discover_sessions(projects_dir, project_filter, since)?;
    let mut summaries = Vec::new();

    for path in &paths {
        let project = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut session_id: Option<String> = None;
        let mut first_ts: Option<DateTime<Utc>> = None;
        let mut last_ts: Option<DateTime<Utc>> = None;
        let mut assistant_turns = 0usize;
        let mut tool_uses = 0usize;
        let mut model: Option<String> = None;

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };

            if session_id.is_none() {
                session_id = entry.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string());
            }

            let ts = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            if let Some(t) = ts {
                if first_ts.is_none() {
                    first_ts = Some(t);
                }
                last_ts = Some(t);
            }

            if entry.get("type").and_then(|v| v.as_str()) == Some("assistant") {
                assistant_turns += 1;
                if let Some(msg) = entry.get("message") {
                    if model.is_none() {
                        if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                            if !m.is_empty() {
                                model = Some(m.to_string());
                            }
                        }
                    }
                    if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
                        tool_uses += content
                            .iter()
                            .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                            .count();
                    }
                }
            }
        }

        let sid = match session_id {
            Some(s) => s,
            None => continue,
        };

        let tracking = if !db_session_ids.contains(&sid) {
            TrackingStatus::Missing
        } else if started.contains(&sid) && completed.contains(&sid) {
            TrackingStatus::Complete
        } else {
            TrackingStatus::Partial
        };

        summaries.push(SessionSummary {
            session_id: sid,
            project,
            first_ts,
            last_ts,
            assistant_turns,
            tool_uses,
            model,
            tracking,
        });
    }

    // Sort by first timestamp ascending, unknowns last.
    summaries.sort_by(|a, b| match (a.first_ts, b.first_ts) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    Ok(summaries)
}

/// Parse an ISO 8601 timestamp from a JSON entry's `timestamp` field.
/// Falls back to `Utc::now()` if missing or unparseable.
fn parse_timestamp(entry: &serde_json::Value) -> DateTime<Utc> {
    entry
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HOME_LOCK;
    use std::io::Write;
    use tempfile::TempDir;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn with_home<F: FnOnce() -> Result<()>>(tmp: &TempDir, f: F) -> Result<()> {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let result = f();
        match old {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    fn write_jsonl(tmp_dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = tmp_dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    // ── parse_session_log tests ───────────────────────────────────────────────

    #[test]
    fn test_parse_empty_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = write_jsonl(&tmp, "empty.jsonl", "");
        let result = parse_session_log(&path)?;
        assert!(result.is_none(), "empty file should return None");
        Ok(())
    }

    #[test]
    fn test_parse_no_session_id() -> Result<()> {
        let tmp = TempDir::new()?;
        let content = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;
        let path = write_jsonl(&tmp, "no_sid.jsonl", content);
        let result = parse_session_log(&path)?;
        assert!(result.is_none(), "missing sessionId should return None");
        Ok(())
    }

    #[test]
    fn test_parse_tool_uses_extracted() -> Result<()> {
        let tmp = TempDir::new()?;
        let content = r#"{"type":"system","sessionId":"abc123","timestamp":"2026-01-01T10:00:00Z"}
{"type":"assistant","sessionId":"abc123","timestamp":"2026-01-01T10:01:00Z","message":{"model":"claude-sonnet-4-6","content":[{"type":"tool_use","name":"bash"},{"type":"tool_use","name":"read"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
        let path = write_jsonl(&tmp, "tools.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        assert_eq!(session.session_id, "abc123");

        let tool_uses: Vec<&Event> = session
            .events
            .iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, Event::ToolUse { .. }))
            .collect();

        assert_eq!(tool_uses.len(), 2, "expected 2 ToolUse events");

        let names: Vec<&str> = tool_uses
            .iter()
            .filter_map(|e| {
                if let Event::ToolUse { tool_name, .. } = e {
                    Some(tool_name.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read"));
        Ok(())
    }

    #[test]
    fn test_parse_token_usage_summed() -> Result<()> {
        let tmp = TempDir::new()?;
        // Three assistant entries — tokens should be summed.
        let content = r#"{"type":"system","sessionId":"sum1","timestamp":"2026-01-01T10:00:00Z"}
{"type":"assistant","sessionId":"sum1","timestamp":"2026-01-01T10:01:00Z","message":{"model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":2}}}
{"type":"assistant","sessionId":"sum1","timestamp":"2026-01-01T10:02:00Z","message":{"model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":200,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":50}}}
{"type":"assistant","sessionId":"sum1","timestamp":"2026-01-01T10:03:00Z","message":{"model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":50,"output_tokens":5,"cache_creation_input_tokens":3,"cache_read_input_tokens":1}}}
"#;
        let path = write_jsonl(&tmp, "sum.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        let token_usage = session
            .events
            .iter()
            .find_map(|(_, e)| if let Event::TokenUsage { .. } = e { Some(e) } else { None })
            .expect("should have TokenUsage");

        if let Event::TokenUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            total_tokens,
            ..
        } = token_usage
        {
            assert_eq!(*input_tokens, 350);
            assert_eq!(*output_tokens, 35);
            assert_eq!(*cache_creation_input_tokens, 8);
            assert_eq!(*cache_read_input_tokens, 53);
            assert_eq!(*total_tokens, 446);
        }
        Ok(())
    }

    #[test]
    fn test_parse_model_fallback() -> Result<()> {
        let tmp = TempDir::new()?;
        // No assistant entries at all — model should be "unknown".
        let content = r#"{"type":"system","sessionId":"nomodel","timestamp":"2026-01-01T10:00:00Z"}
{"type":"user","sessionId":"nomodel","timestamp":"2026-01-01T10:00:01Z","message":{"role":"user","content":"hi"}}
"#;
        let path = write_jsonl(&tmp, "nomodel.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        let start = session
            .events
            .iter()
            .find_map(|(_, e)| if let Event::SessionStart { .. } = e { Some(e) } else { None })
            .expect("should have SessionStart");

        if let Event::SessionStart { model, .. } = start {
            assert_eq!(model, "unknown");
        }
        Ok(())
    }

    // ── backfill_session tests ────────────────────────────────────────────────

    fn make_session(id: &str) -> BackfilledSession {
        let ts = Utc::now();
        BackfilledSession {
            session_id: id.to_string(),
            project_path: None,
            events: vec![
                (ts, Event::SessionStart { model: "test-model".to_string(), source: "backfill".to_string() }),
                (ts, Event::TokenUsage {
                    model: "test-model".to_string(),
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    total_tokens: 15,
                }),
                (ts, Event::SessionEnd),
            ],
        }
    }

    #[test]
    fn test_backfill_session_skip_when_complete() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            // A complete session requires BOTH session_start and session_end.
            storage::insert_event(
                "complete_session",
                &Event::SessionStart { model: "m".to_string(), source: "hook".to_string() },
                Utc::now(),
            )?;
            storage::insert_event("complete_session", &Event::SessionEnd, Utc::now())?;

            let session = make_session("complete_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Skipped));
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_replaced_when_only_session_end() -> Result<()> {
        // A session that has session_end but no session_start (hooks installed mid-session)
        // should be replaced, not skipped.
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            storage::insert_event("tail_only_session", &Event::SessionEnd, Utc::now())?;

            let session = make_session("tail_only_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Replaced));

            let events = storage::get_events(Some("tail_only_session"))?;
            assert_eq!(events.len(), 3, "should have full backfilled set");
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_inserted_when_new() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            let session = make_session("new_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Inserted));

            let events = storage::get_events(Some("new_session"))?;
            assert_eq!(events.len(), 3, "all events should be inserted");
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_replaced_when_partial() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            // Insert a partial session — has events but no session_end.
            storage::insert_event(
                "partial_session",
                &Event::SessionStart { model: "old".to_string(), source: "hook".to_string() },
                Utc::now(),
            )?;

            let session = make_session("partial_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Replaced));

            // Verify old event was replaced with the new full set.
            let events = storage::get_events(Some("partial_session"))?;
            assert_eq!(events.len(), 3, "should have 3 events after replace");
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_dry_run_no_write() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            let session = make_session("dry_run_session");
            let action = backfill_session(&session, true)?;
            // Should report Inserted (no prior data) but write nothing.
            assert!(matches!(action, BackfillAction::Inserted));

            let events = storage::get_events(Some("dry_run_session"))?;
            assert!(events.is_empty(), "dry_run should not write anything");
            Ok(())
        })
    }
}
