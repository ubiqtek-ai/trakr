use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use chrono::{Datelike, Utc};

macro_rules! tlog {
    ($($arg:tt)*) => {
        eprintln!("{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), format_args!($($arg)*))
    };
}

use trakr::archive;
use trakr::backfill;
use trakr::hooks;
use trakr::storage;

/// Stats returned by `run_log_reconciliation`.
struct ReconcileStats {
    pub n_new: usize,
    pub n_updated: usize,
    pub n_unchanged: usize,
}

#[derive(Parser)]
#[command(
    name = "trakr",
    version,
    about = "Track Claude Code context usage and estimate spend across sessions"
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
    /// Set up ~/.trakr/ (DB, directories, config)
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
    Spend {
        /// Output compact JSON instead of the human-readable table (no reconciliation)
        #[arg(long)]
        json: bool,
    },
    /// Check the health of the full tracking pipeline (settings, hooks, OTEL, server, DB)
    Status,
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
    /// Repair DB rows built by the old (over-counting) parser — re-backfills every session
    /// whose transcript still exists; leaves sessions with no surviving transcript untouched.
    Repair {
        /// Show what would be done without writing to the DB
        #[arg(long)]
        dry_run: bool,
    },
    /// Show stats about Claude Code's native session logs (read-only diagnostic)
    #[command(name = "inspect")]
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
    /// Tail the trakr serve log
    Logs {
        /// Number of lines to show initially (default 50)
        #[arg(short = 'n', default_value = "50")]
        lines: usize,
    },
    /// Install trakr serve as a macOS LaunchAgent (starts on login)
    InstallService,
    /// Remove the trakr LaunchAgent
    UninstallService,
    /// Restart the trakr LaunchAgent (picks up a new binary)
    RestartService,
    /// Copy Claude transcripts from ~/.claude/projects/ into ~/.trakr/archive/ (incremental)
    Archive,
    /// Manually trigger a session-log reconciliation and print a summary
    Sync,
    /// Fetch current model pricing from LiteLLM and update ~/.trakr/rates.json
    SyncRates,
}

fn main() {
    let cli = Cli::parse();
    // Hook handlers must never block Claude — always exit 0.
    if let Err(e) = run(cli) {
        eprintln!("trakr error: {:#}", e);
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
        Commands::Spend { json } => cmd_spend(json),
        Commands::Status => cmd_status(),
        Commands::BackfillLogs { project, since, dry_run } => {
            cmd_backfill_logs(project.as_deref(), since.as_deref(), dry_run)
        }
        Commands::Repair { dry_run } => cmd_repair(dry_run),
        Commands::InspectLogs { project, since, verbose } => {
            cmd_inspect_logs(project.as_deref(), since.as_deref(), verbose)
        }
        Commands::ShowPrompts { session_id } => cmd_show_prompts(&session_id),
        Commands::Logs { lines } => cmd_logs(lines),
        Commands::InstallService => cmd_install_service(),
        Commands::UninstallService => cmd_uninstall_service(),
        Commands::RestartService => { cmd_uninstall_service()?; cmd_install_service() },
        Commands::Archive => cmd_archive(),
        Commands::Sync => cmd_sync(),
        Commands::SyncRates => cmd_sync_rates(),
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
    use trakr::event::Event;

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

    let base = home.join(".trakr");
    let sessions = base.join("sessions");
    let transcripts = base.join("transcripts");
    let archive_dir = base.join("archive");
    fs::create_dir_all(&sessions)?;
    fs::create_dir_all(&transcripts)?;
    fs::create_dir_all(&archive_dir)?;

    storage::init_db()?;
    trakr::config::write_default_config()?;

    println!("trakr: initialised {}", base.display());
    println!("trakr: unified DB:         {}", base.join("trakr.db").display());
    println!("trakr: sessions directory: {}", sessions.display());
    println!("trakr: transcripts:        {}", transcripts.display());
    println!("trakr: archive:            {}", archive_dir.display());
    println!("trakr: config:             {}", base.join("config.toml").display());
    println!();
    println!("Run `trakr install-service` to start the background service.");
    println!("Run `trakr backfill-logs` to import existing Claude sessions.");

    Ok(())
}

fn cmd_reset(yes: bool) -> Result<()> {
    use std::fs;
    use std::io::{self, Write as IoWrite};
    use anyhow::Context;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let base = home.join(".trakr");

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
    let db = base.join("trakr.db");
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

    // Remove archived transcripts but keep the directory.
    let transcripts = base.join("transcripts");
    if transcripts.exists() {
        for entry in fs::read_dir(&transcripts)? {
            let path = entry?.path();
            if path.extension().map_or(false, |e| e == "jsonl") {
                fs::remove_file(&path)?;
            }
        }
    }

    println!("Reset complete — all events and transcripts cleared.");
    Ok(())
}

fn cmd_list() -> Result<()> {
    let sessions = storage::get_sessions()?;

    if sessions.is_empty() {
        println!("No sessions recorded yet. Run `trakr hook session-start` or `trakr migrate`.");
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
    use trakr::event::Event;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let sessions_dir = home.join(".trakr").join("sessions");

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
                let db_path = home.join(".trakr").join("trakr.db");
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
    use trakr::event::Event;

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
    use trakr::event::Event;
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

    println!("trakr stats");
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
                    .filter(|(_, e)| matches!(e, trakr::event::Event::ToolUse { .. }))
                    .count();
                println!(
                    "[new]     {}  {}  →  {} tool uses",
                    short_id, project_name, tool_uses
                );
                n_new += 1;

                // Update file metadata for change detection in future sweeps.
                if !dry_run {
                    update_file_meta_for_path(&session.session_id, path, &session.last_activity_at);
                }
            }
            BackfillAction::Replaced => {
                let tool_uses = session
                    .events
                    .iter()
                    .filter(|(_, e)| matches!(e, trakr::event::Event::ToolUse { .. }))
                    .count();
                println!(
                    "[replace] {}  {}  →  {} tool uses",
                    short_id, project_name, tool_uses
                );
                n_replaced += 1;

                // Update file metadata for change detection in future sweeps.
                if !dry_run {
                    update_file_meta_for_path(&session.session_id, path, &session.last_activity_at);
                }
            }
        }
    }

    println!();
    println!(
        "Done. {} new, {} replaced, {} skipped.",
        n_new, n_replaced, n_skipped
    );

    Ok(())
}

