# Implementation Plan

## ── WHAT'S NEXT ──────────────────────────────────────────────────────────
**Next:** Publish v0.1.16 (OTEL removed from `trakr audit` output + enterprise note), then run `trakr audit <actual>` on the WORK machine and read the per-model table vs claude.ai to locate the ~$100 gap.
**Then:** Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
**Sub-doc:** (none)
**Blockers:** None — v0.1.16 release build clean
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
- **[RETIRED 2026-06-23]** — hooks are no longer the ingestion path. `trakr init` installs NO hooks; ingestion is sweep-only (`serve` reconciliation loop + inline sweep in `spend`/`sync`). `handle_session_end` and the `trakr hook` subcommand remain only for back-compat with old installs. See Action 5.8 checkpoint.
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
- **[RETIRED 2026-06-23]** — `cmd_init` no longer writes hooks to `~/.claude/settings.json` (only OTEL env vars when OTEL is enabled). The hook-merge code has been removed; ingestion is sweep-based.
- ✓ DONE - ~~`cmd_init` merges `SessionStart`/`SessionEnd` hooks into `~/.claude/settings.json` directly (idempotent)~~
- ✓ DONE - ~~`suggested_hook_config()` updated to correct event names; no more `PostToolUse`/`Stop`~~

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

## Phase 4f: Activity Spend Breakdown

### Action 4f.1: `trakr breakdown` command
- ✓ DONE - `src/breakdown.rs` — per-category token breakdown from archived transcripts
  - `ToolCategory` enum: CodeRead, CodeWrite, CodeSearch, Execution, WebResearch, Delegation, Response, Other
  - `ToolCall { name, bash_command }` — carries actual bash command text for Bash/Execute calls
  - `categorise_bash_command(cmd)` — inspects leading binary: grep/rg/ag/find/fd/cat/bat/head/tail/wc/diff/ls/tree/eza/exa → CodeSearch; everything else → Execution
  - `categorise_call(call)` + `categorise_turn(calls)` — priority-based classification (WebResearch < Delegation < CodeWrite < Execution < CodeSearch < CodeRead < Other < Response)
  - `compute_breakdown_from_files(paths, card)` — concatenates main transcript + subagent files; deduplicates by `message.id`; tracks first/last timestamp from any JSONL line with `timestamp` field
  - `compute_breakdown_from_transcript(path, card)` — thin wrapper around `compute_breakdown_from_files`
  - `merge_rows(all)` — aggregate across multiple sessions
  - 12 unit tests (categorisation rules + merge correctness); 81 tests total
- ✓ DONE - `storage::get_session_ids_for_month(year_month)` — uses `MAX(token_usage timestamp)` approach (not `started_at`, which is unreliable for backfilled sessions):
  ```sql
  SELECT session_id FROM events WHERE event_type = 'token_usage'
  GROUP BY session_id HAVING strftime('%Y-%m', MAX(timestamp)) = ?1
  ```
- ✓ DONE - `storage::get_all_time_spend_usd()` — sums all token_usage events across all sessions/months
- ✓ DONE - `trakr spend --all` — all-time total (both transcripts and cost adjustments); defaults to current month
- ✓ DONE - `trakr breakdown [--session <id>] [--month YYYY-MM] [--all]` CLI command
  - Default: current month (matches `trakr spend` behaviour)
  - `--all`: all sessions regardless of date
  - `--month`: filters via DB session IDs then loads matching transcripts
  - `--session`: single transcript
  - Scans `~/.trakr/archive/<slug>/<uuid>/subagents/agent-*.jsonl` and includes subagent files alongside main transcript
  - Output: table with Turns, Input, Output, Cache read, Total tokens, Cost, Share columns
  - Header shows actual date range: `trakr breakdown — YYYY-MM-DD → YYYY-MM-DD`
- ✓ DONE - v0.1.10 → v0.1.13 version bumps

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

### Action 5.6: OTEL gap-fill (optional ~9% accuracy improvement)
Design doc: `doc/planning/otel-gap-fill-plan.md`

