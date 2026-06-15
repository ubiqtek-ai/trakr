use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use std::path::{Path, PathBuf};

use crate::event::Event;
use crate::storage;

/// A session reconstructed from a Claude Code native session log.
pub struct BackfilledSession {
    pub session_id: String,
    pub project_path: Option<String>,
    /// The native Claude JSONL file this session was parsed from.
    pub source_path: PathBuf,
    pub events: Vec<(DateTime<Utc>, Event)>,
    pub title: Option<String>,
    /// Compact summary text from `isCompactSummary:true` user messages, truncated to 2000 chars.
    pub summary: Option<String>,
    pub last_prompt: Option<String>,
    /// Timestamp of the last event seen in the transcript (main + subagent files).
    ///
    /// Written to `sessions.last_activity_at` on every successful backfill. This is the basis
    /// for the "active session" display heuristic (within the last hour). It is distinct from
    /// `sessions.ended_at`, which only the SessionEnd hook may set.
    pub last_activity_at: DateTime<Utc>,
}

/// Outcome of attempting to backfill a single session.
pub enum BackfillAction {
    /// No prior data existed; all events were inserted.
    Inserted,
    /// Prior data existed; it was deleted and the new parse was inserted.
    ///
    /// B2: the old `Skipped` variant (which prevented re-parsing sessions that already had
    /// both `session_start` + `session_end`) is removed. Every session is now always
    /// re-parsed — idempotent via `replace_session`, and no synthetic `session_end` is written
    /// so re-parsing a live session is harmless.
    Replaced,
    /// Unused placeholder kept for forward compatibility — never returned.
    #[allow(dead_code)]
    Skipped,
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

/// Extract text from a Claude message's `content` field.
///
/// Handles both plain-string content and content arrays with `type:"text"` blocks.
fn extract_content_text(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

/// Truncate a string to at most `max_chars` Unicode scalar values.
fn truncate_chars(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// Accumulates per-model token counts, deduplicating by `message.id`.
///
/// One API response may be written as multiple `assistant` JSONL lines (one per content
/// block: thinking, text, tool_use) all repeating the same `usage` object. The first
/// occurrence wins; duplicates are silently dropped. Entries with no `message.id`
/// (rare; ~12 corpus-wide) are always counted — there is no key to dedupe on.
struct PerModelAccumulator {
    /// Total tokens per model name: (input, output, cache_creation, cache_read, cache_creation_1h).
    by_model: std::collections::HashMap<String, (u64, u64, u64, u64, u64)>,
    /// Set of `message.id` values already counted — prevents double-counting multi-block
    /// API responses.
    seen_message_ids: std::collections::HashSet<String>,
}

impl PerModelAccumulator {
    fn new() -> Self {
        Self {
            by_model: std::collections::HashMap::new(),
            seen_message_ids: std::collections::HashSet::new(),
        }
    }

    /// Attempt to accumulate usage from one `assistant` JSONL entry.
    ///
    /// Returns `true` if usage was counted, `false` if it was deduped.
    fn accumulate(&mut self, message: &serde_json::Value) -> bool {
        let model = message
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|m| !m.is_empty())
            .unwrap_or("unknown")
            .to_string();

        let msg_id = message.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());

        // Dedupe: if we have a message id and have already seen it, skip.
        if let Some(ref id) = msg_id {
            if !self.seen_message_ids.insert(id.clone()) {
                return false;
            }
        }

        if let Some(usage) = message.get("usage") {
            let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cache_creation = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // 1-hour TTL cache writes cost 2× input rate (vs 1.25× for 5-min).
            // The nested `cache_creation` object carries the per-tier split.
            let cache_creation_1h = usage
                .get("cache_creation")
                .and_then(|cc| cc.get("ephemeral_1h_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let entry = self.by_model.entry(model).or_insert((0, 0, 0, 0, 0));
            entry.0 += input;
            entry.1 += output;
            entry.2 += cache_creation;
            entry.3 += cache_read;
            entry.4 += cache_creation_1h;
        }

        true
    }

    /// Emit one `TokenUsage` event per distinct model, timestamped at `last_ts`.
    ///
    /// Returns the events and the model with the most output tokens (for `sessions.model`).
    fn into_events(self, last_ts: DateTime<Utc>) -> (Vec<(DateTime<Utc>, Event)>, Option<String>) {
        let mut usage_events = Vec::new();
        let mut dominant_model: Option<String> = None;
        let mut max_output: u64 = 0;

        for (model, (input, output, cache_creation, cache_read, cache_creation_1h)) in &self.by_model {
            let total = input + output + cache_creation + cache_read;
            usage_events.push((
                last_ts,
                Event::TokenUsage {
                    model: model.clone(),
                    input_tokens: *input,
                    output_tokens: *output,
                    cache_creation_input_tokens: *cache_creation,
                    cache_read_input_tokens: *cache_read,
                    cache_creation_1h_input_tokens: *cache_creation_1h,
                    total_tokens: total,
                },
            ));
            if *output > max_output {
                max_output = *output;
                dominant_model = Some(model.clone());
            }
        }

        // Sort by model name for deterministic output (useful in tests).
        usage_events.sort_by(|(_, a), (_, b)| {
            let model_a = if let Event::TokenUsage { model, .. } = a { model } else { "" };
            let model_b = if let Event::TokenUsage { model, .. } = b { model } else { "" };
            model_a.cmp(model_b)
        });

        (usage_events, dominant_model)
    }
}

/// Parse assistant entries from a slice of JSONL lines, accumulating into `acc` and
/// appending tool-use events to `tool_use_events`.
fn parse_assistant_entries(
    lines: &[serde_json::Value],
    acc: &mut PerModelAccumulator,
    tool_use_events: &mut Vec<(DateTime<Utc>, Event)>,
) {
    for entry in lines {
        if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(message) = entry.get("message") else { continue };
        let ts = parse_timestamp(entry);

        acc.accumulate(message);

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
}

/// Discover subagent JSONL files for a main session file.
///
/// Given a main file at `<dir>/<uuid>.jsonl`, looks for
/// `<dir>/<uuid>/subagents/agent-*.jsonl`.
///
/// Public alias: `discover_subagent_files_pub` for callers outside this module
/// (e.g. the reconciliation sweep in `main.rs`).
fn discover_subagent_files(main_path: &Path) -> Vec<PathBuf> {
    let Some(stem) = main_path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let Some(parent) = main_path.parent() else {
        return Vec::new();
    };
    let subagents_dir = parent.join(stem).join("subagents");
    if !subagents_dir.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().map_or(false, |ext| ext == "jsonl")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n.starts_with("agent-"))
        })
        .collect();
    paths.sort();
    paths
}

/// Public wrapper around `discover_subagent_files` for callers outside this module.
pub fn discover_subagent_files_pub(main_path: &Path) -> Vec<PathBuf> {
    discover_subagent_files(main_path)
}

/// Read and parse a JSONL file, returning only valid JSON lines.
fn read_jsonl_lines(path: &Path) -> Result<Vec<serde_json::Value>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading session log {}", path.display()))?;
    let lines = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ok(lines)
}

