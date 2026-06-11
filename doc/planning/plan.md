# Implementation Plan

## ── WHAT'S NEXT ──────────────────────────────────────────────────────────
**Next:** Action 4d.1 — Archive full Claude session transcripts at SessionEnd
**Sub-doc:** (none)
**Blockers:** None — `transcript_path` already available in SessionEnd hook payload
─────────────────────────────────────────────────────────────────────────────

## Phase 1: Project Foundation

### Action 1.1: Initialize Rust project
- ✓ DONE - Create Cargo.toml with base dependencies (serde, serde_json, chrono, rusqlite)
- ✓ DONE - Set up project structure: src/main.rs, src/lib.rs, src/hooks.rs, src/event.rs, src/storage.rs, src/transcript.rs
- ✓ DONE - Configure for crates.io publishing (metadata, license, docs)

### Action 1.2: Hook interface design
- ✓ DONE - Define data structures for captured events (ToolUse, SessionStart, SessionEnd, SubagentStart, SubagentStop, ContextCompression, TokenUsage, Other)
- ✓ DONE - Create JSON schema for hook payloads (implicit via serde)
- ✓ DONE - Design session storage format (SQLite unified DB + JSONL backups)

### Action 1.3: Core types & serialisation
- ✓ DONE - Implement Event enum with all variants in src/event.rs
- ✓ DONE - Add serde derives for JSON interchange
- ✓ DONE - Add unit tests for event serialization/deserialization

## Phase 2: Hook Integration

### Action 2.1: Hook listener
- ✓ DONE - Build hook command wrapper (src/main.rs handles `hook` subcommand)
- ✓ DONE - Parse JSON from Claude Code hooks (src/hooks.rs)
- ✓ DONE - Append events to session storage (src/storage.rs with dual SQLite + JSONL)
- ✓ DONE - Handle transcript parsing for token usage (src/transcript.rs)

### Action 2.2: Session management
- ✓ DONE - Implement session initialisation and directory structure (cmd_init)
- ✓ DONE - Track sessions by ID (string-based session identifiers)
- ✓ DONE - Store per-session event log with metadata in unified SQLite DB
- ✓ DONE - Create JSONL backup files for each session
- ✓ DONE - Implement migration from JSONL to unified DB (cmd_migrate)

### Action 2.3: Hook documentation
- ✓ DONE - Suggested hook config printed by `ctx-trakr init`
- ✓ DONE - Hook types documented in code comments (tool-use, session-start, session-end)
- ✓ DONE - Full README with installation, hook setup, all commands, config reference

## Phase 3: Spend Tracking & Status Line

### Action 3.1: Cost estimation
- ✓ DONE - Rate card in src/cost.rs (Haiku/Sonnet/Opus/Fable, June 2026 pricing)
- ✓ DONE - compute_cost_usd() — input/output/cache_creation/cache_read token costs
- ✓ DONE - get_monthly_spend_usd() in storage.rs — last TokenUsage per completed session × rate card
- ✓ DONE - Unit tests for all model tiers and cache token types (6 tests)

### Action 3.2: Budget config
- ✓ DONE - src/config.rs — TOML config at ~/.ctx-trakr/config.toml
- ✓ DONE - Fields: monthly_budget_usd (default 50.0), api_port (8787), otel_port (4318)
- ✓ DONE - write_default_config() called from cmd_init; safe to re-run (no-op if exists)
- ✓ DONE - Unit tests (defaults, custom budget, idempotent write)

### Action 3.3: OTEL receiver
- ✓ DONE - src/otel_receiver.rs — OTLP HTTP/JSON receiver (axum, port 4318 by default)
- ✓ DONE - Parses claude_code.cost.usage metric; handles both gauge and sum data point shapes
- ✓ DONE - Extracts session_id from data-point attributes, falls back to resource attributes, then "unknown"
- ✓ DONE - SessionCosts type: Arc<Mutex<HashMap<session_id, f64>>> shared with API server
- ✓ DONE - Unit tests for attribute extraction, accumulation, fallback behaviour (5 tests)
- NOTE: requires OTEL_EXPORTER_OTLP_PROTOCOL=http/json — protobuf not supported in v1