#### Phase A — Attribute dump experiment (gates design choice)
- ✓ DONE - Instrument `handle_metrics` to write raw OTEL body to `~/.trakr/otel-dump-metrics.jsonl`
- ✓ DONE - Add `/v1/logs` route writing to `~/.trakr/otel-dump-logs.jsonl`
- ✓ DONE - Ran fresh Claude Code session with telemetry enabled; read the dumps
- ✓ DONE - Key finding: `claude_code.api_request` log records carry `query_source` (`repl_main_thread` vs `generate_session_title`, `auxiliary`, etc.), `request_id`, `cost_usd` — no dedup or subtraction needed
- ✓ DONE - Branch chosen: new "Option 4" — filter log records by `query_source != "repl_main_thread"`; use `cost_usd` directly
- NOTE - Full findings documented in `doc/otel-telemetry-schema.md`

#### Phase B — Config flag & CLI (parallel with A)
- ✓ DONE - `src/config.rs`: add `otel_enabled: bool` + `otel_port: u16` (default false / 4318, `#[serde(default)]`; `save_config()` added)
- ✓ DONE - `trakr otel enable`: set flag, write OTEL env vars to `~/.claude/settings.json` (including `OTEL_LOGS_EXPORTER=otlp`), restart daemon
- ✓ DONE - `trakr otel disable`: clear flag, remove env vars, restart daemon
- ✓ DONE - `cmd_serve`: gate OTEL receiver start on `config.otel_enabled`; uses configured `otel_port`
- ✓ DONE - `trakr status`: show OTEL as disabled when flag off; show port when on
- NOTE - Commands named `enable`/`disable` (not `install`/`uninstall` as originally planned)

#### Phase C — Gap-fill logic
- ✓ DONE - `Event::BackgroundApiCall { request_id, model, cost_usd, query_source }` added to `src/event.rs`
- ✓ DONE - Migration v4: `request_id TEXT` column on events + unique partial index for dedup
- ✓ DONE - `storage::insert_background_api_call` — INSERT OR IGNORE on `request_id`
- ✓ DONE - `storage::get_monthly_background_spend_usd` — sums background costs for month
- ✓ DONE - `replace_session` / `delete_events_for_session` exclude `background_api_call` rows (survive reconciliation)
- ✓ DONE - `otel_receiver::handle_logs` fully implemented: parses `claude_code.api_request`, filters non-`repl_main_thread`, stores via `spawn_blocking`
- ✓ DONE - `trakr spend`: shows Transcripts / Background / Total breakdown when OTEL data present; Cost only when no background data
- ✓ DONE - `trakr spend --json`: adds `background_usd` and `total_usd` fields
- ✓ DONE - 4 new unit tests in `otel_receiver.rs` (skip main thread, capture title-gen, capture auxiliary, skip zero-cost)
- ✓ DONE - `trakr v0.1.5` published to crates.io; 73 tests passing

#### Phase D — UX polish
- ✓ DONE - README: "Optional: OTEL gap-fill" section rewritten; "Manual spend adjustments" section added; commands table updated; `trakr init` quick-start updated
- TODO - `trakr status`: warn if OTEL enabled but no batches received in >90 s

### Action 5.7: Manual cost adjustment
- ✓ DONE - `Event::CostAdjustment { day, amount_usd, reason }` added to `src/event.rs` — uses YYYY-MM-DD `day` field (more precise than `month`) for month attribution; `reason` is `Option<String>`
- ✓ DONE - `storage::get_monthly_adjustment_usd(year_month)` — sums `cost_adjustment` events by timestamp month
- ✓ DONE - `trakr adjust <day> <amount> [--reason "..."]` CLI command — positional `day` + `amount` (negative allowed via `allow_hyphen_values`); stored under session `"__adjustments__"` with `timestamp = <day>T00:00:00Z`
- ✓ DONE - `trakr spend` shows Adjustment line (with `+`/`-` sign) when non-zero; contributes to Total; `--json` gains `adjustment_usd` field
- ✓ DONE - v0.1.6 version bumped in `Cargo.toml`
- ✓ DONE - OTEL-by-default in `trakr init` reverted: enterprise CC accounts silently ignore standard `OTEL_*` env vars (early-return gate in binary); OTEL remains opt-in via `trakr otel enable`; config comment added noting enterprise limitation
- ✓ DONE - `trakr init` hints added: "Run `trakr restart-service` if the service is already running"
- ✓ DONE - `get_monthly_background_spend_usd` returns `(f64, usize)` — cost + call count; Background line in `trakr spend` now shows `$X.XX (N calls)`
- ✓ DONE - `doc/otel-enterprise-investigation.md` added: full binary analysis, two OTEL code paths, workarounds considered and rejected
- ✓ DONE - v0.1.7 / v0.1.8 / v0.1.9 bumped in `Cargo.toml`