/// Parse a Claude Code native session log and reconstruct a `BackfilledSession`.
///
/// Also discovers and includes any sibling subagent files at
/// `<main_file_dir>/<session-uuid>/subagents/agent-*.jsonl`.
///
/// Returns `Ok(None)` if the file is empty or no `sessionId` is found.
/// Malformed JSON lines are silently skipped.
pub fn parse_session_log(path: &Path) -> Result<Option<BackfilledSession>> {
    let lines = read_jsonl_lines(path)?;

    if lines.is_empty() {
        return Ok(None);
    }

    let session_id = lines
        .iter()
        .find_map(|entry| {
            entry.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string())
        });

    let Some(session_id) = session_id else {
        return Ok(None);
    };

    let project_path: Option<String> = lines
        .iter()
        .find_map(|entry| entry.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        });

    let first_ts = parse_timestamp(lines.first().unwrap());

    let mut events: Vec<(DateTime<Utc>, Event)> = Vec::new();
    let mut acc = PerModelAccumulator::new();
    // Dominant model for the first non-empty assistant entry (used for SessionStart).
    let mut first_model: Option<String> = None;
    let mut last_ts = first_ts;
    let mut tool_use_events: Vec<(DateTime<Utc>, Event)> = Vec::new();

    let mut title: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut last_prompt: Option<String> = None;

    for entry in &lines {
        // Only advance last_ts when the entry actually carries a timestamp.
        // Synthetic lines (ai-title, last-prompt) have no timestamp field;
        // parse_timestamp would fall back to Utc::now() and corrupt the value.
        if let Some(ts) = try_parse_timestamp(entry) {
            last_ts = ts;
        }

        let entry_type = entry.get("type").and_then(|v| v.as_str());

        match entry_type {
            Some("ai-title") => {
                if title.is_none() {
                    title = entry.get("aiTitle").and_then(|v| v.as_str()).map(|s| s.to_string());
                }
            }
            Some("last-prompt") => {
                last_prompt = entry.get("prompt").and_then(|v| v.as_str()).map(|s| s.to_string());
            }
            Some("user")
                if entry
                    .get("isCompactSummary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false) =>
            {
                if summary.is_none() {
                    let text = entry.get("message").and_then(|m| extract_content_text(m));
                    summary = text.map(|t| truncate_chars(t, 2000));
                }
            }
            _ => {}
        }

        // Capture first model seen for SessionStart display.
        if entry_type == Some("assistant") {
            if first_model.is_none() {
                if let Some(message) = entry.get("message") {
                    if let Some(m) = message.get("model").and_then(|v| v.as_str()) {
                        if !m.is_empty() {
                            first_model = Some(m.to_string());
                        }
                    }
                }
            }
        }
    }

    // Process assistant entries from main file (A1 + A2: deduped per-model accumulation).
    parse_assistant_entries(&lines, &mut acc, &mut tool_use_events);

    // A3: also parse any sibling subagent files — same sessionId, same dedupe set.
    let subagent_files = discover_subagent_files(path);
    for sub_path in &subagent_files {
        match read_jsonl_lines(sub_path) {
            Ok(sub_lines) => {
                // Update last_ts if subagent file extends the timeline.
                // Walk backwards to find the last entry that actually carries a timestamp.
                if let Some(sub_last) = sub_lines.iter().rev().find_map(try_parse_timestamp) {
                    if sub_last > last_ts {
                        last_ts = sub_last;
                    }
                }
                parse_assistant_entries(&sub_lines, &mut acc, &mut tool_use_events);
            }
            Err(e) => {
                eprintln!(
                    "trakr: skipping subagent file {}: {}",
                    sub_path.display(),
                    e
                );
            }
        }
    }

    let (usage_events, dominant_model) = acc.into_events(last_ts);

    // Use dominant model (most output tokens) for SessionStart; fall back to first seen.
    let session_model = dominant_model
        .or(first_model)
        .unwrap_or_else(|| "unknown".to_string());

    // B1: backfill never fabricates a session_end.  The event stream is:
    //   SessionStart { source: "backfill" }, tool uses, per-model token usages.
    // Only the real SessionEnd hook (which observed the true ending) may write one.
    events.push((
        first_ts,
        Event::SessionStart {
            model: session_model,
            source: "backfill".to_string(),
        },
    ));
    events.extend(tool_use_events);
    events.extend(usage_events);

    Ok(Some(BackfilledSession {
        session_id,
        project_path,
        source_path: path.to_path_buf(),
        events,
        title,
        summary,
        last_prompt,
        last_activity_at: last_ts,
    }))
}