### Action 3.4: HTTP API server
- ✓ DONE - src/server.rs — axum HTTP server (port 8787 by default)
- ✓ DONE - GET /spend/monthly — SQLite completed sessions + OTEL active sessions, double-count guard
- ✓ DONE - Response: period, spent_estimated_usd, budget_usd, sources breakdown, note label
- ✓ DONE - ctx-trakr serve subcommand — starts server + OTEL receiver via tokio::runtime (sync CLI unaffected)
- ✓ DONE - ctx-trakr spend subcommand — SQLite-only quick check, no server required

## Phase 4: Querying & Analysis

### Action 4.1: Query CLI
- ✓ DONE - `ctx-trakr list` — lists all sessions with event counts
- ✓ DONE - `ctx-trakr show <session>` — human-readable event timeline
- ✓ DONE - `ctx-trakr stats` — top tools, token totals, model distribution, session list
- TODO - Filtering by tool, model, date range
- TODO - JSON output flag

### Action 4.2: Export/reporting
- TODO - JSON export for analysis pipelines
- TODO - Session timeline visualisation (text-based)

## Phase 4b: Backfill from Claude Code Session Logs

Design doc: `doc/claude-session-logs.md`

### Action 4b.1: Discovery and parsing (`src/backfill.rs`)
- ✓ DONE - `discover_sessions(projects_dir, project_filter, since_filter) → Vec<SessionLogFile>`
  - Scan `~/.claude/projects/*/` for `.jsonl` files at depth 1
  - Optional substring filter on encoded project path (`--project`)
  - Optional date filter on file mtime (`--since YYYY-MM-DD`)
- ✓ DONE - `parse_session_log(path) → BackfilledSession`
  - Walk all entries; extract `sessionId`, `timestamp` from every line
  - Accumulate tool uses from `assistant.message.content[]` blocks with `type:"tool_use"`
  - Sum `message.usage` across all `assistant` entries (per-turn, not cumulative)
  - Model from first `assistant` entry with a non-empty `message.model`
  - Produce: `SessionStart { source: "backfill" }`, N × `ToolUse`, one `TokenUsage` (summed), `SessionEnd`

### Action 4b.2: Idempotent insertion (`src/storage.rs` + `src/backfill.rs`)
- ✓ DONE - `delete_events_for_session(session_id)` in `src/storage.rs`
- ✓ DONE - `replace_session()` transactional delete+insert in `src/storage.rs`
- ✓ DONE - `backfill_session(session, dry_run) → BackfillResult` in `src/backfill.rs`
  - Skip if DB has **both** `session_start` AND `session_end` for this session_id
  - If partial data exists (no `session_end`): delete existing events, insert full backfilled stream
  - If no data exists: insert full backfilled stream
  - In dry-run mode: print what would happen, write nothing

### Action 4b.3: CLI subcommand (`src/main.rs`)
- ✓ DONE - `BackfillLogs` subcommand with flags: `--project <substr>`, `--since <YYYY-MM-DD>`, `--dry-run`
- ✓ DONE - `InspectLogs` subcommand — lists discovered sessions with tracking status and log stats
- ✓ DONE - `ShowPrompts` subcommand — shows first/last entries per session from raw log
- ✓ DONE - Per-session status output: `[skip]`, `[new]`, `[replace]`
- ✓ DONE - Summary: N new, N replaced, N skipped
- ✓ DONE - `backfill` module exported from `src/lib.rs`

### Action 4b.4: Tests
- ✓ DONE - Unit tests for `parse_session_log`: tool use extraction, token summation, model fallback, empty file
- ✓ DONE - Unit tests for idempotency: skip-on-complete, replace-on-partial, safe re-run
- ✓ DONE - Unit test for `discover_sessions`: project filter, since filter
- NOTE - Skip rule is: skip only if BOTH `session_start` AND `session_end` present (not just `session_end`)

