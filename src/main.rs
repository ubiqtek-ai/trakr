use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use chrono::Utc;

use ctx_trakr::backfill;
use ctx_trakr::hooks;
use ctx_trakr::storage;

#[derive(Parser)]
#[command(
    name = "ctx-trakr",
    version,
    about = "Track Claude Code context, tools, models, and agents via hooks"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Dispatch a Claude Code hook event (reads JSON from stdin)
    Hook {
        /// Hook event type: tool-use | session-start | session-end
        event_type: String,
    },
    /// Set up ~/.ctx-trakr/ and register hooks in Claude Code settings
    Init,
    /// List recorded sessions from the unified DB
    List,
    /// Migrate existing per-session JSONL files into the unified DB
    Migrate,
    /// Show a human-readable timeline of all events in a session
    Show {
        /// Session ID to display
        session_id: String,
    },
    /// Show aggregate stats across all sessions
    Stats,
    /// Delete all recorded data (DB and JSONL files)
    Reset {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Start the HTTP API server and OTEL receiver
    Serve {
        /// Override the API server port (default from config, fallback 8787)
        #[arg(long)]
        api_port: Option<u16>,
        /// Override the OTEL receiver port (default from config, fallback 4318)
        #[arg(long)]
        otel_port: Option<u16>,
    },
    /// Show month-to-date estimated spend from completed sessions (SQLite only)
    Spend,
    /// Backfill session data from Claude Code's native session logs
    BackfillLogs {
        /// Only process projects whose path contains this substring
        #[arg(long)]
        project: Option<String>,
        /// Skip sessions last modified before this date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Print what would be done without writing to the DB
        #[arg(long)]
        dry_run: bool,
    },
    /// Show stats about Claude Code's native session logs (read-only diagnostic)
    InspectLogs {
        /// Only show projects whose path contains this substring
        #[arg(long)]
        project: Option<String>,
        /// Skip sessions last modified before this date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Show full per-session list
        #[arg(long, short = 'v')]
        verbose: bool,
    },
    /// Show user prompts from a Claude Code session log
    ShowPrompts {
        /// Session ID (or unambiguous prefix) to look up
        session_id: String,
    },
}

fn main() {
    let cli = Cli::parse();
    // Hook handlers must never block Claude — always exit 0.
    if let Err(e) = run(cli) {
        eprintln!("ctx-trakr error: {:#}", e);
    }
    std::process::exit(0);
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Hook { event_type } => dispatch_hook(&event_type),
        Commands::Init => cmd_init(),
        Commands::List => cmd_list(),
        Commands::Migrate => cmd_migrate(),
        Commands::Show { session_id } => cmd_show(&session_id),
        Commands::Stats => cmd_stats(),
        Commands::Reset { yes } => cmd_reset(yes),
        Commands::Serve { api_port, otel_port } => cmd_serve(api_port, otel_port),
        Commands::Spend => cmd_spend(),
        Commands::BackfillLogs { project, since, dry_run } => {
            cmd_backfill_logs(project.as_deref(), since.as_deref(), dry_run)
        }
        Commands::InspectLogs { project, since, verbose } => {
            cmd_inspect_logs(project.as_deref(), since.as_deref(), verbose)
        }
        Commands::ShowPrompts { session_id } => cmd_show_prompts(&session_id),
    }
}

fn dispatch_hook(event_type: &str) -> Result<()> {
    match event_type {
        "tool-use" => hooks::handle_tool_use(),
        "session-start" => hooks::handle_session_start(),
        "session-end" => hooks::handle_session_end(),
        other => handle_unknown_hook(other),
    }
}

fn handle_unknown_hook(hook_event_name: &str) -> Result<()> {
    use std::io::Read;
    use chrono::Utc;
    use ctx_trakr::event::Event;

    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .ok(); // best-effort

    let payload: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or(serde_json::Value::Null);

    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown_session")
        .to_string();

    let event = Event::Other {
        hook_event_name: hook_event_name.to_string(),
        payload,
    };

    storage::insert_event(&session_id, &event, Utc::now())?;
    Ok(())
}

