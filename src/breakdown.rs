use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;

use crate::cost::compute_cost_usd_with_card;
use crate::rates::RateCard;

/// Classification of what a Claude turn was primarily doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    /// Reading source files via the Read/Grep/Glob/Ls tools.
    CodeRead,
    /// Writing or editing files (Write, Edit, MultiEdit, NotebookEdit).
    CodeWrite,
    /// Bash commands that explore code: grep, rg, find, fd, cat, head, tail, wc, diff, ls, tree.
    CodeSearch,
    /// Bash commands that build or run code: cargo, npm, git, python, make, etc.
    Execution,
    /// Fetching web pages or searching the web (WebFetch, WebSearch).
    WebResearch,
    /// Spawning or communicating with sub-agents (Agent, SendMessage).
    Delegation,
    /// Pure text turn — no tool call at all.
    Response,
    /// Skill / MCP calls or anything else not covered above.
    Other,
}

impl ToolCategory {
    pub fn label(self) -> &'static str {
        match self {
            ToolCategory::CodeRead    => "Code read",
            ToolCategory::CodeWrite   => "Code write",
            ToolCategory::CodeSearch  => "Code search",
            ToolCategory::Execution   => "Build/Run",
            ToolCategory::WebResearch => "Web research",
            ToolCategory::Delegation  => "Delegation",
            ToolCategory::Response    => "Response",
            ToolCategory::Other       => "Other",
        }
    }

    /// Priority used when a turn calls multiple tools: lower = higher priority.
    fn priority(self) -> u8 {
        match self {
            ToolCategory::WebResearch => 0,
            ToolCategory::Delegation  => 1,
            ToolCategory::CodeWrite   => 2,
            ToolCategory::Execution   => 3,
            ToolCategory::CodeSearch  => 4,
            ToolCategory::CodeRead    => 5,
            ToolCategory::Other       => 6,
            ToolCategory::Response    => 7,
        }
    }
}

/// A tool call extracted from a single assistant JSONL line.
struct ToolCall {
    name: String,
    /// Only populated for Bash/Execute calls; holds `input.command`.
    bash_command: Option<String>,
}

/// Classify a Bash command by inspecting its leading token.
///
/// Returns `CodeSearch` for file-reading and search commands (grep, rg, find, cat, …),
/// or `Execution` for everything else (cargo, npm, git, python, make, …).
fn categorise_bash_command(cmd: &str) -> ToolCategory {
    // Trim leading whitespace and skip env-var assignments (FOO=bar cmd …).
    let bin = cmd
        .trim()
        .split_whitespace()
        .find(|t| !t.contains('='))
        .unwrap_or("")
        // Strip any path prefix (/usr/bin/grep → grep).
        .rsplit('/')
        .next()
        .unwrap_or("");

    match bin {
        // Searching
        "grep" | "rg" | "ag" | "ack" | "fzf" => ToolCategory::CodeSearch,
        // Finding files
        "find" | "fd" => ToolCategory::CodeSearch,
        // Reading file content
        "cat" | "bat" | "head" | "tail" | "less" | "more" => ToolCategory::CodeSearch,
        // File stats / comparison
        "wc" | "diff" | "colordiff" => ToolCategory::CodeSearch,
        // Directory listing
        "ls" | "tree" | "eza" | "exa" => ToolCategory::CodeSearch,
        _ => ToolCategory::Execution,
    }
}

/// Classify a single tool call (name + optional bash command).
fn categorise_call(call: &ToolCall) -> ToolCategory {
    match call.name.as_str() {
        "Read" | "Grep" | "Glob" | "Ls" | "LS" | "List" => ToolCategory::CodeRead,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => ToolCategory::CodeWrite,
        "Bash" | "Execute" => {
            call.bash_command
                .as_deref()
                .map(categorise_bash_command)
                .unwrap_or(ToolCategory::Execution)
        }
        "WebFetch" | "WebSearch" => ToolCategory::WebResearch,
        "Agent" | "SendMessage" | "Spawn" => ToolCategory::Delegation,
        _ => ToolCategory::Other,
    }
}

/// Classify a turn given all tool calls within it.
///
/// Uses `priority()` so the most significant call wins when a turn mixes categories.
/// A turn with no calls becomes `Response`.
fn categorise_turn(calls: &[ToolCall]) -> ToolCategory {
    if calls.is_empty() {
        return ToolCategory::Response;
    }
    calls
        .iter()
        .map(categorise_call)
        .min_by_key(|c| c.priority())
        .unwrap_or(ToolCategory::Other)
}

/// Per-category token and cost totals.
#[derive(Debug, Clone)]
pub struct BreakdownRow {
    pub category: ToolCategory,
    pub turns: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_creation_1h_tokens: u64,
    pub cost_usd: f64,
}