### Action 5.8: `trakr audit <actual>` — discrepancy locator
- ✓ DONE - `trakr audit <actual> [--month YYYY-MM] [--yes]` in `src/main.rs` (`cmd_audit`)
- ✓ DONE - Deliberately does NOT run a reconciliation sweep first — surfaces sessions on disk but missing from DB
- ✓ DONE - Decomposes gap: `actual` (claude.ai) − trakr total (transcripts + background + adjustment) = gap
- ✓ DONE - Orphan scan: `discover_sessions` + `parse_session_log` over `~/.claude/projects`; filter by `last_activity_at` month (matches DB's MAX-timestamp attribution); session IDs not in `get_session_ids_for_month` are orphans; priced via `price_session_events` helper (sums `TokenUsage` events × rate card, identical to `get_monthly_spend_usd`)
- ✓ DONE - Residual = gap − untracked; sign-aware hints (under-report → background/rate-card; over-report → duplicate sessions/double-counted subagents)
- ✓ DONE - Offers reconciling `CostAdjustment` for residual ONLY when no material orphans (>$1) — otherwise recommends `trakr sync` first to avoid double-count after future ingestion; adjustment dated today (current month) or month-end (past month)
- ✓ DONE - **Explanatory enrichment** (audit must explain, not just calculate the gap):
  - Per-model spend table (`storage::get_monthly_spend_by_model` + `model_tier`) shown under the residual so the user can eyeball trakr's Opus/Sonnet/Haiku/Fable split against claude.ai's per-model usage screen — pinpoints WHICH tier is short
  - Directional guidance: more Haiku on claude.ai → invisible background calls (suggest `otel enable`); short paid tier → cross-machine usage under same account
  - Diagnostics block: coverage (disk logs in month vs tracked count), OTEL state (measured vs flat estimate), rate-card sync age (warns >48h stale)
- ✓ DONE - README: Features bullet, commands table row, "Auditing the discrepancy" section (sample output corrected — sweep-based, not hook-based)
- ✓ DONE - v0.1.15 bumped; 89 tests passing (`model_tier` test added; rest composes already-tested library fns)
- NOTE - Untracked-log orphan detection is the new diagnostic; `trakr breakdown` footer only caught the inverse (DB rows w/ no transcript)

### Action 5.5: Anthropic Analytics API integration (optional "exact mode")
- TODO - `GET /v1/organizations/analytics/cost_report` — returns pre-calculated spend in cents; no token multiplication needed
- TODO - Auth: `x-api-key` with `read:analytics` scope (org admin key — not available to all users)
- TODO - `trakr sync-analytics` command — fetches and stores authoritative spend figures, replacing transcript estimates for the covered period
- TODO - `trakr spend --exact` flag — uses Analytics API figures when available, falls back to transcript-based estimate
- NOTE - This closes the ~9% gap from invisible background API calls (title/summary generation) that never appear in local session transcripts
- NOTE - Requires org-level API key; transcript-based tracking remains the default for users without one
- NOTE - Per-user endpoint: `GET /analytics/cost_report?user_ids[]=<id>&bucket_width=1d` — allows per-user filtering within an org

---

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

## ── CHECKPOINT: Session 2026-06-15 (OTEL Phase A+B implementation) ─────────

**What was completed this session:**
- `src/config.rs`: `otel_enabled: bool` + `otel_port: u16` added to `Config` (both with `#[serde(default)]`); `save_config()` added for programmatic config writes; 1 new test (`save_config_round_trips_otel_flag`)
- `src/otel_receiver.rs`: `dump_to_jsonl()` helper appends raw OTLP bytes to `~/.trakr/` files; `handle_metrics` now dumps to `otel-dump-metrics.jsonl`; new `POST /v1/logs` route dumps to `otel-dump-logs.jsonl`
- `src/main.rs`: `trakr otel enable` / `trakr otel disable` subcommands — update config, merge/remove 5 OTEL env vars in `~/.claude/settings.json`, restart launchd service; `cmd_serve` gates `start_otel_receiver` on `cfg.otel_enabled`; `trakr status` shows OTEL state
- 69 tests passing (was 68); `cargo build` clean

**State of the project:**
- `trakr otel enable` is ready to use; running it will write env vars, restart the daemon, and begin capturing raw OTEL payloads to `~/.trakr/otel-dump-*.jsonl`
- Phase A experiment not yet run — dumps are empty until a new Claude session fires telemetry
- v0.1.4 still not published to crates.io (built locally, pricing fix included)

**Immediate next priorities:**
1. Start new session, run `trakr otel enable`, then use Claude Code for a few minutes and read the dumps — this gates Phase C branch choice
2. Publish v0.1.4 to crates.io (1h cache tier pricing fix)
3. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
4. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
5. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (OTEL gap-fill Phase A–C complete) ──────

**What was completed this session:**
- Phase A experiment run: captured live OTEL dumps, discovered `claude_code.api_request` log records carry `query_source` (`repl_main_thread` vs `generate_session_title`, `auxiliary`), `request_id`, and `cost_usd` — making Options 2 & 3 unnecessary
- Full findings documented in `doc/otel-telemetry-schema.md` with example payloads
- Phase C implemented as "Option 4" (filter by `query_source`, use `cost_usd` directly):
  - `Event::BackgroundApiCall` variant added to `src/event.rs`
  - Migration v4: `request_id` column + unique partial index on `events` (dedup)
  - `storage::insert_background_api_call`, `get_monthly_background_spend_usd`
  - `replace_session` / `delete_events_for_session` preserve `background_api_call` rows
  - `otel_receiver::handle_logs` parses log batches and stores background calls live via `spawn_blocking`
  - `trakr spend` shows Transcripts / Background / Total when OTEL data present; `--json` gains `background_usd` + `total_usd`
  - 4 new tests in `otel_receiver.rs`; all migrations hardened with INSERT OR IGNORE
- `trakr v0.1.5` published to crates.io; 73 tests passing; daemon restarted

**State of the project:**
- Background API calls (title generation, context preloading) now stored live as they arrive via OTEL; spend query picks them up automatically with no separate reconciliation loop
- `trakr spend` will show a "Background" line once real background calls accumulate; currently 0 (calls appear within ~60 s of each new Claude Code session starting)
- 73 tests passing; `cargo build` clean; launchd daemon running with OTEL enabled on port 4318

**Immediate next priorities:**
1. Phase D — README "Optional: OTEL gap-fill" section
2. Phase D — `trakr status`: warn if OTEL enabled but no batches in >90 s
3. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
4. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
5. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (OTEL by default + manual adjustments) ──

**What was completed this session:**
- `trakr init` now enables OTEL by default: `write_default_config()` writes `otel_enabled = true`; `cmd_init` calls `merge_otel_env_to_claude_settings()` so env vars land in `~/.claude/settings.json` automatically
- `Event::CostAdjustment { day, amount_usd, reason }` added to `src/event.rs`; `event_type_label()` returns `"cost_adjustment"`
- `storage::get_monthly_adjustment_usd()` sums adjustment events for a given month
- `trakr adjust <day> <amount> [--reason "..."]` subcommand: stored under `"__adjustments__"` session with `timestamp = <day>T00:00:00Z`; negative amounts work via `allow_hyphen_values`
- `trakr spend` shows "Adjustment" line with `+`/`-` sign when non-zero; total = transcripts + background + adjustment; `--json` gains `adjustment_usd` field
- README: features list, quick-start, commands table (added `adjust`, `sync`, `sync-rates`, `restart-service`, `otel enable/disable`; fixed `inspect-logs` → `inspect`), new "Manual spend adjustments" section, "Optional: OTEL gap-fill" section rewritten
- `Cargo.toml` bumped to `0.1.6`; 73 tests passing; Phase D README item marked done

**State of the project:**
- `trakr v0.1.6` ready to publish; `cargo build` clean; 73 tests passing
- OTEL on by default for all fresh installs; existing installs unaffected until they re-run `trakr init`
- Manual adjustments fully functional: `trakr adjust 2026-05-01 -48.00 --reason "pre-install gap"` works

**Immediate next priorities:**
1. Phase D remaining — `trakr status`: warn if OTEL enabled but no batches received in >90 s
2. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
3. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
4. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-15 (OTEL enterprise investigation + UX tweaks) ──

**What was completed this session:**
- Diagnosed OTEL not working on enterprise CC: binary analysis revealed an early-return gate (`NP()`) that bypasses standard `OTEL_*` env vars entirely on enterprise accounts; workarounds (hijacking `BETA_TRACING_ENDPOINT`) inadvisable as it intercepts Anthropic's enterprise telemetry
- Reverted OTEL-by-default in `trakr init`: config default back to `otel_enabled = false`; OTEL setup block removed from `cmd_init`; config comment added noting enterprise limitation
- `doc/otel-enterprise-investigation.md` added: full binary analysis, two code paths, workarounds rejected, conclusion closed
- `trakr init` now prints "Run `trakr restart-service` if the service is already running" hint
- `get_monthly_background_spend_usd` returns `(f64, usize)` (cost + call count); Background line in `trakr spend` shows `$X.XX (N calls)` so it's visible when OTEL is capturing data
- v0.1.7 / v0.1.8 / v0.1.9 bumped (in progress — user committing and publishing)

**State of the project:**
- 73 tests passing; `cargo build` clean; v0.1.9 ready to publish
- OTEL is opt-in only (`trakr otel enable`); works on personal accounts, silently no-ops on enterprise — documented
- Background call count visible in `trakr spend` when OTEL data is present

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Action 5.6 Phase D — `trakr status` warn if OTEL enabled but no batches received in >90 s
3. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
4. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-19 (trakr breakdown initial) ──────────────────

**What was completed this session:**
- `trakr breakdown` command implemented: classifies every API turn by the tools called and attributes token cost to one of 7 categories (CodeRead, CodeWrite, Execution, WebResearch, Delegation, Response, Other)
- `src/breakdown.rs` added (~200 lines): categorisation logic, per-transcript computation, multi-session merge
- `storage::get_session_ids_for_month` added for `--month` filtering
- 8 new tests (6 categorisation + 2 merge); 81 total passing
- Sample output (June 2026, 79 sessions): CodeWrite 32.6% ($106), Execution 25.9% ($84), Response 20.3% ($66), CodeRead 15.2% ($50), WebResearch 1.6% ($5)

**State of the project:**
- 81 tests passing; `cargo build` clean; `trakr breakdown` ready to use

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
3. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-19 (breakdown refinements + --all flag) ────────

**What was completed this session:**
- `Bash` categorisation split: introduced `CodeSearch` category; `categorise_bash_command` inspects leading binary (grep/find/cat/etc → CodeSearch, everything else → Execution)
- `ToolCall` struct carries `bash_command: Option<String>` extracted from `input.command` in tool_use blocks
- Subagent JSONL files included: scans `~/.trakr/archive/<slug>/<uuid>/subagents/agent-*.jsonl` and feeds them into `compute_breakdown_from_files` alongside the main transcript — closed a ~$17 gap vs `trakr spend`
- `storage::get_session_ids_for_month` fixed: switched from `started_at` (unreliable for backfilled sessions — was set to backfill run time) to `MAX(token_usage timestamp)`; correct month filter now matches `trakr spend`
- Date range header: header shows `trakr breakdown — YYYY-MM-DD → YYYY-MM-DD` using actual first/last `timestamp` fields from JSONL lines
- Total tokens column added before Cost column
- `--all` flag added to both `trakr spend` and `trakr breakdown`; both default to current month without the flag
- `storage::get_all_time_spend_usd()` added for `trakr spend --all`
- v0.1.10 → v0.1.13 version bumps

**State of the project:**
- 81 tests passing; `cargo build` clean; v0.1.13 ready to push/publish
- `trakr breakdown` total matches `trakr spend` within ~$2 (rounding); subagent gap closed
- June 2026 home machine breakdown: CodeWrite 36.3% ($121), Response 17.7% ($59), CodeRead 15.7% ($53), Execution 11.9% ($40), CodeSearch 11.8% ($39)

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
3. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-23 (spend/breakdown reconciliation) ──────────

**What was completed this session:**
- Root-cause analysis of spend vs breakdown gap on work machine ($317.24 breakdown vs $353.42 trakr spend): reconciled as $1.70 DB gap (sessions with no transcript file) + $19.14 background + $15.34 adjustment
- `server.rs` `/spend/monthly` fixed: `spent_estimated_usd` now returns full total (transcripts + background + adjustment); `SpendSources` gains `background_usd` and `adjustment_usd` fields — endpoint was stale since background/adjustment were added to `trakr spend --json` but server was never updated
- `trakr breakdown` reconciliation footer added: shows how breakdown total → trakr spend total, using same background resolution logic as `cmd_spend` (OTEL if enabled, else `background_factor` estimate, else zero); identifies count + cost of sessions in DB with no transcript file
- v0.1.14 published to crates.io; 88 tests passing

**State of the project:**
- `trakr spend --json` `total_usd`, the HTTP API `spent_estimated_usd`, and `trakr breakdown` footer `trakr spend total` all return the same figure
- 88 tests passing; `cargo build` clean; v0.1.14 live on crates.io

**Immediate next priorities:**
1. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
2. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)
3. GitHub Actions CI/CD (Action 5.3)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-23 (trakr audit — discrepancy locator) ──────