fn cmd_init() -> Result<()> {
    use std::fs;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;

    let base = home.join(".ctx-trakr");
    let sessions = base.join("sessions");
    fs::create_dir_all(&sessions)?;

    storage::init_db()?;
    ctx_trakr::config::write_default_config()?;

    println!("ctx-trakr: initialised {}", base.display());
    println!("ctx-trakr: unified DB:        {}", base.join("ctx-trakr.db").display());
    println!("ctx-trakr: sessions directory: {}", sessions.display());
    println!("ctx-trakr: config:             {}", base.join("config.toml").display());

    match write_hooks_to_settings() {
        Ok(()) => println!("ctx-trakr: hooks written to   ~/.claude/settings.json"),
        Err(e) => {
            println!("ctx-trakr: could not write hooks automatically: {}", e);
            println!();
            println!("Add the following to ~/.claude/settings.json under \"hooks\" manually:");
            println!();
            println!("{}", suggested_hook_config());
        }
    }

    Ok(())
}

fn suggested_hook_config() -> String {
    r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "ctx-trakr hook session-start"
          }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "ctx-trakr hook session-end"
          }
        ]
      }
    ]
  }
}"#
    .to_string()
}

/// Merge ctx-trakr hooks into `~/.claude/settings.json` idempotently.
fn write_hooks_to_settings() -> Result<()> {
    use serde_json::json;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .context("creating ~/.claude directory")?;
    let settings_path = claude_dir.join("settings.json");

    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .context("reading ~/.claude/settings.json")?;
        serde_json::from_str(&content).unwrap_or(json!({}))
    } else {
        json!({})
    };

    let to_install = [
        ("SessionStart", "ctx-trakr hook session-start"),
        ("SessionEnd",   "ctx-trakr hook session-end"),
    ];

    {
        let settings_obj = settings.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("~/.claude/settings.json is not a JSON object"))?;
        let hooks_val = settings_obj.entry("hooks").or_insert(json!({}));
        let hooks_obj = hooks_val.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks field is not a JSON object"))?;

        for (event, command) in &to_install {
            let arr = hooks_obj.entry(*event).or_insert(json!([]));
            let arr = arr.as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("{} hooks is not an array", event))?;

            let already = arr.iter().any(|entry| {
                entry.get("hooks")
                    .and_then(|v| v.as_array())
                    .map(|hs| hs.iter().any(|h| {
                        h.get("command").and_then(|v| v.as_str()) == Some(*command)
                    }))
                    .unwrap_or(false)
            });

            if !already {
                arr.push(json!({
                    "hooks": [{"type": "command", "command": command}]
                }));
            }
        }
    }

    let json = serde_json::to_string_pretty(&settings)
        .context("serialising settings.json")?;
    std::fs::write(&settings_path, format!("{}\n", json))
        .context("writing ~/.claude/settings.json")?;

    Ok(())
}

fn cmd_reset(yes: bool) -> Result<()> {
    use std::fs;
    use std::io::{self, Write as IoWrite};
    use anyhow::Context;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let base = home.join(".ctx-trakr");

    if !base.exists() {
        println!("Nothing to reset — {} does not exist.", base.display());
        return Ok(());
    }

    if !yes {
        print!("This will delete all data in {}. Continue? [y/N] ", base.display());
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).ok();
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Clear the DB contents but keep the file and schema.
    let db = base.join("ctx-trakr.db");
    if db.exists() {
        let conn = rusqlite::Connection::open(&db)
            .with_context(|| format!("opening DB at {}", db.display()))?;
        conn.execute_batch("DELETE FROM events;")
            .context("truncating events table")?;
    }

    // Remove JSONL session files but keep the directory.
    let sessions = base.join("sessions");
    if sessions.exists() {
        for entry in fs::read_dir(&sessions)? {
            let path = entry?.path();
            if path.extension().map_or(false, |e| e == "jsonl") {
                fs::remove_file(&path)?;
            }
        }
    }

    println!("Reset complete — all events cleared.");
    Ok(())
}

