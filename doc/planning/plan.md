# Implementation Plan

## ── WHAT'S NEXT ──────────────────────────────────────────────────────────
**Next:** Action 4d.3 — surface `title` + `summary` in `trakr list` and `trakr show`
**Sub-doc:** (none)
**Blockers:** None (spend gap root cause identified and fixed; Haiku background-call gap documented as known limitation)
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
- **[superseded by single-ledger plan]** — OTEL is now informational only; transcripts are the single spend source

### Action 3.4: HTTP API server
- ✓ DONE - src/server.rs — axum HTTP server (port 8787 by default)
- ✓ DONE - GET /spend/monthly — SQLite completed sessions + OTEL active sessions, double-count guard
- ✓ DONE - Response: period, spent_estimated_usd, budget_usd, sources breakdown, note label
- ✓ DONE - ctx-trakr serve subcommand — starts server + OTEL receiver via tokio::runtime (sync CLI unaffected)
- ✓ DONE - ctx-trakr spend subcommand — SQLite-only quick check, no server required
- **[superseded by single-ledger plan]** — OTEL path in /spend/monthly replaced; spend now from transcript token_usage events only

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
- NOTE: originally labelled "OTEL receiver" in early planning — refers to concurrency hardening; not superseded

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
- ✓ DONE - At `SessionEnd`, copy native JSONL (`transcript_path`) to `~/.trakr/transcripts/<session-id>.jsonl`
- ✓ DONE - Add `transcripts/` dir creation to `cmd_init` and `init_db()`
- ✓ DONE - `backfill_session` also archives from `source_path` — backfill path covered
- ✓ DONE - User owns retention; no auto-pruning in trakr

### Action 4d.2: Extract summary fields into `sessions` table
- ✓ DONE - Schema migrations: `schema_migrations` version table, v1 baseline, v2 adds `title`, `summary`, `last_prompt`, `generated_summary` columns
- ✓ DONE - Parse `ai-title` line → `sessions.title` column
- ✓ DONE - Parse first `isCompactSummary:true` user message text → `sessions.summary` column (truncated to 2000 chars)
- ✓ DONE - Parse `last-prompt` line → `sessions.last_prompt` column
- ✓ DONE - Populated from both hook path (live) and backfill path
- NOTE - `generated_summary` column exists, stays null until Haiku inference wired up

### Action 4d.3: Expose in CLI
- TODO - `trakr show <session>` — print `title` + `summary` if present
- TODO - `trakr list` — show title alongside session ID and project
- NOTE - `inspect-logs --verbose` now shows title + per-session spend (2026-06-14)

## Phase 4e: Dynamic Pricing via LiteLLM

### Action 4e.1: Live rate card from LiteLLM
- ✓ DONE - `src/rates.rs` — fetch `https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`; parse Claude model entries; cache to `~/.trakr/rates.json`
- ✓ DONE - `src/cost.rs` refactored — `compute_cost_usd_with_card` (takes `&RateCard`); `compute_cost_usd` loads card from disk on each call; cache_creation rate corrected from 1× to 1.25× input (matches Anthropic published pricing)
- ✓ DONE - `src/storage.rs` — 3 spend query functions load rate card once per function (not per event)
- ✓ DONE - `trakr sync-rates` command — fetches and caches rates, prints "Rates synced (N models)" to stdout, appends timestamped line to `serve.log`
- ✓ DONE - `trakr serve` — daily rates refresh task (runs at startup + every 24 h via `tokio::spawn`)
- ✓ DONE - `trakr status` — Storage section shows when rates were last fetched; warns if > 48 h stale
- ✓ DONE - Daemon startup log now shows `~/.trakr` dir instead of `~`
- NOTE - LiteLLM fetches 237 Claude entries (all provider variants); exact key match on `claude-*` names used by Claude Code; provider-namespaced entries (`anthropic.*`, `bedrock/*`) unused but harmless

## Phase 5: Polish & Release

### Action 5.1: Testing
- ✓ DONE - Unit tests: 51 passing (updated hooks tests, added upsert_session_meta coverage)
- TODO - CLI command integration tests
- TODO - End-to-end test: hook → storage → spend endpoint

### Action 5.2: Documentation
- ✓ DONE - README: installation, hook setup, spend/serve workflow, tmux status-line example, config reference, all commands, cost table
- ✓ DONE - Update README to reflect new SessionEnd-only hook architecture (rewritten 2026-06-11: `trakr` binary name, `~/.trakr/` paths, port 8788, init-writes-everything flow, status/service/logs commands, "How tracking works" section)
- ✓ DONE - Troubleshooting guide (OTEL not connecting, DB missing, etc.) — README Troubleshooting section: OTEL never-received (new-session requirement, ~60 s export interval), http/json-only protocol, port clashes, low spend, stale binary