**What was completed this session:**
- Work-machine spend was ~$100 under claude.ai; built `trakr audit <actual>` (Action 5.8) to locate the cause
- `cmd_audit` in `src/main.rs`: reads DB total without a sweep, then scans `~/.claude/projects` for session logs whose `last_activity_at` falls in the month but whose session ID is absent from `get_session_ids_for_month` — "untracked logs on disk", priced identically via `price_session_events`
- **Made audit explanatory, not just arithmetic** (user feedback: it was only calculating the gap): per-model spend table (`storage::get_monthly_spend_by_model` + `model_tier`) to compare against claude.ai's per-model screen, directional guidance (more Haiku → background calls; short paid tier → cross-machine), and a Diagnostics block (coverage disk-vs-tracked, OTEL state, rate-card sync age)
- **Hook ingestion path RETIRED** (user pointed out the SessionEnd hook was supposed to be gone): confirmed `cmd_init` installs no hooks — ingestion is sweep-only. Marked `Commands::Hook` doc + plan Actions 4c.1/4c.4 as retired/back-compat-only. Handlers kept for old installs (deleting would break installs with the hook wired and no daemon)
- **`trakr spend` layout fix**: moved Total to its own line at the bottom under a rule (Budget/Used now sit above it); was confusingly buried mid-list
- README: Features bullet, commands table row, "Auditing the discrepancy" section (sample output corrected to sweep-based, not hook-based); Cargo.toml → v0.1.15