fn cmd_list() -> Result<()> {
    let sessions = storage::get_sessions()?;

    if sessions.is_empty() {
        println!("No sessions recorded yet. Run `ctx-trakr hook session-start` or `ctx-trakr migrate`.");
        return Ok(());
    }

    println!("{:<40}  events", "session_id");
    println!("{}", "-".repeat(55));

    for (session_id, count) in &sessions {
        println!("{:<40}  {}", session_id, count);
    }

    println!();
    println!("Total: {} session(s)", sessions.len());

    Ok(())
}

fn cmd_migrate() -> Result<()> {
    use std::fs;
    use chrono::Utc;
    use ctx_trakr::event::Event;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let sessions_dir = home.join(".ctx-trakr").join("sessions");

    if !sessions_dir.exists() {
        println!("No sessions directory found at {}. Nothing to migrate.", sessions_dir.display());
        return Ok(());
    }

    // Ensure the unified DB is ready.
    storage::init_db()?;

    let mut jsonl_files: Vec<_> = fs::read_dir(&sessions_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |ext| ext == "jsonl")
        })
        .collect();

    jsonl_files.sort_by_key(|e| e.file_name());

    if jsonl_files.is_empty() {
        println!("No JSONL files found in {}. Nothing to migrate.", sessions_dir.display());
        return Ok(());
    }

    let mut total_sessions = 0usize;
    let mut total_events = 0usize;

    for entry in jsonl_files {
        let path = entry.path();
        let session_id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(id) => id.to_string(),
            None => {
                eprintln!("Skipping file with unparseable name: {}", path.display());
                continue;
            }
        };

        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Skipping {}: {}", path.display(), e);
                continue;
            }
        };

        let mut session_count = 0usize;

        for (line_num, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let record: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "  Skipping malformed line {} in {}: {}",
                        line_num + 1,
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            // Parse timestamp; fall back to now if missing/malformed.
            let timestamp = record
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);

            // Parse the event from the "payload" field.
            let payload_val = match record.get("payload") {
                Some(v) => v.clone(),
                None => {
                    eprintln!(
                        "  Skipping line {} in {} — missing 'payload' field",
                        line_num + 1,
                        path.display()
                    );
                    continue;
                }
            };

            let event: Event = match serde_json::from_value(payload_val) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "  Skipping line {} in {} — cannot parse event: {}",
                        line_num + 1,
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            // Idempotent insert: check whether this exact (session_id, timestamp, event_type) combo
            // already exists before inserting. We use INSERT OR IGNORE via a unique-ish check.
            // Simpler: just attempt the insert and rely on the fact that re-running is usually safe
            // for append-only data. For true idempotency we check first.
            let event_type = event.event_type_label();
            let ts_str = timestamp.to_rfc3339();

            // Check if this event already exists (by session_id + timestamp + event_type + payload).
            let payload_str = serde_json::to_string(&event).unwrap_or_default();
            let already_exists = {
                // We need a raw connection to run a check query; reuse storage's open path via a
                // small helper. Since storage doesn't expose the connection, open it directly here.
                let db_path = home.join(".ctx-trakr").join("ctx-trakr.db");
                let conn = rusqlite::Connection::open(&db_path)
                    .with_context(|| format!("opening DB at {}", db_path.display()))?;
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND timestamp=?2 AND event_type=?3 AND payload=?4",
                    rusqlite::params![session_id, ts_str, event_type, payload_str],
                    |row| row.get(0),
                ).unwrap_or(0);
                count > 0
            };

            if !already_exists {
                storage::insert_event(&session_id, &event, timestamp)?;
                session_count += 1;
            }
        }

        println!("Migrated session {}: {} events", session_id, session_count);
        total_sessions += 1;
        total_events += session_count;
    }

    println!();
    println!(
        "✓ Migrated {} session(s), {} event(s)",
        total_sessions, total_events
    );

    Ok(())
}