/// Update file-metadata columns in the sessions table after a successful backfill.
///
/// Computes a composite (size, mtime) that covers both the main file and any subagent
/// files so that the change-detection logic in `run_log_reconciliation` stays accurate.
fn update_file_meta_for_path(
    session_id: &str,
    main_path: &std::path::Path,
    last_activity_at: &chrono::DateTime<Utc>,
) {
    let meta = composite_file_meta(main_path);
    if let Some((size, mtime)) = meta {
        let _ = storage::update_session_file_meta(
            session_id,
            size,
            &mtime,
            Some(&last_activity_at.to_rfc3339()),
        );
    }
}

/// Compute a composite (total_size, max_mtime_rfc3339) across the main file and its subagent files.
///
/// Returns None if the main file's metadata is unreadable.
fn composite_file_meta(main_path: &std::path::Path) -> Option<(i64, String)> {
    use backfill::discover_subagent_files_pub;

    let main_meta = std::fs::metadata(main_path).ok()?;
    let main_size = main_meta.len() as i64;
    let main_mtime: chrono::DateTime<Utc> = main_meta.modified().ok()?.into();

    let mut total_size = main_size;
    let mut max_mtime = main_mtime;

    for sub_path in discover_subagent_files_pub(main_path) {
        if let Ok(sub_meta) = std::fs::metadata(&sub_path) {
            total_size += sub_meta.len() as i64;
            if let Ok(sub_mtime_sys) = sub_meta.modified() {
                let sub_mtime: chrono::DateTime<Utc> = sub_mtime_sys.into();
                if sub_mtime > max_mtime {
                    max_mtime = sub_mtime;
                }
            }
        }
    }

    Some((total_size, max_mtime.to_rfc3339()))
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

    // ── Discover log files ────────────────────────────────────────────────────

    let log_paths = backfill::discover_sessions(&projects_dir, project, since_date)?;
    let n_log_files = log_paths.len();

    // Build a map: session_id → (PathBuf, file_mtime_rfc3339)
    // We need the mtime for sync-status comparison and the date-range display.
    let mut log_file_map: std::collections::HashMap<String, (std::path::PathBuf, Option<String>)> =
        std::collections::HashMap::new();

    let mut earliest_mtime: Option<chrono::DateTime<Utc>> = None;
    let mut latest_mtime:   Option<chrono::DateTime<Utc>> = None;

    for path in &log_paths {
        let sid = match peek_session_id(path) {
            Some(s) => s,
            None => continue,
        };
        let mtime_opt: Option<String> = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: chrono::DateTime<Utc> = t.into();
                if earliest_mtime.map_or(true, |e| dt < e) { earliest_mtime = Some(dt); }
                if latest_mtime.map_or(true,   |l| dt > l) { latest_mtime   = Some(dt); }
                dt.to_rfc3339()
            });
        log_file_map.insert(sid, (path.clone(), mtime_opt));
    }

    // ── DB sessions ───────────────────────────────────────────────────────────

    let db_sessions = storage::get_all_sessions_meta().unwrap_or_default();
    let db_session_ids: std::collections::HashSet<&str> =
        db_sessions.iter().map(|s| s.session_id.as_str()).collect();

    // Counts for the summary block.
    let n_in_db = log_file_map.keys()
        .filter(|sid| db_session_ids.contains(sid.as_str()))
        .count();

    // Stale: in DB, file has changed since last parse (file_mtime differs from stored).
    let n_stale = db_sessions.iter()
        .filter(|s| {
            if let Some((_, current_mtime)) = log_file_map.get(&s.session_id) {
                match (&s.file_mtime, current_mtime) {
                    (Some(stored), Some(current)) => stored != current,
                    (Some(_), None) => false, // can't read file mtime — not stale
                    (None, Some(_)) => false,  // file_mtime NULL → never parsed → "new"
                    (None, None) => false,
                }
            } else {
                false // not in log_file_map → orphaned, not stale
            }
        })
        .count();

    // New: log file exists but session is not in DB at all.
    let n_new_files = log_file_map.keys()
        .filter(|sid| !db_session_ids.contains(sid.as_str()))
        .count();

    // Orphaned: in DB but no corresponding log file.
    let n_orphaned = db_sessions.iter()
        .filter(|s| !log_file_map.contains_key(&s.session_id))
        .count();

    // ── Spend ─────────────────────────────────────────────────────────────────

    let total_spend = storage::get_total_spend_usd().unwrap_or(0.0);
    let year_month = Utc::now().format("%Y-%m").to_string();
    let (month_spend, _) = storage::get_monthly_spend_usd(&year_month).unwrap_or((0.0, 0));
    let month_label = chrono::Local::now().format("%B %Y").to_string();
    let total_tokens = storage::get_token_totals(None).unwrap_or_default();
    let month_tokens = storage::get_token_totals(Some(&year_month)).unwrap_or_default();

    // ── Summary block ─────────────────────────────────────────────────────────

    const W: usize = 49;
    let date_range = match (earliest_mtime, latest_mtime) {
        (Some(e), Some(l)) => format!(
            "({} → {})",
            e.format("%-d %b %Y"),
            l.format("%-d %b %Y"),
        ),
        _ => String::new(),
    };

    println!("Session logs  ({})", projects_dir.display());
    println!("{}", "─".repeat(W));
    println!("  {:<16} {:>5}   {}", "Log files", n_log_files, date_range);
    println!("  {:<16} {:>5}", "In DB", n_in_db);
    println!("  {:<16} {:>5}   {}", "Stale", n_stale,
        if n_stale > 0 { "(file changed since last parse)" } else { "" });
    println!("  {:<16} {:>5}   {}", "New", n_new_files,
        if n_new_files > 0 { "(log files not yet in DB)" } else { "" });
    println!("  {:<16} {:>5}   {}", "Orphaned", n_orphaned,
        if n_orphaned > 0 { "(in DB, log file missing)" } else { "" });
    println!();
    println!("  {:<24} ${:.2}", "Spend (all time)", total_spend);
    println!("  {:<24} ${:.2}", format!("Spend ({})", month_label), month_spend);
    println!();
    println!("  {:<24} {:>8} in / {:>8} out / {:>8} cache",
        "Tokens (all time)",
        fmt_tokens_compact(total_tokens.input),
        fmt_tokens_compact(total_tokens.output),
        fmt_tokens_compact(total_tokens.cache_read + total_tokens.cache_creation));
    println!("  {:<24} {:>8} in / {:>8} out / {:>8} cache",
        format!("Tokens ({})", month_label),
        fmt_tokens_compact(month_tokens.input),
        fmt_tokens_compact(month_tokens.output),
        fmt_tokens_compact(month_tokens.cache_read + month_tokens.cache_creation));

    // ── Verbose: per-session table ────────────────────────────────────────────

    if verbose {
        // Collect spend map.
        let spend_map = storage::get_spend_by_session().unwrap_or_default();

        println!();
        println!("{:<10}  {:<24}  {:<30}  {:<8}  {}",
            "Date", "Project", "Title", "Spend", "Sync");
        println!("{}", "─".repeat(82));

        for s in &db_sessions {
            // Determine date from last_activity_at.
            let date = s.last_activity_at.as_deref()
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown   ".to_string());

            // Project: last path component of project_path, truncated to 24 chars.
            let project_raw = s.project_path.as_deref().unwrap_or("");
            let project_display = {
                let last = project_raw.rsplit('/').next().unwrap_or(project_raw);
                if last.len() > 24 { &last[..24] } else { last }
            };

            // Title: truncate to 30 chars or show placeholder.
            let title_display = match &s.title {
                Some(t) => {
                    if t.chars().count() > 30 {
                        let truncated: String = t.chars().take(27).collect();
                        format!("{}...", truncated)
                    } else {
                        t.clone()
                    }
                }
                None => "(no title)".to_string(),
            };

            // Spend.
            let spend = spend_map.get(&s.session_id).copied().unwrap_or(0.0);
            let spend_display = format!("${:.2}", spend);

            // Sync status.
            let sync_status = if let Some((_, current_mtime)) = log_file_map.get(&s.session_id) {
                match (&s.file_mtime, current_mtime) {
                    (Some(stored), Some(current)) if stored == current => "✓",
                    (Some(_), Some(_)) => "stale",
                    (None, Some(_)) => "new",
                    _ => "?",
                }
            } else {
                "gone"
            };

            println!("{:<10}  {:<24}  {:<30}  {:<8}  {}",
                date, project_display, title_display, spend_display, sync_status);
        }
    }

    Ok(())
}

