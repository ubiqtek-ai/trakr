use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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

    println!("ctx-trakr: initialized {}", base.display());
    println!("ctx-trakr: unified DB:        {}", base.join("ctx-trakr.db").display());
    println!("ctx-trakr: sessions directory: {}", sessions.display());

    println!();
    println!("Add the following to ~/.claude/settings.json under \"hooks\":");
    println!();
    println!("{}", suggested_hook_config());

    Ok(())
}

fn suggested_hook_config() -> String {
    r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "ctx-trakr hook tool-use"
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
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