fn cmd_show(session_id: &str) -> Result<()> {
    use ctx_trakr::event::Event;

    let events = storage::get_events(Some(session_id))?;

    if events.is_empty() {
        println!("No events found for session: {}", session_id);
        return Ok(());
    }

    println!("Session: {}", session_id);
    println!("{}", "-".repeat(57));

    for (_sid, ts, event) in &events {
        let time = ts.format("%H:%M:%S").to_string();
        let label = event.event_type_label();

        let detail = match event {
            Event::SessionStart { model, source } => {
                format!("model={} source={}", model, source)
            }
            Event::SessionEnd => String::new(),
            Event::ToolUse { tool_name, status, duration_ms, .. } => {
                let dur = duration_ms
                    .map(|d| format!(", {}ms", d))
                    .unwrap_or_default();
                format!("{} ({}{})", tool_name, status, dur)
            }
            Event::TokenUsage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                total_tokens,
                ..
            } => {
                format!(
                    "input={} output={} cache_read={} cache_create={} total={}",
                    input_tokens, output_tokens, cache_read_input_tokens,
                    cache_creation_input_tokens, total_tokens
                )
            }
            Event::SubagentStart { name, agent_type } => {
                format!("name={} type={}", name, agent_type)
            }
            Event::SubagentStop { name } => {
                format!("name={}", name)
            }
            Event::ContextCompression { before_tokens, after_tokens } => {
                format!("before={} after={}", before_tokens, after_tokens)
            }
            Event::Other { hook_event_name, .. } => {
                format!("hook={}", hook_event_name)
            }
        };

        if detail.is_empty() {
            println!("{}  {:<20}", time, label);
        } else {
            println!("{}  {:<20}  {}", time, label, detail);
        }
    }

    println!("{}", "-".repeat(57));
    println!("{} events", events.len());

    Ok(())
}