/// Silently sweep all Claude log files, inserting or updating any session that has changed.
///
/// B2: The old liveness guard (`looks_active` / `ACTIVE_LOG_WINDOW`) is removed. Re-parsing a
/// running session is now safe — no synthetic `session_end` is written, and `replace_session`
/// is idempotent. The sweep runs every 30 s in the serve loop (B3) so the spend figure stays
/// fresh without manual intervention.
///
/// B3 change detection: the main file's (size, mtime) and composite subagent totals are
/// compared against what is stored in `sessions.file_size` / `sessions.file_mtime`. If nothing
/// changed, that session is skipped. After a successful parse+replace the columns are updated.
fn run_log_reconciliation() -> Result<ReconcileStats> {
    use backfill::BackfillAction;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.exists() {
        return Ok(ReconcileStats { n_new: 0, n_updated: 0, n_unchanged: 0 });
    }

    let paths = backfill::discover_sessions(&projects_dir, None, None)?;
    let mut n_new = 0usize;
    let mut n_updated = 0usize;
    let mut n_unchanged = 0usize;

    for path in &paths {
        // B3: change detection — skip re-parsing if the file hasn't changed.
        //
        // We need the session_id to look up stored meta, but we don't want to parse the full
        // file just for the id. Read only the first non-empty line to extract sessionId cheaply.
        let session_id_hint = peek_session_id(path);

        if let Some(ref sid) = session_id_hint {
            if let Ok(Some((stored_size, stored_mtime))) = storage::get_session_file_meta(sid) {
                if let Some((current_size, current_mtime)) = composite_file_meta(path) {
                    if current_size == stored_size && current_mtime == stored_mtime {
                        n_unchanged += 1;
                        continue; // Nothing changed — skip re-parse.
                    }
                }
            }
        }

        if let Ok(Some(session)) = backfill::parse_session_log(path) {
            match backfill::backfill_session(&session, false) {
                Ok(BackfillAction::Inserted) => {
                    n_new += 1;
                    update_file_meta_for_path(&session.session_id, path, &session.last_activity_at);
                }
                Ok(BackfillAction::Replaced) => {
                    n_updated += 1;
                    update_file_meta_for_path(&session.session_id, path, &session.last_activity_at);
                }
                _ => {}
            }
        }
    }

    Ok(ReconcileStats { n_new, n_updated, n_unchanged })
}