**State of the project:**
- `cargo build` clean; 89 tests passing; v0.1.15 ready to commit/publish (not yet published)
- Home machine: 0 cost-bearing orphans, so audit goes to residual + per-model — the diagnostics surfaced a real finding (rate card 74h stale; OTEL disabled). Orphan path needs the work machine to exercise fully
- `trakr audit <actual>` is the recommended first step whenever trakr drifts from claude.ai

**Immediate next priorities:**
1. Run `trakr audit <work-actual>` on the work machine — read the per-model table against claude.ai's per-model split to see which tier is short
2. Publish v0.1.15 to crates.io
3. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
4. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)

─────────────────────────────────────────────────────────────────────────────

## ── CHECKPOINT: Session 2026-06-23 (OTEL removed from audit output) ─────────

**What was completed this session:**
- Stripped OTEL from `cmd_audit` output in `src/main.rs` (enterprise accounts can't use OTEL):
  - Background calc no longer reads `get_monthly_background_spend_usd` / shows "N OTEL calls"; Background is always the flat `background_factor` estimate
  - Removed the residual guidance line suggesting `trakr otel enable`
  - Replaced the "OTEL gap-fill" Diagnostics line with `Background  flat estimate, not measured`
  - Added a foot-of-output note: background API calls (title/summary, mostly Haiku) are billed but never appear in transcripts; capturing them would need OTEL, unavailable on enterprise — so Background is an estimate
- README: refreshed `trakr audit` sample output to match (per-model split + Diagnostics block + enterprise note); "Residual (unexplained)" → "Residual after ingest"
- Cargo.toml → v0.1.16; release build clean

**State of the project:**
- `cargo build --release` clean; v0.1.16 ready to commit/publish (user publishing)
- Audit path is now OTEL-free in both command output and docs; enterprise limitation explained inline

**Immediate next priorities:**
1. Publish v0.1.16 to crates.io
2. Run `trakr audit <work-actual>` on the work machine — read per-model table vs claude.ai
3. Action 4d.3 — `trakr list` with title + project; `trakr show` with title + summary
4. Action 5.4 — CodeQL setup (reference: `~/projects/tsk`)

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