fn cmd_stats() -> Result<()> {
    use ctx_trakr::event::Event;
    use std::collections::HashMap;

    let all_events = storage::get_events(None)?;
    let sessions = storage::get_sessions()?;

    let total_sessions = sessions.len();
    let total_events = all_events.len();

    // Aggregate stats.
    let mut tool_counts: HashMap<String, u64> = HashMap::new();
    let mut model_counts: HashMap<String, u64> = HashMap::new();
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut cache_read: u64 = 0;
    let mut cache_create: u64 = 0;
    let mut token_total: u64 = 0;

    // Track first-seen timestamp per session.
    let mut session_first: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();

    for (sid, ts, event) in &all_events {
        session_first
            .entry(sid.clone())
            .and_modify(|existing| {
                if ts < existing {
                    *existing = *ts;
                }
            })
            .or_insert(*ts);

        match event {
            Event::ToolUse { tool_name, .. } => {
                let normalized = capitalize_first(tool_name);
                *tool_counts.entry(normalized).or_insert(0) += 1;
            }
            Event::TokenUsage {
                model,
                input_tokens: inp,
                output_tokens: out,
                cache_creation_input_tokens: cc,
                cache_read_input_tokens: cr,
                total_tokens: tot,
            } => {
                *model_counts.entry(model.clone()).or_insert(0) += 1;
                input_tokens += inp;
                output_tokens += out;
                cache_read += cr;
                cache_create += cc;
                token_total += tot;
            }
            _ => {}
        }
    }

    println!("ctx-trakr stats");
    println!("{}", "=".repeat(39));
    println!();
    println!("Sessions: {}   Events: {}", total_sessions, total_events);

    // Tool counts sorted by frequency descending.
    if !tool_counts.is_empty() {
        println!();
        println!("Top tools:");
        let mut tools: Vec<(String, u64)> = tool_counts.into_iter().collect();
        tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (name, count) in &tools {
            println!("  {:<20}  {}", name, count);
        }
    }

    // Token usage.
    if token_total > 0 {
        println!();
        println!("Token usage:");
        println!("  Input:          {:>13}", fmt_num(input_tokens));
        println!("  Output:         {:>13}", fmt_num(output_tokens));
        println!("  Cache read:     {:>13}", fmt_num(cache_read));
        println!("  Cache create:   {:>13}", fmt_num(cache_create));
        println!("  Total:          {:>13}", fmt_num(token_total));
    }

    // Model distribution.
    if !model_counts.is_empty() {
        println!();
        println!("Models:");
        let mut models: Vec<(String, u64)> = model_counts.into_iter().collect();
        models.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (model, count) in &models {
            println!("  {:<30}  {}", model, count);
        }
    }

    // Session list.
    if !sessions.is_empty() {
        println!();
        println!("Sessions:");
        for (sid, count) in &sessions {
            let date = session_first
                .get(sid)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown   ".to_string());
            let short_id = if sid.len() > 12 {
                format!("{}...", &sid[..9])
            } else {
                sid.clone()
            };
            println!("  {:<14}  {}  {} events", short_id, date, count);
        }
    }

    Ok(())
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn cmd_backfill_logs(
    project: Option<&str>,
    since: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    use backfill::BackfillAction;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");

    if !projects_dir.exists() {
        println!("No Claude projects directory found at {}.", projects_dir.display());
        return Ok(());
    }

    let since_date = since
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .with_context(|| format!("'{}' is not a valid date — expected YYYY-MM-DD", s))
        })
        .transpose()?;

    if dry_run {
        println!("DRY RUN — no changes will be written.");
    }

    storage::init_db()?;

    let paths = backfill::discover_sessions(&projects_dir, project, since_date)?;

    let mut n_new = 0usize;
    let mut n_replaced = 0usize;
    let mut n_skipped = 0usize;

    for path in &paths {
        let project_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let session = match backfill::parse_session_log(path) {
            Ok(Some(s)) => s,
            Ok(None) => {
                // Empty or unidentifiable file — skip silently.
                continue;
            }
            Err(e) => {
                println!("[parse error] {}: {}", path.display(), e);
                continue;
            }
        };

        let short_id = if session.session_id.len() >= 8 {
            &session.session_id[..8]
        } else {
            &session.session_id
        };

        let action = backfill::backfill_session(&session, dry_run)?;

        match action {
            BackfillAction::Skipped => {
                println!("[skip]    {}  {}", short_id, project_name);
                n_skipped += 1;
            }
            BackfillAction::Inserted => {
                let tool_uses = session
                    .events
                    .iter()
                    .filter(|(_, e)| matches!(e, ctx_trakr::event::Event::ToolUse { .. }))
                    .count();
                // Assistant turns = number of TokenUsage events (one per session here, but
                // we count assistant entries indirectly via tool-use bearing turns).
                // We emit one TokenUsage per session so we count ToolUse to proxy turns.
                println!(
                    "[new]     {}  {}  →  {} tool uses",
                    short_id, project_name, tool_uses
                );
                n_new += 1;
            }
            BackfillAction::Replaced => {
                let tool_uses = session
                    .events
                    .iter()
                    .filter(|(_, e)| matches!(e, ctx_trakr::event::Event::ToolUse { .. }))
                    .count();
                println!(
                    "[replace] {}  {}  →  {} tool uses",
                    short_id, project_name, tool_uses
                );
                n_replaced += 1;
            }
        }
    }

    println!();
    println!("Done. {} new, {} replaced, {} skipped.", n_new, n_replaced, n_skipped);

    Ok(())
}