/// Decide whether to skip, insert, or replace a session, then act accordingly.
///
/// Also archives the native Claude transcript and populates summary fields.
///
/// B1: A session is **never** skipped due to having a `session_end` in the DB — only the
/// SessionEnd hook produces genuine endings, and re-parsing is idempotent via `replace_session`.
/// The skip path is intentionally removed; every session found in the transcript is (re-)parsed.
pub fn backfill_session(session: &BackfilledSession, dry_run: bool) -> Result<BackfillAction> {
    storage::init_db()?;

    let existing = storage::get_events(Some(&session.session_id))?;
    let action = if existing.is_empty() {
        BackfillAction::Inserted
    } else {
        BackfillAction::Replaced
    };

    if !dry_run {
        storage::replace_session(&session.session_id, &session.events)?;
        let last_activity_str = session.last_activity_at.to_rfc3339();
        // Use the variant that always writes last_activity_at (not COALESCE'd).
        storage::upsert_session_meta_with_activity(
            &session.session_id,
            session.project_path.as_deref(),
            session.events.first().map(|(ts, _)| *ts),
            // ended_at is omitted — only the SessionEnd hook sets a true ending timestamp (invariant 1).
            session.events.iter().find_map(|(_, e)| {
                if let Event::SessionStart { model, .. } = e {
                    if model != "unknown" { Some(model.as_str()) } else { None }
                } else {
                    None
                }
            }),
            Some("backfill"),
            session.title.as_deref(),
            session.summary.as_deref(),
            session.last_prompt.as_deref(),
            &last_activity_str,
        )?;
        if let Err(e) = storage::archive_transcript(&session.session_id, &session.source_path) {
            eprintln!("trakr: failed to archive transcript for {}: {}", &session.session_id[..8.min(session.session_id.len())], e);
        }
    }

    Ok(action)
}