struct TurnEntry {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cache_creation_1h_tokens: u64,
    calls: Vec<ToolCall>,
}

/// Extract tool calls (name + bash command where applicable) from a content array.
fn extract_tool_calls(content: &serde_json::Value) -> Vec<ToolCall> {
    let Some(arr) = content.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
        .filter_map(|b| {
            let name = b.get("name").and_then(|v| v.as_str())?.to_string();
            let bash_command = if matches!(name.as_str(), "Bash" | "Execute") {
                b.get("input")
                    .and_then(|inp| inp.get("command"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            };
            Some(ToolCall { name, bash_command })
        })
        .collect()
}

/// Parse one or more Claude Code JSONL files (main transcript + any subagent files) and return
/// a per-category breakdown plus the date range (first timestamp, last timestamp) seen.
///
/// Passing multiple files is equivalent to treating them as a single session — deduplication
/// by `message.id` handles any cross-file collisions.  Deduplication logic matches
/// `PerModelAccumulator` in `backfill.rs` so token totals align with `trakr spend`.
pub fn compute_breakdown_from_transcript(
    path: &Path,
    card: &RateCard,
) -> Result<(Vec<BreakdownRow>, Option<(DateTime<Utc>, DateTime<Utc>)>)> {
    compute_breakdown_from_files(&[path], card)
}

/// Like `compute_breakdown_from_transcript` but accepts multiple JSONL paths.
pub fn compute_breakdown_from_files(
    paths: &[&Path],
    card: &RateCard,
) -> Result<(Vec<BreakdownRow>, Option<(DateTime<Utc>, DateTime<Utc>)>)> {
    let mut all_contents = String::new();
    for path in paths {
        match std::fs::read_to_string(path) {
            Ok(c) => all_contents.push_str(&c),
            Err(e) => eprintln!("Warning: could not read {}: {}", path.display(), e),
        }
    }
    let contents = all_contents;

    let mut ordered_ids: Vec<String> = Vec::new();
    let mut turns: HashMap<String, TurnEntry> = HashMap::new();
    let mut anon_counter: u64 = 0;
    let mut first_ts: Option<DateTime<Utc>> = None;
    let mut last_ts: Option<DateTime<Utc>> = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        // Track date range from any line with a timestamp field.
        if let Some(ts) = obj
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
        {
            if first_ts.is_none() || ts < first_ts.unwrap() {
                first_ts = Some(ts);
            }
            if last_ts.is_none() || ts > last_ts.unwrap() {
                last_ts = Some(ts);
            }
        }

        if obj.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(message) = obj.get("message") else {
            continue;
        };

        let msg_id = message.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
        let calls_in_line = message
            .get("content")
            .map(extract_tool_calls)
            .unwrap_or_default();

        let key = msg_id.unwrap_or_else(|| {
            let k = format!("__anon_{}", anon_counter);
            anon_counter += 1;
            k
        });

        if let Some(entry) = turns.get_mut(&key) {
            entry.calls.extend(calls_in_line);
        } else {
            let usage = message.get("usage").cloned().unwrap_or(serde_json::Value::Null);
            let model = message
                .get("model")
                .and_then(|v| v.as_str())
                .filter(|m| !m.is_empty())
                .unwrap_or("unknown")
                .to_string();

            let cache_creation_1h = usage
                .get("cache_creation")
                .and_then(|cc| cc.get("ephemeral_1h_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            ordered_ids.push(key.clone());
            turns.insert(
                key,
                TurnEntry {
                    model,
                    input_tokens: usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    output_tokens: usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    cache_read_tokens: usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    cache_creation_tokens: usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    cache_creation_1h_tokens: cache_creation_1h,
                    calls: calls_in_line,
                },
            );
        }
    }

    let mut by_cat: HashMap<ToolCategory, BreakdownRow> = HashMap::new();

    for id in &ordered_ids {
        let Some(entry) = turns.get(id) else {
            continue;
        };
        let cat = categorise_turn(&entry.calls);
        let cost = compute_cost_usd_with_card(
            &entry.model,
            entry.input_tokens,
            entry.output_tokens,
            entry.cache_creation_tokens,
            entry.cache_read_tokens,
            entry.cache_creation_1h_tokens,
            card,
        );
        let row = by_cat.entry(cat).or_insert(BreakdownRow {
            category: cat,
            turns: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_1h_tokens: 0,
            cost_usd: 0.0,
        });
        row.turns += 1;
        row.input_tokens += entry.input_tokens;
        row.output_tokens += entry.output_tokens;
        row.cache_read_tokens += entry.cache_read_tokens;
        row.cache_creation_tokens += entry.cache_creation_tokens;
        row.cache_creation_1h_tokens += entry.cache_creation_1h_tokens;
        row.cost_usd += cost;
    }

    let mut rows: Vec<BreakdownRow> = by_cat.into_values().collect();
    rows.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let date_range = match (first_ts, last_ts) {
        (Some(f), Some(l)) => Some((f, l)),
        _ => None,
    };
    Ok((rows, date_range))
}

/// Merge a slice of row lists into a single aggregated list, sorted by cost descending.
pub fn merge_rows(all: Vec<Vec<BreakdownRow>>) -> Vec<BreakdownRow> {
    let mut by_cat: HashMap<ToolCategory, BreakdownRow> = HashMap::new();
    for rows in all {
        for row in rows {
            let acc = by_cat.entry(row.category).or_insert(BreakdownRow {
                category: row.category,
                turns: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cache_creation_1h_tokens: 0,
                cost_usd: 0.0,
            });
            acc.turns += row.turns;
            acc.input_tokens += row.input_tokens;
            acc.output_tokens += row.output_tokens;
            acc.cache_read_tokens += row.cache_read_tokens;
            acc.cache_creation_tokens += row.cache_creation_tokens;
            acc.cache_creation_1h_tokens += row.cache_creation_1h_tokens;
            acc.cost_usd += row.cost_usd;
        }
    }
    let mut rows: Vec<BreakdownRow> = by_cat.into_values().collect();
    rows.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> ToolCall {
        ToolCall { name: name.to_string(), bash_command: None }
    }

    fn bash(cmd: &str) -> ToolCall {
        ToolCall { name: "Bash".to_string(), bash_command: Some(cmd.to_string()) }
    }

    #[test]
    fn no_tools_is_response() {
        assert_eq!(categorise_turn(&[]), ToolCategory::Response);
    }

    #[test]
    fn read_tool_is_code_read() {
        assert_eq!(categorise_turn(&[call("Read")]), ToolCategory::CodeRead);
    }

    #[test]
    fn bash_grep_is_code_search() {
        assert_eq!(categorise_turn(&[bash("grep -rn foo src/")]), ToolCategory::CodeSearch);
    }

    #[test]
    fn bash_rg_is_code_search() {
        assert_eq!(categorise_turn(&[bash("rg 'impl Foo' src/")]), ToolCategory::CodeSearch);
    }

    #[test]
    fn bash_cat_is_code_search() {
        assert_eq!(categorise_turn(&[bash("cat src/main.rs")]), ToolCategory::CodeSearch);
    }

    #[test]
    fn bash_cargo_is_execution() {
        assert_eq!(categorise_turn(&[bash("cargo test")]), ToolCategory::Execution);
    }

    #[test]
    fn bash_git_is_execution() {
        assert_eq!(categorise_turn(&[bash("git status")]), ToolCategory::Execution);
    }

    #[test]
    fn web_wins_over_read() {
        assert_eq!(
            categorise_turn(&[call("Read"), call("WebFetch")]),
            ToolCategory::WebResearch
        );
    }

    #[test]
    fn agent_wins_over_bash_cargo() {
        assert_eq!(
            categorise_turn(&[bash("cargo build"), call("Agent")]),
            ToolCategory::Delegation
        );
    }

    #[test]
    fn code_write_beats_code_search() {
        assert_eq!(
            categorise_turn(&[bash("grep foo src/"), call("Edit")]),
            ToolCategory::CodeWrite
        );
    }

    #[test]
    fn bash_with_path_prefix() {
        assert_eq!(
            categorise_turn(&[bash("/usr/bin/grep -r foo .")]),
            ToolCategory::CodeSearch
        );
    }

    #[test]
    fn bash_with_env_var_prefix() {
        assert_eq!(
            categorise_turn(&[bash("RUST_LOG=debug cargo test")]),
            ToolCategory::Execution
        );
    }

    #[test]
    fn merge_rows_sums_correctly() {
        let a = vec![BreakdownRow {
            category: ToolCategory::CodeRead,
            turns: 3,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 200,
            cache_creation_tokens: 10,
            cache_creation_1h_tokens: 0,
            cost_usd: 1.0,
        }];
        let b = vec![BreakdownRow {
            category: ToolCategory::CodeRead,
            turns: 2,
            input_tokens: 50,
            output_tokens: 25,
            cache_read_tokens: 100,
            cache_creation_tokens: 5,
            cache_creation_1h_tokens: 0,
            cost_usd: 0.5,
        }];
        let merged = merge_rows(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].turns, 5);
        assert_eq!(merged[0].input_tokens, 150);
        assert!((merged[0].cost_usd - 1.5).abs() < 1e-9);
    }
}