fn cmd_inspect_logs(project: Option<&str>, since: Option<&str>, verbose: bool) -> Result<()> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");

    if !projects_dir.exists() {
        println!("No Claude projects directory found at {}.", projects_dir.display());
        return Ok(());
    }

    let since_date = since
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .with_context(|| format!("'{}' is not a valid date — expected YYYY-MM-DD", s))
        })
        .transpose()?;

    let summaries = backfill::inspect_logs(&projects_dir, project, since_date)?;

    use ctx_trakr::backfill::TrackingStatus;

    // ── Claude logs section ────────────────────────────────────────────────────

    let log_count = summaries.len();
    let log_earliest = summaries.iter().filter_map(|s| s.first_ts).min();
    let log_latest = summaries.iter().filter_map(|s| s.last_ts).max();
    let n_complete = summaries.iter().filter(|s| s.tracking == TrackingStatus::Complete).count();
    let n_partial  = summaries.iter().filter(|s| s.tracking == TrackingStatus::Partial).count();
    let n_missing  = summaries.iter().filter(|s| s.tracking == TrackingStatus::Missing).count();

    println!("Claude Code session logs  ({})", projects_dir.display());
    println!("{}", "-".repeat(55));
    if log_count == 0 {
        println!("  No session logs found.");
    } else {
        println!("  Sessions:    {}", log_count);
        println!("  Date range:  {}  →  {}",
            log_earliest.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "unknown".to_string()),
            log_latest.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "unknown".to_string()),
        );
        println!("  Complete:    {}  (fully tracked by hooks)", n_complete);
        println!("  Partial:     {}  (hooks ran but session_start or session_end missing)", n_partial);
        println!("  Missing:     {}  (not in DB at all — backfill-logs would add these)", n_missing);
    }

    // ── ctx-trakr DB section ───────────────────────────────────────────────────

    println!();
    println!("ctx-trakr DB  (~/.ctx-trakr/ctx-trakr.db)");
    println!("{}", "-".repeat(55));
    match storage::get_db_summary()? {
        None => {
            println!("  DB is empty or does not exist.");
        }
        Some((db_earliest, db_latest, db_count)) => {
            // Sessions in DB that have no corresponding log file (logs pruned by Claude Code).
            let in_logs: std::collections::HashSet<&str> =
                summaries.iter().map(|s| s.session_id.as_str()).collect();
            let db_ids: std::collections::HashSet<String> = storage::get_sessions()
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let logs_pruned = db_ids.iter().filter(|id| !in_logs.contains(id.as_str())).count();

            println!("  Sessions:    {}", db_count);
            println!("  Date range:  {}  →  {}",
                db_earliest.format("%Y-%m-%d"),
                db_latest.format("%Y-%m-%d"),
            );
            if logs_pruned > 0 {
                println!("  No log file: {} session(s) in DB with no corresponding Claude log (project deleted or log pruned)", logs_pruned);
            }
        }
    }

    // ── verbose: per-session list ──────────────────────────────────────────────

    if verbose && log_count > 0 {
        println!();
        println!("{:<36}  {}  {:>8}  {}",
            "session", "date      ", "tracking", "project");
        println!("{}", "-".repeat(90));

        for s in &summaries {
            let date = s.first_ts
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown   ".to_string());
            let status = match s.tracking {
                TrackingStatus::Complete => "complete",
                TrackingStatus::Partial  => "partial ",
                TrackingStatus::Missing  => "missing ",
            };
            let short_project = if s.project.len() > 36 {
                format!("{}...", &s.project[..33])
            } else {
                s.project.clone()
            };
            println!("{}  {}  {}  {}", s.session_id, date, status, short_project);
        }
    }

    Ok(())
}

/// Silently backfill any log-file sessions not yet fully recorded in the DB.
///
/// Called on `serve` startup so a missed SessionEnd hook self-heals before logs are pruned.
fn run_log_reconciliation() -> Result<()> {
    use backfill::BackfillAction;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.exists() {
        return Ok(());
    }

    let paths = backfill::discover_sessions(&projects_dir, None, None)?;
    let mut n_new = 0usize;
    let mut n_replaced = 0usize;

    for path in &paths {
        if let Ok(Some(session)) = backfill::parse_session_log(path) {
            match backfill::backfill_session(&session, false) {
                Ok(BackfillAction::Inserted) => n_new += 1,
                Ok(BackfillAction::Replaced) => n_replaced += 1,
                _ => {}
            }
        }
    }

    if n_new + n_replaced > 0 {
        eprintln!(
            "ctx-trakr: reconciled {} new, {} replaced session(s) from logs",
            n_new, n_replaced
        );
    }

    Ok(())
}