/// Read only the first non-empty line of a JSONL file and extract the `sessionId` field.
///
/// Cheap enough to call in the reconciliation hot path — avoids a full file parse just to
/// look up stored metadata.
fn peek_session_id(path: &std::path::Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(f);
    use std::io::BufRead;
    for line in reader.lines() {
        let line = line.ok()?;
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) {
                return Some(sid.to_string());
            }
        }
        break; // Only check the first non-empty line.
    }
    None
}

fn cmd_serve(api_port_override: Option<u16>, _otel_port_override: Option<u16>) -> Result<()> {
    use trakr::config;
    use trakr::otel_receiver;
    use trakr::server::{AppState, start_server};

    let cfg = config::load_config()?;
    let api_port = api_port_override.unwrap_or(cfg.api_port);

    storage::init_db()?;

    let trakr_dir = dirs::home_dir().map(|p| p.join(".trakr").display().to_string()).unwrap_or_else(|| "unknown".to_string());
    tlog!("trakr: daemon starting  budget=${:.2}  sync={}s  api={}  home={}",
        cfg.monthly_budget_usd,
        cfg.sync_interval_secs,
        if cfg.api_enabled { format!("enabled (:{} )", cfg.api_port) } else { "disabled".to_string() },
        trakr_dir,
    );

    let rt = tokio::runtime::Runtime::new().context("creating tokio runtime")?;
    rt.block_on(async move {
        let costs = otel_receiver::new_session_costs();

        let state = AppState {
            costs: costs.clone(),
            budget_usd: cfg.monthly_budget_usd,
        };

        let sync_interval = cfg.sync_interval_secs;

        tokio::spawn(async move {
            loop {
                let result = tokio::task::spawn_blocking(run_log_reconciliation).await;
                match result {
                    Ok(Err(e)) => tlog!("trakr: sync warning: {:#}", e),
                    Err(e)    => tlog!("trakr: sync task panicked: {:#}", e),
                    Ok(Ok(stats)) => {
                        if stats.n_new > 0 || stats.n_updated > 0 {
                            tlog!("trakr: syncing: {} new, {} updated, {} unchanged",
                                stats.n_new, stats.n_updated, stats.n_unchanged);
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(sync_interval)).await;
            }
        });

        // Spawn a daily rates refresh task. Runs once at startup then every 24 h.
        tokio::spawn(async {
            loop {
                let result = tokio::task::spawn_blocking(|| trakr::rates::refresh_rates()).await;
                match result {
                    Ok(Ok(n))  => tlog!("trakr: rates refreshed: {} Claude models", n),
                    Ok(Err(e)) => tlog!("trakr: rates refresh warning: {:#}", e),
                    Err(e)     => tlog!("trakr: rates task panicked: {:#}", e),
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(86_400)).await;
            }
        });

        // C2: spawn the daily archive sweep as a background task.
        // Runs once at startup and then every 24 h.  Uses spawn_blocking because
        // run_archive_sweep is sync I/O.  Errors are logged but do not crash the server.
        tokio::spawn(async {
            loop {
                let result = tokio::task::spawn_blocking(|| {
                    let home = dirs::home_dir()
                        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
                    let projects_dir = home.join(".claude").join("projects");
                    let archive_dir = home.join(".trakr").join("archive");
                    if !projects_dir.exists() {
                        return Ok(());
                    }
                    std::fs::create_dir_all(&archive_dir)
                        .with_context(|| format!("creating archive dir {}", archive_dir.display()))?;
                    let stats = archive::run_archive_sweep(&projects_dir, &archive_dir)?;
                    if stats.copied > 0 {
                        tlog!("trakr: archive sweep: {} file(s) copied ({} bytes), {} unchanged",
                            stats.copied, stats.bytes_copied, stats.unchanged);
                    }
                    Ok::<(), anyhow::Error>(())
                })
                .await;
                match result {
                    Ok(Err(e)) => tlog!("trakr: archive warning: {:#}", e),
                    Err(e)    => tlog!("trakr: archive task panicked: {:#}", e),
                    Ok(Ok(())) => {}
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(86_400)).await;
            }
        });

        tokio::spawn(async {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut sig) = signal(SignalKind::terminate()) {
                sig.recv().await;
                tlog!("trakr: daemon stopping");
                std::process::exit(0);
            }
        });

        if cfg.api_enabled {
            start_server(api_port, state).await;
            tlog!("trakr: daemon stopping");
        } else {
            // API disabled — park the runtime so the background loops keep running.
            std::future::pending::<()>().await;
        }
    });

    Ok(())
}

fn cmd_spend(json: bool) -> Result<()> {
    use trakr::config;

    let cfg = config::load_config()?;
    let year_month = Utc::now().format("%Y-%m").to_string();

    storage::init_db()?;

    if json {
        // Fast path: DB read only, no reconciliation. The serve daemon keeps the DB current.
        let (spent, count) = storage::get_monthly_spend_usd(&year_month)?;
        let pct = if cfg.monthly_budget_usd > 0.0 { spent / cfg.monthly_budget_usd * 100.0 } else { 0.0 };
        println!(
            r#"{{"month":"{month}","spent_usd":{spent:.2},"budget_usd":{budget:.2},"pct":{pct:.1},"sessions":{count}}}"#,
            month  = year_month,
            spent  = spent,
            budget = cfg.monthly_budget_usd,
            pct    = pct,
            count  = count,
        );
        return Ok(());
    }

    // B4: always use SQLite — no OTEL term, no live-API-vs-SQLite split.
    // Run one inline incremental sweep before reading; sub-second when nothing changed
    // because the change-detection logic skips unchanged sessions without a full re-parse.
    if let Err(e) = run_log_reconciliation() {
        eprintln!("trakr: reconciliation warning: {:#}", e);
    }
    // Stats from the inline sweep are intentionally discarded here.

    let (spent, count) = storage::get_monthly_spend_usd(&year_month)?;
    let now_local = chrono::Local::now();
    let month_label = now_local.format("%B %Y").to_string();
    let pct = if cfg.monthly_budget_usd > 0.0 { spent / cfg.monthly_budget_usd * 100.0 } else { 0.0 };
    let day = now_local.day();
    let suffix = match day % 100 {
        11 | 12 | 13 => "th",
        _ => match day % 10 { 1 => "st", 2 => "nd", 3 => "rd", _ => "th" },
    };
    let human_ts = format!("{}{} {} @ {} ({})",
        day, suffix,
        now_local.format("%B %Y"),
        now_local.format("%H:%M:%S"),
        now_local.format("%:z"),
    );

    const LW: usize = 12;
    const VW: usize = 10;
    const W: usize = 2 + LW + 1 + VW;
    let cost_val   = format!("{:>width$}", format!("${:.2}", spent),                  width = VW);
    let budget_val = format!("{:>width$}", format!("${:.2}", cfg.monthly_budget_usd), width = VW);
    let used_val   = format!("{:>width$}", format!("{:.1}%", pct),                    width = VW);

    println!("Spend for {} ({} sessions)", month_label, count);
    println!("{}", "-".repeat(W));
    println!("  {:<width$} {}", "Cost",   cost_val,   width = LW);
    println!("  {:<width$} {}", "Budget", budget_val, width = LW);
    println!("  {:<width$} {}", "Used",   used_val,   width = LW);
    println!("{}", "-".repeat(W));
    println!("Last updated: {}", human_ts);

    Ok(())
}

fn cmd_sync() -> Result<()> {
    storage::init_db()?;

    println!("Syncing session logs...");

    let stats = run_log_reconciliation()?;

    println!(
        "  Reconciled: {} new, {} updated, {} unchanged",
        stats.n_new, stats.n_updated, stats.n_unchanged
    );

    let now_local = chrono::Local::now();
    let day = now_local.day();
    let suffix = match day % 100 {
        11 | 12 | 13 => "th",
        _ => match day % 10 { 1 => "st", 2 => "nd", 3 => "rd", _ => "th" },
    };
    let human_ts = format!("{}{} {} @ {} ({})",
        day, suffix,
        now_local.format("%B %Y"),
        now_local.format("%H:%M:%S"),
        now_local.format("%:z"),
    );
    println!("Last updated: {}", human_ts);

    Ok(())
}

fn cmd_sync_rates() -> Result<()> {
    use trakr::rates;
    use std::io::Write as IoWrite;

    let (msg, summary) = match rates::refresh_rates() {
        Ok(n)  => (
            format!("{} trakr: rates refreshed: {} Claude models\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), n),
            format!("Rates synced ({} models)", n),
        ),
        Err(e) => (
            format!("{} trakr: rates refresh failed: {:#}\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), e),
            format!("Rates sync failed: {:#}", e),
        ),
    };

    // Append to serve.log so the event is visible alongside daemon output.
    if let Ok(home) = dirs::home_dir().ok_or(()) {
        let log_path = home.join(".trakr").join("serve.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = f.write_all(msg.as_bytes());
        }
    }

    println!("{}", summary);
    Ok(())
}

fn cmd_status() -> Result<()> {
    use trakr::config;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let cfg = config::load_config()?;
    let mut problems: Vec<String> = Vec::new();

    let ok = |good: bool| if good { "✓" } else { "✗" };
    let info = |_: bool| "ℹ";

    // ── Transcripts pipeline (health signals) ──────────────────────────────────

    println!("Transcripts pipeline  (primary health signal)");
    println!("{}", "-".repeat(60));

    let projects_dir = home.join(".claude").join("projects");
    let projects_readable = projects_dir.exists()
        && std::fs::read_dir(&projects_dir).is_ok();
    println!("  {} {:<32} {}",
        ok(projects_readable), "Claude projects dir",
        if projects_readable {
            projects_dir.display().to_string()
        } else {
            format!("{} (not readable)", projects_dir.display())
        });
    if !projects_readable {
        problems.push(format!("Claude projects dir not readable: {}", projects_dir.display()));
    }

    let base = home.join(".trakr");
    let db_path = base.join("trakr.db");
    let db_summary = if db_path.exists() { storage::get_db_summary().ok().flatten() } else { None };
    match &db_summary {
        Some((earliest, latest, count)) => {
            println!("  {} {:<32} {} sessions  ({} → {})", ok(true), "trakr.db",
                count, earliest.format("%Y-%m-%d"), latest.format("%Y-%m-%d"));
        }
        None => {
            println!("  {} {:<32} {}", ok(false), "trakr.db",
                if db_path.exists() { "empty" } else { "missing — run `trakr init`" });
            if !db_path.exists() {
                problems.push("DB missing — run `trakr init`".to_string());
            }
        }
    }

    // DB freshness: compare newest transcript mtime vs newest DB last_activity_at.
    if projects_readable && db_summary.is_some() {
        let newest_transcript_mtime = find_newest_mtime(&projects_dir);
        let newest_db_activity = storage::get_active_session_count(86400 * 7).ok(); // any in the last week
        let _ = newest_db_activity; // used for display only
        match newest_transcript_mtime {
            Some(tm) => {
                let age_mins = Utc::now().signed_duration_since(tm).num_minutes();
                let age_str = if age_mins < 60 {
                    format!("{} min ago", age_mins)
                } else if age_mins < 1440 {
                    format!("{}h {}m ago", age_mins / 60, age_mins % 60)
                } else {
                    format!("{}d {}h ago", age_mins / 1440, (age_mins % 1440) / 60)
                };
                println!("  {} {:<32} newest transcript {}",
                    ok(age_mins < 120), "DB freshness", age_str);
            }
            None => {
                println!("  {} {:<32} no transcripts found", ok(false), "DB freshness");
            }
        }
    }

    // ── Storage ────────────────────────────────────────────────────────────────

    println!();
    println!("Storage  ({})", base.display());
    println!("{}", "-".repeat(60));

    let config_exists = config::config_path()?.exists();
    println!("  {} {:<32} budget ${:.2}, api :{}{}",
        ok(config_exists), "config.toml",
        cfg.monthly_budget_usd, cfg.api_port,
        if config_exists { "" } else { "  (file missing — using defaults)" });

    let transcripts_dir = base.join("transcripts");
    let transcript_count = std::fs::read_dir(&transcripts_dir)
        .map(|rd| rd.filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |x| x == "jsonl"))
            .count())
        .unwrap_or(0);
    println!("  {} {:<32} {} archived", ok(transcripts_dir.exists()), "transcripts/", transcript_count);

    let rates_label = match trakr::rates::last_fetched_at() {
        Some(dt) => {
            let age_mins = Utc::now().signed_duration_since(dt).num_minutes();
            let when = if age_mins < 60 {
                format!("{} min ago", age_mins)
            } else if age_mins < 1440 {
                format!("{} h ago", age_mins / 60)
            } else {
                format!("{} days ago", age_mins / 1440)
            };
            format!("fetched {}  (run `trakr sync-rates` to refresh)", when)
        }
        None => "not fetched yet — run `trakr sync-rates`".to_string(),
    };
    let rates_fresh = trakr::rates::last_fetched_at()
        .map(|dt| Utc::now().signed_duration_since(dt).num_hours() < 48)
        .unwrap_or(false);
    println!("  {} {:<32} {}", ok(rates_fresh), "rates.json", rates_label);

    // ── Server (informational) ─────────────────────────────────────────────────

    println!();
    println!("Service  (reconciliation loop — keeps DB current every 30 s)");
    println!("{}", "-".repeat(60));

    let plist_installed = launch_agent_plist_path().map(|p| p.exists()).unwrap_or(false);
    println!("  {} {:<32} {}", info(plist_installed), "launchd service",
        if plist_installed { LAUNCH_AGENT_LABEL } else { "not installed — run `trakr install-service`" });

    if cfg.api_enabled {
        let api_base = format!("http://127.0.0.1:{}", cfg.api_port);
        let api_up = try_get(&format!("{}/spend/monthly", api_base)).is_some();
        println!("  {} {:<32} {}", info(api_up), "API server",
            if api_up { format!("{} (responding)", api_base) }
            else { format!("{} not responding", api_base) });
    } else {
        println!("  {} {:<32} disabled (set api_enabled = true in config.toml to enable)",
            info(false), "API server");
    }

    // ── Summary ────────────────────────────────────────────────────────────────

    println!();
    if problems.is_empty() {
        println!("All checks passed.");
    } else {
        println!("{} problem(s) found:", problems.len());
        for p in &problems {
            println!("  • {}", p);
        }
    }

    Ok(())
}

/// Walk `dir` recursively (depth 1 only for Claude projects) and return the most recent mtime.
fn find_newest_mtime(projects_dir: &std::path::Path) -> Option<chrono::DateTime<Utc>> {
    let mut newest: Option<chrono::DateTime<Utc>> = None;
    let Ok(entries) = std::fs::read_dir(projects_dir) else { return None };
    for entry in entries.filter_map(|e| e.ok()) {
        let proj = entry.path();
        if !proj.is_dir() { continue; }
        let Ok(sub_entries) = std::fs::read_dir(&proj) else { continue };
        for sub in sub_entries.filter_map(|e| e.ok()) {
            let p = sub.path();
            if p.extension().map_or(true, |e| e != "jsonl") { continue; }
            if let Ok(meta) = std::fs::metadata(&p) {
                if let Ok(mtime_sys) = meta.modified() {
                    let mtime: chrono::DateTime<Utc> = mtime_sys.into();
                    if newest.map_or(true, |n| mtime > n) {
                        newest = Some(mtime);
                    }
                }
            }
        }
    }
    newest
}

/// Repair DB rows built by the old (over-counting) parser.
///
/// For every session whose transcript still exists under `~/.claude/projects/`
/// or `~/.trakr/transcripts/`, deletes stale events and re-backfills from the
/// transcript. Pass `--dry-run` to preview without writing. Default is to apply.
///
/// Sessions with no surviving transcript are left untouched; a count is printed.
fn cmd_repair(dry_run: bool) -> Result<()> {

    storage::init_db()?;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");
    let trakr_transcripts = home.join(".trakr").join("transcripts");

    // Build a map from session_id → transcript path for every surviving file.
    let mut transcript_index: std::collections::HashMap<String, std::path::PathBuf> =
        std::collections::HashMap::new();

    // Search ~/.claude/projects/<project>/<uuid>.jsonl
    if projects_dir.exists() {
        let log_paths = backfill::discover_sessions(&projects_dir, None, None)?;
        for path in log_paths {
            // Extract session_id from the file stem or first line.
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                // Try to get the real session_id from the file (stem may be the uuid but
                // the internal sessionId is canonical).
                let sid = peek_session_id(&path)
                    .unwrap_or_else(|| stem.to_string());
                transcript_index.insert(sid, path);
            }
        }
    }

    // Also search ~/.trakr/transcripts/<session_id>.jsonl (hook-archived copies).
    if trakr_transcripts.exists() {
        let Ok(entries) = std::fs::read_dir(&trakr_transcripts) else { /* skip */ return Ok(()) };
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.extension().map_or(true, |e| e != "jsonl") { continue; }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                // Only add if not already found in projects_dir (prefer native path).
                transcript_index.entry(stem.to_string()).or_insert(p);
            }
        }
    }

    // Load all sessions from the DB.
    let db_sessions = storage::get_all_session_records()?;
    let completed_ids = storage::get_completed_session_ids()?;

    let mut to_rebuild: Vec<(String, std::path::PathBuf, bool)> = Vec::new();
    let mut legacy_no_transcript = 0usize;

    for record in &db_sessions {
        let sid = &record.session_id;
        let has_synthetic_end = completed_ids.contains(sid)
            && record.source.as_deref() == Some("backfill");

        if let Some(path) = transcript_index.get(sid) {
            to_rebuild.push((sid.clone(), path.clone(), has_synthetic_end));
        } else {
            legacy_no_transcript += 1;
        }
    }

    if dry_run {
        println!("DRY RUN — no changes will be written.");
        println!();
        println!("{:<38}  {:<32}  has_synthetic_end",
            "session_id", "transcript_path");
        println!("{}", "-".repeat(95));
        for (sid, path, synthetic) in &to_rebuild {
            let path_str = path.display().to_string();
            let short_path = if path_str.len() > 32 {
                format!("...{}", &path_str[path_str.len() - 29..])
            } else {
                path_str
            };
            println!("{:<38}  {:<32}  {}",
                if sid.len() > 38 { &sid[..38] } else { sid },
                short_path,
                if *synthetic { "yes" } else { "no" }
            );
        }
        println!();
        println!("{} session(s) to rebuild.", to_rebuild.len());
        println!("{} session(s) retain legacy (inflated) figures — raw transcript lost.", legacy_no_transcript);
    } else {
        // --run: delete and re-backfill.
        let mut n_rebuilt = 0usize;
        let mut n_failed = 0usize;
        for (sid, path, _) in &to_rebuild {
            // Delete all events for this session.
            if let Err(e) = storage::delete_events_for_session(sid) {
                eprintln!("trakr repair: failed to delete events for {}: {}", &sid[..8.min(sid.len())], e);
                n_failed += 1;
                continue;
            }
            // Re-backfill from transcript.
            match backfill::parse_session_log(path) {
                Ok(Some(session)) => {
                    match backfill::backfill_session(&session, false) {
                        Ok(_) => {
                            update_file_meta_for_path(&session.session_id, path, &session.last_activity_at);
                            n_rebuilt += 1;
                        }
                        Err(e) => {
                            eprintln!("trakr repair: backfill failed for {}: {}", &sid[..8.min(sid.len())], e);
                            n_failed += 1;
                        }
                    }
                }
                Ok(None) => {
                    eprintln!("trakr repair: could not parse transcript for {} (empty/no session_id)", &sid[..8.min(sid.len())]);
                    n_failed += 1;
                }
                Err(e) => {
                    eprintln!("trakr repair: parse error for {}: {}", &sid[..8.min(sid.len())], e);
                    n_failed += 1;
                }
            }
        }
        println!("Repair complete: {} rebuilt, {} failed, {} with no transcript (left untouched).",
            n_rebuilt, n_failed, legacy_no_transcript);
    }

    Ok(())
}