### Action 4b.5: Hook event name audit
- ✓ DONE - Confirmed correct Claude Code hook event names: `SessionStart`, `SessionEnd`, `PreToolUse`
  - NOT `Stop` (old incorrect assumption) — the real name is `SessionEnd`
  - NOT `PostToolUse` — current config uses `PreToolUse`
- ✓ DONE - Documented in `doc/claude-hooks.md`
- ✓ DONE - Fixed `ctx-trakr init` suggested config and auto-writes correct hooks to `~/.claude/settings.json`

## Phase 4c: Architecture Hardening

### Action 4c.1: SessionEnd → full JSONL parse pipeline
- ✓ DONE - `handle_session_end` now calls `backfill::parse_session_log` + `storage::replace_session`
  - Accurate summed token counts across all turns (was last-turn-only — $0.24 → $281.70 on fresh backfill)
  - One atomic write per session; idempotent; ground truth from Claude's own log
- ✓ DONE - `handle_tool_use` made no-op (drains stdin, writes nothing); PreToolUse hook removed from config
- ✓ DONE - Fallback to minimal `session_end` insert if transcript missing or unparseable

### Action 4c.2: Project context in DB
- ✓ DONE - New `sessions` table: `session_id PRIMARY KEY, project_path, started_at, ended_at, model, source`
- ✓ DONE - `upsert_session_meta()` in `storage.rs` — COALESCE-based upsert so partial updates don't clobber
- ✓ DONE - Populated from `cwd` in hook payload (real path) and from log file's parent dir name in backfill

### Action 4c.3: SQLite concurrency hardening
- ✓ DONE - `PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;` in `open_db()` — prevents silent `SQLITE_BUSY` loss under multi-session tmux workflow

### Action 4c.4: Hook config correctness
- ✓ DONE - `cmd_init` merges `SessionStart`/`SessionEnd` hooks into `~/.claude/settings.json` directly (idempotent)
- ✓ DONE - `suggested_hook_config()` updated to correct event names; no more `PostToolUse`/`Stop`

### Action 4c.5: Reconciliation sweep
- ✓ DONE - `run_log_reconciliation()` called on `serve` startup — backfills any sessions whose `SessionEnd` hook was missed before Claude's 30-day log retention expires
- ✓ DONE - `inspect-logs` "Log pruned" label fixed to "No log file" with accurate description

### Action 4c.6: DB wipe + fresh backfill
- ✓ DONE - Wiped old piecemeal-hook data and backfilled 50 sessions from Claude logs
  - 50/50 log files matched in DB, 0 orphans, 0 partial sessions

## Phase 4d: Full Session Transcript Storage

Research completed (2026-06-11): Fable agent compared ctx-trakr JSONL vs Claude native JSONL.
Key findings:
- Claude's native JSONL at `~/.claude/projects/<slug>/<uuid>.jsonl` contains full conversation: user prompts, assistant replies, thinking blocks, tool calls with inputs, tool results
- Compact summary/recap IS stored in the JSONL as `type:"user"` lines with `isCompactSummary:true`
- `ai-title` and `last-prompt` lines give cheap ready-made session summaries — no inference needed
- `transcript_path` is already available in the SessionEnd hook payload (`src/hooks.rs:84`)
- No official rotation/pruning policy documented; files appear to persist indefinitely

### Action 4d.1: Archive full Claude transcripts at SessionEnd
- TODO - At `SessionEnd`, copy native JSONL (`transcript_path`) to `~/.ctx-trakr/transcripts/<session-id>.jsonl`
- TODO - Add `transcripts/` dir creation to `cmd_init` and `init_db()`
- TODO - User owns retention; no auto-pruning in ctx-trakr

### Action 4d.2: Extract summary fields into `sessions` table
- TODO - Parse `ai-title` line → `sessions.title` column
- TODO - Parse first `isCompactSummary:true` user message text → `sessions.summary` column
- TODO - Parse `last-prompt` line → `sessions.last_prompt` column (truncated last user turn)
- TODO - Schema migration: add `title`, `summary`, `last_prompt` columns to `sessions` table
- TODO - Populate from both hook path (live) and backfill path