fn cmd_serve(api_port_override: Option<u16>, otel_port_override: Option<u16>) -> Result<()> {
    use ctx_trakr::config;
    use ctx_trakr::otel_receiver;
    use ctx_trakr::server::{AppState, start_server};

    let cfg = config::load_config()?;
    let api_port = api_port_override.unwrap_or(cfg.api_port);
    let otel_port = otel_port_override.unwrap_or(cfg.otel_port);

    storage::init_db()?;

    // Reconcile any sessions whose SessionEnd hook was missed before starting the server.
    if let Err(e) = run_log_reconciliation() {
        eprintln!("ctx-trakr: reconciliation warning: {:#}", e);
    }

    let rt = tokio::runtime::Runtime::new().context("creating tokio runtime")?;
    rt.block_on(async move {
        let costs = otel_receiver::new_session_costs();

        let state = AppState {
            costs: costs.clone(),
            budget_usd: cfg.monthly_budget_usd,
        };

        tokio::join!(
            start_server(api_port, state),
            otel_receiver::start_otel_receiver(otel_port, costs),
        );
    });

    Ok(())
}

fn cmd_spend() -> Result<()> {
    use ctx_trakr::config;

    let cfg = config::load_config()?;
    let year_month = Utc::now().format("%Y-%m").to_string();
    let (spent, count) = storage::get_monthly_spend_usd(&year_month)?;

    println!(
        "${:.2} / ${:.2}  ({} completed session(s) in {})",
        spent, cfg.monthly_budget_usd, count, year_month
    );
    println!("(SQLite only — start `ctx-trakr serve` for live active-session data)");

    Ok(())
}

fn cmd_show_prompts(session_id: &str) -> Result<()> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");

    // Find the log file by searching all project subdirs for <session_id>.jsonl.
    // Also accept an unambiguous prefix.
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&projects_dir)
        .with_context(|| format!("reading {}", projects_dir.display()))?
    {
        let proj = entry?.path();
        if !proj.is_dir() { continue; }
        for sub in std::fs::read_dir(&proj)? {
            let path = sub?.path();
            if path.extension().map_or(true, |e| e != "jsonl") { continue; }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem == session_id || stem.starts_with(session_id) {
                    found.push(path);
                }
            }
        }
    }

    let path = match found.len() {
        0 => anyhow::bail!("no session log found for '{}'", session_id),
        1 => found.remove(0),
        _ => anyhow::bail!(
            "{} sessions match '{}' — be more specific:\n{}",
            found.len(), session_id,
            found.iter().map(|p| format!("  {}", p.display())).collect::<Vec<_>>().join("\n")
        ),
    };

    let project = path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    println!("Session:  {}", session_id);
    println!("Project:  {}", project);
    println!("Log:      {}", path.display());
    println!("{}", "-".repeat(60));

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    // Parse all lines up front so we can find first/last timestamps.
    let entries: Vec<serde_json::Value> = contents.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();

    let parse_ts = |entry: &serde_json::Value| -> Option<String> {
        entry.get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
    };

    let first_ts = entries.iter().find_map(|e| parse_ts(e))
        .unwrap_or_else(|| "unknown".to_string());
    let last_ts = entries.iter().rev().find_map(|e| parse_ts(e))
        .unwrap_or_else(|| "unknown".to_string());

    println!("[{}]  ── first entry ──", first_ts);

    let mut count = 0usize;
    for entry in &entries {
        if entry.get("type").and_then(|v| v.as_str()) != Some("user") { continue }
        if entry.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false) { continue }

        let content = entry.get("message").and_then(|m| m.get("content"));
        // Only string content is a human prompt — list content is tool results.
        let Some(text) = content.and_then(|c| c.as_str()) else { continue };

        // Skip injected system/command messages.
        if text.starts_with("<local-command") || text.starts_with("<command-name") { continue }

        let ts = parse_ts(entry).unwrap_or_else(|| "unknown time     ".to_string());
        println!("[{}]  {}", ts, text.trim());
        count += 1;
    }

    println!("[{}]  ── last entry ──", last_ts);
    println!("{}", "-".repeat(60));
    println!("{} prompt(s)", count);
    Ok(())
}

/// Format an integer with thousands separators.
fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}
