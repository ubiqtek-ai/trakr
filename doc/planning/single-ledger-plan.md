# Execution plan: single-ledger spend from transcripts

**Audience:** an implementation agent (Sonnet) executing without further design
discussion. Everything needed is in this file plus the referenced docs. When in
doubt, the design docs win, in this order:
[transcript-structure.md](../transcript-structure.md) →
[event-sourced-sessions.md](../event-sourced-sessions.md) →
[session-end-detection.md](../session-end-detection.md).

**Goal:** Claude's transcripts at `~/.claude/projects/` become the single
source for all spend. Fix the parser bugs that make today's numbers ~2× too
high, remove the completed/active boundary from the spend maths, add a daily
archive sweep. OTEL and hooks are **parked**: code stays, compiles, and runs,
but no spend correctness depends on it.

---

## Invariants — do not violate

1. **Never fabricate a `session_end` event.** Only the SessionEnd hook may
   write one (it observed it). Backfill/reconciliation record what the
   transcript proves: start, tool uses, token usage — never an end.
2. **Count `message.usage` once per `message.id`.** One API response is
   written as multiple `assistant` JSONL lines (one per content block), each
   repeating the same usage object. First occurrence wins (repeats are
   identical).
3. **Spend never keys on endings.** A session's tokens were spent whether or
   not it ended. Any query filtering on `session_end` for money purposes is a
   bug.
4. **Claude's format stays quarantined in the parser modules**
   (`backfill.rs` / `transcript.rs`). Nothing outside them may inspect raw
   JSONL shapes.
5. **British English** in all naming, comments, and docs.
6. All existing tests must pass or be deliberately updated with a comment-free
   honest assertion of the new behaviour. `cargo build` warning-free.

## Current state you inherit