/// Parse an ISO 8601 timestamp from a JSON entry's `timestamp` field.
/// Falls back to `Utc::now()` if missing or unparseable.
fn try_parse_timestamp(entry: &serde_json::Value) -> Option<DateTime<Utc>> {
    entry
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_timestamp(entry: &serde_json::Value) -> DateTime<Utc> {
    try_parse_timestamp(entry).unwrap_or_else(Utc::now)
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

    #[test]
    fn test_parse_title_and_summary_extracted() -> Result<()> {
        let tmp = TempDir::new()?;
        let content = r#"{"type":"system","sessionId":"recap1","timestamp":"2026-01-01T10:00:00Z"}
{"type":"ai-title","sessionId":"recap1","timestamp":"2026-01-01T10:01:00Z","aiTitle":"Implement transcript archiving"}
{"type":"user","sessionId":"recap1","timestamp":"2026-01-01T10:02:00Z","isCompactSummary":true,"message":{"role":"user","content":"This session is being continued. Summary: work in progress."}}
{"type":"last-prompt","sessionId":"recap1","timestamp":"2026-01-01T10:03:00Z","prompt":"update the plan"}
"#;
        let path = write_jsonl(&tmp, "recap.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        assert_eq!(session.title.as_deref(), Some("Implement transcript archiving"));
        assert_eq!(session.summary.as_deref(), Some("This session is being continued. Summary: work in progress."));
        assert_eq!(session.last_prompt.as_deref(), Some("update the plan"));
        Ok(())
    }

    #[test]
    fn test_parse_summary_truncated_to_2000_chars() -> Result<()> {
        let tmp = TempDir::new()?;
        let long_text = "x".repeat(3000);
        let content = format!(
            r#"{{"type":"system","sessionId":"trunc1","timestamp":"2026-01-01T10:00:00Z"}}
{{"type":"user","sessionId":"trunc1","timestamp":"2026-01-01T10:01:00Z","isCompactSummary":true,"message":{{"role":"user","content":"{}"}}}}
"#,
            long_text
        );
        let path = write_jsonl(&tmp, "trunc.jsonl", &content);
        let session = parse_session_log(&path)?.expect("should parse");

        let summary = session.summary.expect("should have summary");
        assert_eq!(summary.chars().count(), 2000, "summary should be truncated to 2000 chars");
        Ok(())
    }

    #[test]
    fn test_source_path_set() -> Result<()> {
        let tmp = TempDir::new()?;
        let content = r#"{"type":"system","sessionId":"pathtest","timestamp":"2026-01-01T10:00:00Z"}"#;
        let path = write_jsonl(&tmp, "pathtest.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");
        assert_eq!(session.source_path, path);
        Ok(())
    }

    // ── A1: dedupe by message.id ──────────────────────────────────────────────

    #[test]
    fn test_dedupe_by_message_id() -> Result<()> {
        // One API call emits 3 assistant JSONL lines (thinking, text, tool_use) — each
        // carries the same message.id and identical usage.  Usage should be counted once.
        let tmp = TempDir::new()?;
        let content = r#"{"type":"system","sessionId":"dedup1","timestamp":"2026-01-01T10:00:00Z"}
{"type":"assistant","sessionId":"dedup1","timestamp":"2026-01-01T10:01:00Z","message":{"id":"msg_abc","model":"claude-sonnet-4-6","content":[{"type":"thinking","thinking":"..."}],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
{"type":"assistant","sessionId":"dedup1","timestamp":"2026-01-01T10:01:00Z","message":{"id":"msg_abc","model":"claude-sonnet-4-6","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
{"type":"assistant","sessionId":"dedup1","timestamp":"2026-01-01T10:01:00Z","message":{"id":"msg_abc","model":"claude-sonnet-4-6","content":[{"type":"tool_use","name":"bash"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
        let path = write_jsonl(&tmp, "dedup1.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        let usage_events: Vec<&Event> = session
            .events
            .iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, Event::TokenUsage { .. }))
            .collect();

        assert_eq!(usage_events.len(), 1, "one model → one TokenUsage event");

        if let Event::TokenUsage { input_tokens, output_tokens, .. } = usage_events[0] {
            // Only counted once despite 3 lines with the same message.id
            assert_eq!(*input_tokens, 100, "input counted once (not 3×)");
            assert_eq!(*output_tokens, 50, "output counted once (not 3×)");
        }
        Ok(())
    }

    // ── A2: per-model usage ───────────────────────────────────────────────────

    #[test]
    fn test_per_model_usage() -> Result<()> {
        // Two distinct models in one session → two separate TokenUsage events.
        let tmp = TempDir::new()?;
        let content = r#"{"type":"system","sessionId":"multimodel1","timestamp":"2026-01-01T10:00:00Z"}
{"type":"assistant","sessionId":"multimodel1","timestamp":"2026-01-01T10:01:00Z","message":{"id":"msg_s1","model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":1000000,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
{"type":"assistant","sessionId":"multimodel1","timestamp":"2026-01-01T10:02:00Z","message":{"id":"msg_h1","model":"claude-haiku-4-5","content":[],"usage":{"input_tokens":0,"output_tokens":1000000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
        let path = write_jsonl(&tmp, "multimodel1.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        let usage_events: Vec<&Event> = session
            .events
            .iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, Event::TokenUsage { .. }))
            .collect();

        assert_eq!(usage_events.len(), 2, "two models → two TokenUsage events");

        // Check that each model appears exactly once with the correct counts.
        let mut saw_sonnet = false;
        let mut saw_haiku = false;
        for event in &usage_events {
            if let Event::TokenUsage { model, input_tokens, output_tokens, .. } = event {
                if model.contains("sonnet") {
                    assert_eq!(*input_tokens, 1_000_000);
                    assert_eq!(*output_tokens, 0);
                    saw_sonnet = true;
                } else if model.contains("haiku") {
                    assert_eq!(*input_tokens, 0);
                    assert_eq!(*output_tokens, 1_000_000);
                    saw_haiku = true;
                }
            }
        }
        assert!(saw_sonnet, "expected a sonnet TokenUsage event");
        assert!(saw_haiku, "expected a haiku TokenUsage event");

        // Verify spend = sum of both at correct rates:
        //   sonnet: 1M input @ $3/MTok = $3.00
        //   haiku: 1M output @ $5/MTok = $5.00
        //   total = $8.00
        let total_cost: f64 = usage_events.iter().filter_map(|e| {
            if let Event::TokenUsage { model, input_tokens, output_tokens,
                cache_creation_input_tokens, cache_read_input_tokens,
                cache_creation_1h_input_tokens, .. } = e {
                Some(crate::cost::compute_cost_usd(
                    model, *input_tokens, *output_tokens,
                    *cache_creation_input_tokens, *cache_read_input_tokens,
                    *cache_creation_1h_input_tokens,
                ))
            } else {
                None
            }
        }).sum();
        assert!((total_cost - 8.0).abs() < 1e-9, "expected $8.00 total, got ${}", total_cost);
        Ok(())
    }

    // ── A3: subagent transcripts ──────────────────────────────────────────────

    #[test]
    fn test_subagent_usage_included() -> Result<()> {
        // Main file + a sibling subagent file → usage and tool uses from both appear.
        let tmp = TempDir::new()?;
        let main_uuid = "a1b2c3d4-dead-beef-cafe-000000000001";

        // Write main session file.
        let main_content = format!(
            r#"{{"type":"system","sessionId":"{uuid}","timestamp":"2026-01-01T10:00:00Z"}}
{{"type":"assistant","sessionId":"{uuid}","timestamp":"2026-01-01T10:01:00Z","message":{{"id":"msg_main1","model":"claude-sonnet-4-6","content":[{{"type":"tool_use","name":"bash"}}],"usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}
"#,
            uuid = main_uuid
        );
        let project_dir = tmp.path().join("my-project");
        std::fs::create_dir_all(&project_dir)?;
        let main_path = project_dir.join(format!("{}.jsonl", main_uuid));
        std::fs::write(&main_path, main_content.as_bytes())?;

        // Write subagent file.
        let subagents_dir = project_dir.join(main_uuid).join("subagents");
        std::fs::create_dir_all(&subagents_dir)?;
        let sub_content = format!(
            r#"{{"type":"assistant","sessionId":"{uuid}","isSidechain":true,"timestamp":"2026-01-01T10:02:00Z","message":{{"id":"msg_sub1","model":"claude-haiku-4-5","content":[{{"type":"tool_use","name":"read"}}],"usage":{{"input_tokens":50,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}
"#,
            uuid = main_uuid
        );
        let sub_path = subagents_dir.join("agent-sub1.jsonl");
        std::fs::write(&sub_path, sub_content.as_bytes())?;

        let session = parse_session_log(&main_path)?.expect("should parse");

        // Should have two TokenUsage events — one per model.
        let usage_events: Vec<&Event> = session
            .events
            .iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, Event::TokenUsage { .. }))
            .collect();
        assert_eq!(usage_events.len(), 2, "main + subagent → two TokenUsage events (one per model)");

        // Should have two ToolUse events — bash from main, read from subagent.
        let tool_uses: Vec<&str> = session
            .events
            .iter()
            .filter_map(|(_, e)| if let Event::ToolUse { tool_name, .. } = e { Some(tool_name.as_str()) } else { None })
            .collect();
        assert!(tool_uses.contains(&"bash"), "bash from main file");
        assert!(tool_uses.contains(&"read"), "read from subagent file");
        Ok(())
    }

    // ── backfill_session tests ────────────────────────────────────────────────

    fn make_session(id: &str) -> BackfilledSession {
        let ts = Utc::now();
        BackfilledSession {
            session_id: id.to_string(),
            project_path: None,
            source_path: PathBuf::new(), // non-existent — archive_transcript no-ops
            // B1: no SessionEnd in the backfilled stream — invariant 1.
            events: vec![
                (ts, Event::SessionStart { model: "test-model".to_string(), source: "backfill".to_string() }),
                (ts, Event::TokenUsage {
                    model: "test-model".to_string(),
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    cache_creation_1h_input_tokens: 0,
                    total_tokens: 15,
                }),
            ],
            title: None,
            summary: None,
            last_prompt: None,
            last_activity_at: ts,
        }
    }

    // B1: backfill never skips a session that already has session_start + session_end in the DB.
    // Previously, `backfill_session` had a `Skipped` path for sessions with both events;
    // that path is now removed (B2). Sessions already in the DB are always `Replaced` so that
    // a re-run of the corrected parser self-heals inflated figures.
    #[test]
    fn test_backfill_session_replaces_even_when_complete() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            // Pre-populate a session with session_start + session_end (old behaviour / hook path).
            storage::insert_event(
                "complete_session",
                &Event::SessionStart { model: "m".to_string(), source: "hook".to_string() },
                Utc::now(),
            )?;
            storage::insert_event("complete_session", &Event::SessionEnd, Utc::now())?;

            // B1/B2: should no longer skip — returns Replaced, not Skipped.
            let session = make_session("complete_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Replaced),
                "backfill should replace even sessions that had a session_end (B2: skip path removed)");

            // After replace the DB has the clean backfilled set (no session_end from backfill).
            let events = storage::get_events(Some("complete_session"))?;
            assert_eq!(events.len(), 2, "should have 2 events (start + token_usage, no session_end)");
            let has_session_end = events.iter().any(|(_, _, e)| matches!(e, Event::SessionEnd));
            assert!(!has_session_end, "backfill must not write session_end (invariant 1)");
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_replaced_when_only_session_end() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            storage::insert_event("tail_only_session", &Event::SessionEnd, Utc::now())?;

            let session = make_session("tail_only_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Replaced));

            let events = storage::get_events(Some("tail_only_session"))?;
            assert_eq!(events.len(), 2, "should have 2 backfilled events (start + token_usage)");
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
            assert_eq!(events.len(), 2, "all events should be inserted (start + token_usage)");
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_replaced_when_partial() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            storage::insert_event(
                "partial_session",
                &Event::SessionStart { model: "old".to_string(), source: "hook".to_string() },
                Utc::now(),
            )?;

            let session = make_session("partial_session");
            let action = backfill_session(&session, false)?;
            assert!(matches!(action, BackfillAction::Replaced));

            let events = storage::get_events(Some("partial_session"))?;
            assert_eq!(events.len(), 2, "should have 2 events after replace");
            Ok(())
        })
    }

    #[test]
    fn test_backfill_session_dry_run_no_write() -> Result<()> {
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            let session = make_session("dry_run_session");
            let action = backfill_session(&session, true)?;
            assert!(matches!(action, BackfillAction::Inserted));

            let events = storage::get_events(Some("dry_run_session"))?;
            assert!(events.is_empty(), "dry_run should not write anything");
            Ok(())
        })
    }

    // ── B1: backfill never writes session_end ─────────────────────────────────

    #[test]
    fn test_no_session_end_from_backfill() -> Result<()> {
        // B1: parse_session_log must not emit a SessionEnd event, regardless of file content.
        let tmp = TempDir::new()?;
        let content = r#"{"type":"system","sessionId":"b1test","timestamp":"2026-01-01T10:00:00Z"}
{"type":"assistant","sessionId":"b1test","timestamp":"2026-01-01T10:01:00Z","message":{"id":"msg1","model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
        let path = write_jsonl(&tmp, "b1test.jsonl", content);
        let session = parse_session_log(&path)?.expect("should parse");

        let has_session_end = session.events.iter().any(|(_, e)| matches!(e, Event::SessionEnd));
        assert!(!has_session_end,
            "backfill must never emit SessionEnd (invariant 1 / B1)");

        let has_session_start = session.events.iter().any(|(_, e)| matches!(e, Event::SessionStart { .. }));
        assert!(has_session_start, "should have SessionStart");

        let has_token_usage = session.events.iter().any(|(_, e)| matches!(e, Event::TokenUsage { .. }));
        assert!(has_token_usage, "should have TokenUsage");

        Ok(())
    }

    // ── B6: active display rule ───────────────────────────────────────────────

    // ── B6: active display rule ───────────────────────────────────────────────

    #[test]
    fn test_active_display_rule() -> Result<()> {
        use chrono::Duration;

        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            storage::init_db()?;

            // Session 1: last_activity_at = now - 10 minutes → should be counted as active.
            let recently_active = (Utc::now() - Duration::minutes(10)).to_rfc3339();
            storage::upsert_session_meta_with_activity(
                "active_session", None, None, None, Some("backfill"),
                None, None, None, &recently_active,
            )?;

            // Session 2: last_activity_at = now - 3 hours → should NOT be counted.
            let stale = (Utc::now() - Duration::hours(3)).to_rfc3339();
            storage::upsert_session_meta_with_activity(
                "stale_session", None, None, None, Some("backfill"),
                None, None, None, &stale,
            )?;

            let active = storage::get_active_session_count(3600)?; // within the last hour
            assert_eq!(active, 1, "only the recently-active session (10 min) should count");

            Ok(())
        })
    }

    // ── B6: change detection ──────────────────────────────────────────────────

    #[test]
    fn test_change_detection_skips_unchanged_session() -> Result<()> {
        // When stored (file_size, file_mtime) match the actual file, get_session_file_meta
        // should return the same values, and the reconciliation logic would skip re-parsing.
        // We test the storage layer: write meta, read it back, confirm it matches.
        let tmp = TempDir::new()?;
        with_home(&tmp, || {
            storage::init_db()?;

            // Create a JSONL file.
            let path = write_jsonl(&tmp, "change_detect.jsonl",
                r#"{"type":"system","sessionId":"cd_test","timestamp":"2026-01-01T10:00:00Z"}"#);

            // Record the file's metadata.
            let meta = std::fs::metadata(&path)?;
            let size = meta.len() as i64;
            let mtime_sys = meta.modified()?;
            let mtime: chrono::DateTime<Utc> = mtime_sys.into();
            let mtime_str = mtime.to_rfc3339();

            // Create the sessions row and store file meta.
            storage::upsert_session_meta_with_activity(
                "cd_test", None, None, None, Some("backfill"),
                None, None, None, &Utc::now().to_rfc3339(),
            )?;
            storage::update_session_file_meta("cd_test", size, &mtime_str, None)?;

            // Read it back and verify it matches.
            let stored = storage::get_session_file_meta("cd_test")?
                .expect("should have stored meta");

            assert_eq!(stored.0, size, "stored file_size should match actual");
            assert_eq!(stored.1, mtime_str, "stored file_mtime should match actual");

            // Simulate the change-detection check: if stored == actual, we would skip.
            let current_size = std::fs::metadata(&path)?.len() as i64;
            let current_mtime: chrono::DateTime<Utc> = std::fs::metadata(&path)?.modified()?.into();
            let current_mtime_str = current_mtime.to_rfc3339();

            let would_skip = current_size == stored.0 && current_mtime_str == stored.1;
            assert!(would_skip, "unchanged file should be detected as unchanged and skipped");

            Ok(())
        })
    }
}