### Action 5.3: Crates.io publication
- ✓ DONE - Final dependency audit
- ✓ DONE - Version 0.1.0 published to crates.io as `trakr` (package renamed from `ctx-trakr`)
- ✓ DONE - Version 0.1.1 published (crate rename, launch agent label, `inspect` command, token totals, UX fixes)
- ✓ DONE - Version 0.1.2 published (per-model token breakdown in `stats`, `--verbose` session list, compact format)
- TODO - GitHub Actions CI/CD setup

### Action 5.4: CodeQL static analysis
- TODO - Set up CodeQL on this repo (reference: `~/projects/tsk` already has it configured)

---

## ── CHECKPOINT: Session 2026-06-11 (OTEL verified end-to-end + README) ────

**What was completed this session:**
- OTEL pipeline verified end-to-end for the first time with a real Claude Code session:
  - `trakr init` env vars (`CLAUDE_CODE_ENABLE_TELEMETRY`, `OTEL_METRICS_EXPORTER`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`) confirmed picked up by a fresh session
  - Receiver on :4318 ingested live batches; `trakr status` showed `✓ OTEL receiver — 1 batches, 1 active session(s), $0.27`
  - `trakr spend` showed the live line for the first time: 42 completed ($329.69) + active ($0.27) = $329.96, no double-counting
  - Key operational learning (now in README): env changes apply only to NEW sessions, and the first metrics batch lands ~60 s in (Claude Code's export interval) — `trakr status` correctly flags this window as a problem until the first batch arrives
- README rewritten to match the current architecture (closes both remaining Action 5.2 TODOs):
  - `trakr` binary name, `~/.trakr/` paths, API port 8788, SessionStart/SessionEnd-only hooks
  - Quick start reflects that `init` now writes hooks AND env vars itself; added the new-session restart step
  - New sections: "How tracking works" (SessionEnd transcript parse, OTEL gap-fill, reconciliation sweep) and "Troubleshooting"
  - Documented `status`, `install-service`/`uninstall-service`, `logs`, `backfill-logs`, `inspect-logs`, `show-prompts`; updated storage layout (transcripts/, serve.log, sessions table columns)

**State of the project:**
- Full pipeline live: launchd service running `trakr serve` (API :8788, OTEL :4318), hooks rolling sessions into SQLite, OTEL feeding active-session spend. `trakr status` passes all checks. No code changes this session (docs + verification only); binary unchanged, 54 tests passing.
- Untested seam: the active→completed handoff (live cost dropping out of the OTEL total once SessionEnd lands) hasn't been observed for the verifying session yet — worth a glance at the next `trakr spend`.

**Immediate next priorities:**
1. Action 4d.3 — surface `title`/`summary` in `trakr list` and `trakr show`
2. Verify active→completed spend handoff (no double-count, no gap) after a tracked session ends
3. Filtering/JSON output on `list`, `show`, `stats`
4. CI/CD and crates.io publication

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-13 (architecture redesign + single-ledger plan) ────

**What was completed this session:**
- Identified critical bugs in current spend pipeline: ~2.3× output token overstatement (usage duplicated per content block, no `message.id` dedupe) and ~19% usage invisibility (subagent files never scanned)
- Empirical corpus analysis: 126 JSONL files, 61 MB, 23 projects — measured impact in `doc/transcript-structure.md` §3
- Architectural decision: **OTEL and hooks parked** — Claude's transcripts are now the single spend source; dual-pipeline complexity eliminated
- New docs written:
  - `doc/session-lifecycle.md` — three-category session model (known-complete / active / ended-unhooked), 2026-06-11 reconciliation bug
  - `doc/event-sourced-sessions.md` — event sourcing design principles (event store = observed facts only; spend never keys on endings; projection table for derived state)
  - `doc/transcript-structure.md` — empirical format analysis, 2× overstatement finding, three-layer architecture, archive strategy (two decoupled loops)
  - `doc/planning/single-ledger-plan.md` — self-contained Sonnet execution plan for all four phases (A: parser, B: serve loop, C: archive, D: docs)
  - `doc/README.md` — indexed all new docs
- Interim code changes (to be superseded by single-ledger plan):
  - `src/server.rs`: `active_sessions_count` field added to spend response
  - `src/main.rs`: spend CLI shows "Active sessions (N)"; `backfill-logs --force` flag; `[live?]` skip counter
  - `src/backfill.rs`: `looks_active()` mtime guard (`ACTIVE_LOG_WINDOW = 24h`); 3 tests

**State of the project:**
- `trakr serve` still running old binary (launchd service not restarted); working-tree changes uncommitted. `trakr spend` shows ~$330 (likely ~$150–170 real, given the 2.3× overstatement). `cargo build` clean; 54 tests passing (3 new from liveness guard). Two sessions wrongly stamped `session_end` by backfill on 2026-06-11 — will self-heal when they genuinely end.

**Immediate next priorities:**
1. Implement Phase A of `planning/single-ledger-plan.md` — fix dedupe, per-model pricing, subagent inclusion, spend query (the money bugs)
2. Implement Phase B — backfill never writes `session_end`, remove liveness guard, 30 s sampling loop, drop OTEL term from spend endpoint
3. Implement Phase C — `src/archive.rs`, `trakr archive` command, daily timer in serve
4. Run `trakr repair --dry-run` and report; leave the real repair run to Jim
5. Action 4d.3 (title/summary in `list`/`show`) — deprioritised pending single-ledger work

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-13 (single-ledger implementation) ────────

**What was completed this session:**
- Phase A: parser correctness — dedupe by message.id, per-model TokenUsage, subagent inclusion, spend query rewritten (no session_end filter)
- Phase B: backfill never writes session_end; liveness guard removed; 30s reconciliation loop in serve; repair command; OTEL demoted to informational
- Phase C: src/archive.rs — daily archive sweep mirroring ~/.claude/projects/ → ~/.trakr/archive/
- 66 tests passing; cargo build warning-free
- Before/after spend: $214.19 → $104.82 (2.04× reduction from dedupe fix)

**State of the project:**
- trakr serve runs reconciliation every 30s and archive daily; spend is accurate from transcripts alone
- trakr repair --dry-run shows 58 sessions to rebuild (51 with synthetic session_end from old backfill)
- OTEL receiver still compiles and runs but is informational only

**Immediate next priorities:**
1. Jim to run `trakr repair --run` to rebuild spend from clean transcripts
2. Action 4d.3 — surface title/summary in `trakr list` and `trakr show`
3. Filtering/JSON output on `list`, `show`, `stats`
4. CI/CD and crates.io publication

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-14 (single-ledger complete + UX polish) ────

**What was completed this session:**
- `trakr repair --run` executed: 60 sessions rebuilt from corrected parser, spend corrected
- Bug fixes landed: `aiTitle` field name (titles now populate), `parse_timestamp` Utc::now() fallback (was stomping `last_activity_at` for all backfilled sessions → fake 34 "active" sessions), `trakr repair` defaults to `--run` (no flag required)
- `trakr spend` redesigned: local time with UTC offset, session count in title line, clean 3-row table (Cost / Budget / Used), no OTEL noise
- `trakr inspect-logs` redesigned: single-ledger aware (Stale / New / Orphaned counts), all-time + monthly spend, `--verbose` per-session table with title + spend + sync status; hooks-era Complete/Partial/Missing terminology removed
- `trakr sync` new command: manually triggers reconciliation sweep, prints stats + timestamp
- `TrackingStatus`, `SessionSummary`, `inspect_logs` (hooks-era dead code) deleted from `backfill.rs`
- New storage functions: `get_all_sessions_meta`, `get_spend_by_session`, `get_total_spend_usd`
- 66 tests passing; `cargo build` warning-free

**State of the project:**
- `trakr spend` shows $112.95 / $200.00 (56.5%) for June 2026 — accurate single-source figure
- `trakr inspect-logs` shows 60/60 sessions in DB, 0 stale, titles populated; all-time spend $225.65
- `trakr serve` running as launchd service (30 s reconciliation loop, daily archive sweep)
- Single-ledger architecture fully live; OTEL receiver parked but compiles

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Filtering/JSON output on `list`, `show`, `stats`
3. README update to document `sync`, `inspect-logs` redesign, `repair` default behaviour
4. CI/CD and crates.io publication (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-14 (daemon polish + status line integration) ──

**What was completed this session:**
- Hooks removed: `write_hooks_to_settings()` and `suggested_hook_config()` deleted; `cmd_init` no longer writes hooks to `~/.claude/settings.json`; `~/.claude/settings.json` cleaned of `SessionStart`/`SessionEnd` hook entries
- `trakr restart-service` command added (`cmd_uninstall_service` + `cmd_install_service`)
- `tlog!` macro added for timestamped daemon logs (local time + UTC offset via `chrono::Local`)
- Daemon startup/shutdown log lines: `daemon starting` (canonical: budget, sync interval, api state, home dir) and `daemon stopping` via SIGTERM handler
- Reconciliation renamed to "syncing" throughout logs
- API server made optional: `api_enabled` flag in config (default `false`); `std::future::pending::<()>().await` parks runtime when disabled
- `sync_interval_secs` added to config (default 30s), wired into serve loop
- `cmd_status` cleaned up: hook section removed, OTEL env vars removed, API shown as disabled, service section renamed
- `doc/serve-daemon.md` created: documents daemon architecture, sync loop, SIGTERM handling, log format
- `doc/README.md` updated: Architecture section added pointing to serve-daemon.md
- `trakr spend --json` flag: fast DB-only path, outputs `{"spent":N,"budget":N,"pct":N}` for status line
- `~/dotfiles/home/claude/statusline-command.sh` updated: `trakr spend --json` section with colour coding and `command -v trakr` guard
- `~/.claude/settings.json` `statusLine` field added pointing to the script

**State of the project:**
- Claude Code status line live: shows spend/budget with colour (green/yellow/red) from `trakr spend --json`
- 66 tests passing; `cargo build` clean; launchd service restarted
- Hook-free architecture fully in effect: 30s reconciliation loop is the sole update mechanism

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Filtering/JSON output on `list`, `show`, `stats`
3. README update to document `sync`, `inspect-logs` redesign, `repair` default, no-hooks architecture
4. CI/CD and crates.io publication (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-14 (dynamic pricing + UX fixes) ────────────

**What was completed this session:**
- Phase 4e fully implemented: `src/rates.rs` — fetch/cache/resolve from LiteLLM price list
- `src/cost.rs` refactored to use `rates::resolve`; cache_creation rate corrected to 1.25× input (was 1×)
- `src/storage.rs` — 3 spend query functions load rate card once per call, not per event
- `trakr sync-rates` command — fetches rates, logs timestamped line to `serve.log`, prints "Rates synced (N models)" to stdout
- `trakr serve` — daily rates refresh task alongside existing archive sweep
- `trakr status` — Storage section now shows rates.json last-fetched age; stale if > 48 h
- Daemon startup log: `home=` now shows `~/.trakr` instead of `~`

**State of the project:**
- 66 tests passing; `cargo build` clean; `trakr sync-rates` live (237 Claude models cached)
- Rate card sourced from LiteLLM with exact key matches for all current Claude Code models; hardcoded fallback retained for offline use
- `trakr serve` daemon still running (launchd); will pick up daily rate refresh on next 24 h cycle

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Filtering/JSON output on `list`, `show`, `stats`
3. README update to document `sync-rates`, `sync`, `inspect-logs` redesign, no-hooks architecture
4. CI/CD and crates.io publication (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (publish + UX polish) ──────────────────

**What was completed this session:**
- Crate renamed `ctx-trakr` → `trakr`; GitHub repo renamed to `ubiqtek-ai/trakr`; all `ctx_trakr::` imports updated in `src/main.rs`
- Published `trakr v0.1.0` then `v0.1.1` to crates.io
- `Cargo.toml`: added `homepage`, `repository`, `keywords`, `categories`
- README: title updated, Quick start rewritten (no hooks/OTEL, startup reconciliation handles backfill, `trakr inspect` added)
- `LAUNCH_AGENT_LABEL` corrected to `io.ubiqtek.trakr.serve`
- Clap `name = "ctx-trakr"` fixed to `"trakr"` (was showing wrong name in `--version`)
- DB freshness in `trakr status` now shows human-readable age (`3h 42m ago` / `1d 5h ago`)
- `trakr inspect`: token totals added to summary (all-time + monthly, K/M compact format)
- `inspect-logs` subcommand renamed to `inspect`
- `Justfile`: `install-cli` now passes `--force`

**State of the project:**
- 66 tests passing; `cargo build` clean; `trakr v0.1.1` live on crates.io
- Diagnosing spend accuracy on work machine (installing fresh and comparing `trakr inspect` + `trakr spend` output)

**Immediate next priorities:**
1. Diagnose spend accuracy on work machine using `trakr inspect` token totals
2. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
3. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
4. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (v0.1.2 + spend gap diagnosis) ──────────

**What was completed this session:**
- `trakr stats` extended: per-model token breakdown table (Input / Out / Cache read / Cache create columns with K/M compact format); session list hidden behind `--verbose`
- `fmt_tokens_compact()` helper added to `src/main.rs`
- `trakr v0.1.2` published to crates.io
- Spend gap diagnosis completed for work machine: Anthropic June ($255.50) vs trakr all-time ($244.05) = $11.45 net gap. Confirmed 125 subagent JSONL files = 125 Agent calls (all accounted for). LiteLLM rate card matches Anthropic published pricing exactly. Gap explained by Claude usage before May 26 (pre-installation on work machine). `<synthetic>` model holds 0 tokens — not a factor.

**State of the project:**
- `trakr v0.1.2` live on crates.io; spend tracking accurate on both home and work machines
- 66 tests passing; `cargo build` clean; launchd service running

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
3. GitHub Actions CI/CD (Action 5.3)
4. README update for `sync`, `inspect` redesign, `repair` default, no-hooks architecture

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (stats --month + spend gap deep dive) ──

**What was completed this session:**
- `trakr stats --month YYYY-MM` flag added (`src/main.rs`): filters token breakdown and session list by event timestamp month; "Month: YYYY-MM" header when active
- `trakr v0.1.3` published to crates.io
- Spend gap diagnosis deepened on work machine (Anthropic $254.77 June vs trakr $206.35):
  - Work app screenshot confirmed 100% `claude_code` product — no web/API-key usage
  - Per-model breakdown from work app: Opus $135.21, Sonnet $111.87, Haiku $7.70
  - `trakr stats --month 2026-06` on work machine: Sonnet ~$88.6, Opus ~$114.2, Haiku ~$4.7
  - Undercount ratios: Haiku 64%, Sonnet 26%, Opus 18% — NOT uniform, Haiku worst
  - File count: 37 JSONL files on disk = 37 trakr sessions (no missing parent sessions)
  - Subagent structure: 125 files, all depth-1 flat (`<uuid>/subagents/agent-*.jsonl`) — no nested agents
  - **Working hypothesis:** Claude Code makes background API calls (compact summary generation, title generation) that don't appear in session JSONLs and are invisible to trakr
  - **Next diagnostic:** Python one-liner to sum tokens directly from JSONL files to verify if the files themselves contain the missing tokens (awaiting result from work machine)

**State of the project:**
- `trakr v0.1.3` live on crates.io; 66 tests passing; `cargo build` clean
- Spend gap is real (~$48 in June), cause not yet confirmed — hypothesis is background API calls outside session JSONLs

**Immediate next priorities:**
1. Run JSONL token-sum diagnostic on work machine to confirm/refute background-call hypothesis
2. If confirmed: document the known gap in README/inspect output; consider if OTEL can fill it
3. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
4. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
5. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (1h cache tier pricing fix) ──────────────

**What was completed this session:**
- Root cause of ~19% spend gap identified via Opus agent analysis:
  - **Primary:** `cache_creation_input_tokens` was priced entirely at 1.25× input rate, but the 1h TTL tier (dominant in Claude Code: 70–83% of cache-creation by model) is billed at 2× input rate. The JSONL has the per-tier split at `usage.cache_creation.{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}`.
  - **Known limitation (no fix):** Haiku background calls (title/summary generation) are billed by Anthropic but never written to JSONL files — ~$3/month invisible to trakr regardless.
- `event.rs`: `TokenUsage` gains `#[serde(default)] cache_creation_1h_input_tokens: u64`
- `backfill.rs`: `PerModelAccumulator` reads `usage.cache_creation.ephemeral_1h_input_tokens`; accumulator tuples widened to 5-tuples
- `cost.rs`: `compute_cost_usd_with_card` / `compute_cost_usd` gain `cache_creation_1h_tokens` param; 1h priced at `2× input_per_token`, 5m at `1.25×`; 2 new tests (`cache_creation_1h_at_2x_input_rate`, `cache_creation_mixed_tiers`)
- `storage.rs`: all 3 spend query call sites updated; `main.rs` pattern match fixed
- `trakr repair` run: 67 sessions rebuilt from corrected parser

**State of the project:**
- `trakr spend` June 2026: $112.95 → **$171.88** (+$58.93, +52%) after repair — now correctly accounts for 1h cache tier pricing. Remaining gap vs Anthropic billing (~$10–15) explained by invisible background Haiku calls (known limitation). 68 tests passing; `cargo build` clean.

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Publish v0.1.4 with pricing fix
3. Document Haiku background-call limitation in README / `trakr inspect` output
4. Action 5.4 — CodeQL setup
5. GitHub Actions CI/CD (Action 5.3)

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