### Action 4d.3: Expose in CLI
- TODO - `ctx-trakr show <session>` — print `title` + `summary` if present
- TODO - `ctx-trakr list` — show title alongside session ID and project

## Phase 5: Polish & Release

### Action 5.1: Testing
- ✓ DONE - Unit tests: 51 passing (updated hooks tests, added upsert_session_meta coverage)
- TODO - CLI command integration tests
- TODO - End-to-end test: hook → storage → spend endpoint

### Action 5.2: Documentation
- ✓ DONE - README: installation, hook setup, spend/serve workflow, tmux status-line example, config reference, all commands, cost table
- TODO - Update README to reflect new SessionEnd-only hook architecture
- TODO - Troubleshooting guide (OTEL not connecting, DB missing, etc.)

### Action 5.3: Crates.io publication
- TODO - Final dependency audit
- TODO - Version 0.1.0 release
- TODO - GitHub Actions CI/CD setup

---

## ── CHECKPOINT: Session 2026-06-10 ──────────────────────────────────────

**What was completed this session:**
- Phase 3 (spend tracking) fully implemented: cost.rs, config.rs, otel_receiver.rs, server.rs
- New deps: tokio, axum, toml
- New CLI commands: `serve`, `spend`
- `init` now writes default config.toml
- README rewritten from scratch (previous version described a different, unimplemented design)
- 42 → 44 passing tests (added cost + config + otel_receiver tests)

**State of the binary:**
- `ctx-trakr spend` works against live DB today ($0.24 / $50.00 from 7 sessions)
- `ctx-trakr serve` starts cleanly; GET /spend/monthly returns correct JSON
- All 44 tests pass; `cargo build` clean

**Known gaps / next priorities:**
1. OTEL protocol — only http/json supported; protobuf would be more compatible with Claude Code defaults. Consider adding `opentelemetry-proto` dep for v2.
2. `session_id` in OTEL — assumed to be present as a data-point or resource attribute. Needs real-world validation against Claude Code's actual OTEL output.
3. `cmd_spend` prints a note directing users to `serve` — could instead try a live HTTP call to the server if it's already running, and fall back to SQLite only.
4. Filtering / JSON output on `list`, `show`, `stats` — still TODO.
5. CI/CD and crates.io publication — still TODO.

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-10 (continued) ──────────────────────────

**What was completed this session (Phase 4b):**
- `src/backfill.rs` — full implementation: `discover_sessions`, `parse_session_log`, `backfill_session`, `inspect_logs`
- `src/storage.rs` — added `get_started_session_ids`, `get_db_summary`, `replace_session`, `delete_events_for_session`; poison-safe mutex unwrap
- New CLI subcommands: `backfill-logs`, `inspect-logs`, `show-prompts`
- `TrackingStatus` tri-state (Missing / Partial / Complete) for accurate inspect-logs output
- Skip rule refined: skip only when BOTH `session_start` AND `session_end` present in DB
- Confirmed Claude Code hook event names: `SessionStart`, `SessionEnd`, `PreToolUse` — documented in `doc/claude-hooks.md`
- Design doc: `doc/claude-session-logs.md`

**State of the binary:**
- `backfill-logs --dry-run` works; shows 12 partial sessions to replace across 6 projects
- `inspect-logs` shows per-session tracking status with tri-state
- `show-prompts` shows first/last log entries with synthesised session boundary markers
- All tests pass (unit tests cover backfill idempotency, parse_session_log, discover_sessions)

**Immediate next steps:**
1. Run `backfill-logs` for real (not dry-run) to fix the 12 partial sessions
2. Fix `ctx-trakr init` suggested config — currently emits wrong hook names (`Stop`, `PostToolUse`)
3. Token semantics inconsistency: hooks record last-turn tokens only; backfill sums all turns — needs a decision (backfill is more accurate; hooks could be updated to also sum)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-11 ──────────────────────────────────────