/// Attempt a blocking HTTP GET; returns the body on 200, None otherwise.
fn try_get(url: &str) -> Option<String> {
    use std::io::Read;
    use std::net::TcpStream;

    let addr = url
        .trim_start_matches("http://")
        .split('/')
        .next()?;

    let path = url.trim_start_matches("http://").trim_start_matches(addr);

    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).ok()?;

    let request = format!("GET {} HTTP/1.0\r\nHost: {}\r\n\r\n", path, addr);
    std::io::Write::write_all(&mut stream, request.as_bytes()).ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    // Split headers from body.
    let body = response.split("\r\n\r\n").nth(1)?;
    if response.starts_with("HTTP/1") && response.contains(" 200 ") {
        Some(body.to_string())
    } else {
        None
    }
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

fn cmd_logs(lines: usize) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let log_path = home.join(".trakr").join("serve.log");

    if !log_path.exists() {
        println!("No log file found at {} — is trakr serve running?", log_path.display());
        return Ok(());
    }

    let status = std::process::Command::new("tail")
        .args(["-n", &lines.to_string(), "-f", log_path.to_str().unwrap()])
        .status()
        .context("running tail")?;

    if !status.success() {
        anyhow::bail!("tail exited with status {}", status);
    }

    Ok(())
}

const LAUNCH_AGENT_LABEL: &str = "io.ubiqtek.trakr.serve";