- Branch `main`. Check `git status`/`git log` first: recent commits include
  doc research (PR #1) and possibly uncommitted working-tree changes from
  2026-06-11/12 (an `active_sessions_count` API field; an mtime "liveness
  guard": `backfill::looks_active`, `ACTIVE_LOG_WINDOW`, a `[live?]` skip and
  `--force` flag in `backfill-logs`, and a skip in `run_log_reconciliation`).
- The liveness guard was an interim fix that this plan **obsoletes** (it
  existed only to avoid stamping fake `session_end`s; we no longer write them
  at all). Remove it as instructed in Phase B.
- Key files: `src/backfill.rs` (transcript parsing + backfill),
  `src/storage.rs` (SQLite, schema migrations v1/v2 exist,
  `get_monthly_spend_usd` at ~line 275, `archive_transcript` at ~line 38),
  `src/server.rs` (axum API), `src/otel_receiver.rs` (parked but running),
  `src/main.rs` (CLI), `src/hooks.rs` (hook handlers, stay as-is),
  `src/cost.rs` (rate card).

---

## Phase A — parser correctness (the money bugs)

### A1. Dedupe usage by message id

In `parse_session_log` (`backfill.rs`): maintain a
`HashSet<String>` of seen `message.id`s; only accumulate `message.usage` for
an `assistant` entry whose `message.id` has not been seen. Entries with no
`message.id` (rare, ~12 corpus-wide): count them (no key to dedupe on).

### A2. Per-model usage

`message.model` is per-entry; sessions mix models. Replace the single summed
`TokenUsage` event with **one `TokenUsage` per distinct model** (the event
already carries a `model` field). Keep `sessions.model` populated with the
model that consumed the most output tokens (display only).

### A3. Include subagent transcripts

A session's full record is the main file **plus**
`<dir>/<session-uuid>/subagents/agent-*.jsonl` (same `sessionId` inside,
entries flagged `isSidechain: true`). Extend `parse_session_log` (or a wrapper
that takes the main path) to also parse any sibling subagent files into the
same `BackfilledSession`: their usage joins the per-model dedupe + totals,
their `tool_use` blocks join the tool-use events. Discovery
(`discover_sessions`) still returns main files only — subagent files are
resolved *from* the main path, never treated as sessions.

### A4. Spend query stops keying on endings

Rewrite `storage::get_monthly_spend_usd`:

- Sessions in scope: **any** session with token-usage data attributed to the
  month — not "sessions with a `session_end` this month".
- Month attribution: by the timestamp of the session's **last** `token_usage`
  event (closest to current behaviour; a session spans months as one bucket —
  acceptable, note it in the code).
- Cost: **sum every `token_usage` event** of the session (there is now one
  per model), pricing each via `compute_cost_usd(event.model, …)` — not
  "last event only".

### A5. Tests (Phase A)

- Fixture with one `message.id` repeated across 3 lines (thinking/text/
  tool_use, identical usage) → counted once.
- Fixture mixing two models → two `TokenUsage` events, spend = sum of both at
  the correct rates.
- Fixture with a `subagents/agent-x.jsonl` sibling → its usage and tool uses
  appear in the parsed session.
- Spend query: a session **without** `session_end` contributes to the month.

### A6. Acceptance check (manual, run and report)

After A1–A4, `trakr backfill-logs --dry-run` then a real run on a *copy* of
the DB (or after backup: `cp ~/.trakr/trakr.db /tmp/trakr-pre-A.db`), then
`trakr spend`. Expect the monthly figure to drop to roughly **half** the
pre-change value (corpus measurement predicts ~2× overstatement today).
Cross-check the corpus totals with jq: deduped output tokens ≈ 2.16 M as of
2026-06-12 (see transcript-structure.md §3 table).

---

## Phase B — single ledger in the serve loop

### B1. Backfill never writes `session_end`

In `backfill.rs`, stop emitting the synthetic `SessionEnd` event. The
backfilled stream is: `SessionStart { source: "backfill" }`, tool uses,
per-model token usages. `sessions.ended_at` semantics change to
**last-activity timestamp** (max event timestamp seen in the transcript) —
rename the column only if cheap (migration v3 may add `last_activity_at` and
keep `ended_at` for the hook's true endings; prefer this over overloading).

### B2. Remove the obsolete liveness guard

Delete `backfill::looks_active`, `ACTIVE_LOG_WINDOW`, their three tests, the
skip + counter in `run_log_reconciliation`, and the `[live?]`/`--force` path
in `cmd_backfill_logs`. Reconciliation re-parsing a *running* session is now
safe and desirable: it refreshes that session's spend (no fake end is
written). `replace_session` already makes it idempotent.

### B3. Sampling loop

In `cmd_serve`, replace the single startup reconciliation call with a spawned
tokio task: run the reconciliation sweep **at startup and then every 30 s**.
To keep it cheap, add change detection: migration v3 adds `file_size INTEGER`
and `file_mtime TEXT` to `sessions`; the sweep skips a main file whose
(size, mtime) match the stored values *and* whose subagent dir is unchanged
(simplest correct rule: compare max mtime + total size across main file and
subagent files). Use `tokio::task::spawn_blocking` around the sweep body.

### B4. Spend endpoint and CLI

- `/spend/monthly`: delete the OTEL term and the `completed_ids` double-count
  guard from `server.rs`. Response keeps its shape;
  `sources.active_sessions_*` is now derived from the DB: a session is
  "active" (display only!) if its last activity is within the last hour.
  `completed_sessions_*` → rename fields to `sessions_usd`/`sessions_count`
  only if you also update `cmd_spend`; otherwise keep field names and accept
  the slight misnomer, documenting it in a serde comment.
- `cmd_spend` (CLI): drop the live-API-vs-SQLite split if simpler, but it
  must stay correct with the server down. When the server is not running,
  run one inline incremental sweep (B3's function) before reading SQLite —
  sub-second when nothing changed.
- `trakr status`: OTEL receiver and hook checks become **informational**
  (printed, but never counted in "problems found"); transcript pipeline
  checks (projects dir readable, DB fresh: newest transcript mtime vs newest
  DB activity) become the health signal.

### B5. One-off data repair

Existing DB rows were built by the over-counting parser and contain fake
`session_end`s. After A+B land: delete events for every session whose
transcript still exists (main file present under `~/.claude/projects` or
`~/.trakr/transcripts`) and re-backfill from the file. Sessions with no
surviving raw data: leave untouched, but print a count
("N sessions retain legacy (inflated) figures — raw transcript lost").
Implement as `trakr repair --dry-run/--run`, document in README
troubleshooting. Do not run the real repair yourself — leave that to Jim;
run only `--dry-run` and report.

### B6. Tests (Phase B)

- Backfilled session has no `session_end` event; spend still counts it (A5
  already covers the query half).
- Sweep change-detection: unchanged (size, mtime) → file not re-parsed
  (assert via a parse counter or by checking DB write timestamps).
- Active-display rule: session with last activity 10 min ago → counted
  active; 3 h ago → not.

---

## Phase C — archive sweep

Per transcript-structure.md §6: copies, never parses; daily; incremental.

### C1. `trakr archive` command + serve timer

New module `src/archive.rs`:
`run_archive_sweep(src: &Path, dest: &Path) -> Result<ArchiveStats>`.
Walk `~/.claude/projects/` recursively (main files **and** `subagents/`),
mirror the tree under `~/.trakr/archive/`. Copy when dest is missing or
(size, mtime) differ. Copy via temp file + atomic rename; preserve mtime on
the copy (so comparisons stay stable). Never delete from the archive — it
only grows. Expose as `trakr archive` (prints stats: copied / unchanged /
bytes) and run from `cmd_serve` on a spawned task at startup + every 24 h.

### C2. Relationship to the old per-session archive

`storage::archive_transcript` (hook path, flat
`~/.trakr/transcripts/<id>.jsonl`) stays functional (hooks are parked, not
removed) but the sweep is the system of record. README documents
`~/.trakr/archive/` as the real backup.

### C3. Tests

Temp source tree (two files + a subagent file) → sweep copies all, second
sweep copies none, append to one file → only that file recopied; mtime
preserved.

---

## Phase D — docs and plan upkeep

1. README: spend section ("single source: transcripts"), `trakr archive`,
   `repair`, status semantics, remove the OTEL env-var setup from Quick start
   (move to an "Optional: OTEL cross-check" appendix — the vars do no harm),
   update the troubleshooting entries that reference the OTEL pipeline.
2. `doc/planning/plan.md`: mark superseded actions, add a checkpoint, point
   WHAT'S NEXT at the first unfinished item of this plan.
3. `doc/README.md`: index this plan.

---

## Out of scope — do not do

- Removing OTEL receiver / hook handler code (parked, not deleted).
- The `notify` file-watcher (30 s polling is the decision; watcher is a
  possible later optimisation).
- Incremental byte-offset parsing (full reparse of changed files only).
- The typed-events adapter redesign / multi-agent adapters
  (transcript-structure.md §4) — separate future plan.
- Publishing, CI, crates.io.

## Definition of done

- `cargo test` green; `cargo build` warning-free.
- `trakr spend` (server down) and `/spend/monthly` (server up) agree exactly,
  with no OTEL term in either path.
- Spend ≈ half of pre-change figure; report the before/after numbers.
- A running session's spend visibly grows between two `trakr spend` calls
  ~1 min apart while the session is being used (verify once, manually).
- `trakr archive` twice: second run reports 0 copies.
- `repair --dry-run` output reported, real run left to Jim.