**What was completed this session (Phase 4c):**
- Full architecture hardening — all 6 items from Fable agent review implemented
- `handle_session_end` now uses `parse_session_log` → `replace_session` (ground-truth, summed tokens)
- `handle_tool_use` is now a no-op; PreToolUse hook removed from config
- New `sessions` table with `project_path`, `started_at`, `ended_at`, `model`, `source`
- `upsert_session_meta()` called from both hook and backfill paths
- WAL mode + 5s busy_timeout on all DB connections
- `cmd_init` writes hooks directly into `~/.claude/settings.json` (idempotent merge)
- Reconciliation sweep on `serve` startup
- DB wiped and backfilled from scratch: 50/50 sessions, 0 orphans

**State of the binary:**
- `ctx-trakr spend` shows $281.70 / $200.00 (accurate — previously $0.24 from last-turn-only tokens)
- `ctx-trakr inspect-logs` shows 50 complete, 0 partial, 0 missing, 0 orphan DB sessions
- 51 tests passing; `cargo build` clean

**Immediate next priorities:**
1. Update README to reflect new SessionEnd-only hook architecture (no PreToolUse)
2. Use `sessions` table in `list`/`stats`/`inspect-logs` to show project context
3. Filtering/JSON output on `list`, `show`, `stats` — still TODO
4. CI/CD and crates.io publication — still TODO

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-11 (transcript research) ────────────────

**What was completed this session:**
- Research spike: Fable agent compared ctx-trakr JSONL vs Claude Code's native session JSONL
- Haiku agent researched official docs on session log format, rotation policy, and recap storage
- Confirmed: Claude's native JSONL contains full conversation transcript (messages, tool calls, results, thinking blocks)
- Confirmed: compact summary recap is stored as `isCompactSummary:true` user messages — no inference needed
- Confirmed: `ai-title` and `last-prompt` lines provide cheap DB-ready session summaries
- No official rotation/pruning policy found — files appear to persist indefinitely
- Designed Phase 4d: transcript archiving + summary extraction plan added to plan.md
- Plan file migrated from `doc/plan.md` → `doc/planning/plan.md`

**State of the project:**
- No code changes this session — research and planning only
- Binary unchanged from end of Phase 4c: 51 tests passing, spend shows $281.70 / $200.00
- `transcript_path` already available in SessionEnd hook payload — implementation can start immediately

**Immediate next priorities:**
1. Action 4d.1 — copy native JSONL to `~/.ctx-trakr/transcripts/` at SessionEnd
2. Action 4d.2 — add `title`, `summary`, `last_prompt` columns to `sessions` table; parse from transcript
3. Action 4d.3 — surface title/summary in `list` and `show` CLI commands
4. Update README to reflect SessionEnd-only hook architecture (carried over from 4c)

─────────────────────────────────────────────────────────────────────────────

## Implementation Notes

### Architecture
- **Three data sources**: hooks→SQLite (completed sessions), OTEL receiver (active sessions), Anthropic Admin API (not available — documented for future)
- **No double-counting**: completed session IDs (have session_end in SQLite) are excluded from the OTEL live total in GET /spend/monthly
- **Cost approximation**: token counts from Claude transcript × published rate card. Cache read = 10% of input rate; cache creation = full input rate
- **Hooks are hot-path**: all hook handlers exit 0 regardless of errors; heavy work (transcript parsing) is best-effort

### File layout
```
src/
  cost.rs           rate card + compute_cost_usd()
  config.rs         TOML config loader + write_default_config()
  otel_receiver.rs  OTLP HTTP/JSON receiver, SessionCosts type
  server.rs         axum API server, GET /spend/monthly
  hooks.rs          hook event handlers
  storage.rs        SQLite + JSONL persistence, get_monthly_spend_usd()
  transcript.rs     Claude JSONL transcript parser
  event.rs          Event enum + serde
  lib.rs            module exports
  main.rs           CLI (clap), all subcommands
```