fn launch_agent_plist_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    Ok(home.join("Library").join("LaunchAgents").join(format!("{}.plist", LAUNCH_AGENT_LABEL)))
}

fn cmd_install_service() -> Result<()> {
    let binary = std::env::current_exe().context("cannot determine trakr binary path")?;
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let log_path = home.join(".trakr").join("serve.log");
    let plist_path = launch_agent_plist_path()?;

    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).context("creating LaunchAgents directory")?;
    }

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = LAUNCH_AGENT_LABEL,
        binary = binary.display(),
        log = log_path.display(),
    );

    std::fs::write(&plist_path, &plist)
        .with_context(|| format!("writing plist to {}", plist_path.display()))?;

    let status = std::process::Command::new("launchctl")
        .args(["load", plist_path.to_str().unwrap()])
        .status()
        .context("running launchctl load")?;

    if !status.success() {
        anyhow::bail!("launchctl load failed — plist written to {}", plist_path.display());
    }

    println!("trakr: service installed and started");
    println!("trakr: plist:  {}", plist_path.display());
    println!("trakr: log:    {}", log_path.display());
    println!("trakr: runs automatically on login");

    Ok(())
}

fn cmd_uninstall_service() -> Result<()> {
    let plist_path = launch_agent_plist_path()?;

    if !plist_path.exists() {
        println!("trakr: no service installed (plist not found at {})", plist_path.display());
        return Ok(());
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", plist_path.to_str().unwrap()])
        .status();

    std::fs::remove_file(&plist_path)
        .with_context(|| format!("removing plist at {}", plist_path.display()))?;

    println!("trakr: service stopped and removed");

    Ok(())
}

/// Copy Claude transcripts from `~/.claude/projects/` into `~/.trakr/archive/` (incremental).
///
/// C1: Creates `~/.trakr/archive/` if it doesn't exist, then runs a single archive sweep.
/// Files are mirrored by relative path; only new or changed files are copied.
fn cmd_archive() -> Result<()> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let projects_dir = home.join(".claude").join("projects");
    let archive_dir = home.join(".trakr").join("archive");

    if !projects_dir.exists() {
        println!("No Claude projects directory found at {} — nothing to archive.", projects_dir.display());
        return Ok(());
    }

    std::fs::create_dir_all(&archive_dir)
        .with_context(|| format!("creating archive dir {}", archive_dir.display()))?;

    let stats = archive::run_archive_sweep(&projects_dir, &archive_dir)?;

    println!(
        "Archive complete: {} file(s) copied ({} bytes), {} unchanged.",
        stats.copied, stats.bytes_copied, stats.unchanged
    );

    Ok(())
}

fn fmt_tokens_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
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
