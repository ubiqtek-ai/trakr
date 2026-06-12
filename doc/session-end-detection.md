# Session-end detection: hooks vs file watching

*Research date: 2026-06-12. Companion to [agentsview-comparison.md](agentsview-comparison.md).*

This doc digs into the specific question raised by the AgentsView research:
**how do you know a Claude Code session has ended?** — because trakr's live-cost
design (`GET /spend/monthly` = SQLite completed + OTEL active) depends on that
boundary to avoid double counting. The conclusion up front:

> **For cost accounting, "has the session ended?" is the wrong question.**
> AgentsView never answers it and never needs to. If trakr derives live cost
> from the same source as final cost (the transcript, parsed incrementally),
> the completed/active boundary — and the double-count guard that polices
> it — disappears. "Ended" remains useful only for archival and metadata,
> where being late or wrong is harmless.

---

## 1. Why trakr needs the boundary today

trakr runs **two pipelines that measure the same session in different units**:

| Pipeline | Source | Unit | When |
|---|---|---|---|
| Live | OTEL receiver (`otel_receiver.rs`) accumulates `claude_code.cost.usage` deltas per `session_id` in an in-memory map | Claude Code's *own* cost estimate (USD) | While session is active |
| Final | `SessionEnd` hook → `parse_session_log` → SQLite (`hooks.rs:76`) | trakr's rate-card estimate from summed transcript tokens | Once, at session end |

Because both pipelines see every session, `/spend/monthly` must pick exactly
one per session. The guard (`server.rs:84-91`) re-queries completed session
IDs on every request and drops those from the OTEL sum. The hand-off works
**iff** the `SessionEnd` hook fires and its transcript parse lands in SQLite.

### Failure modes of the hook as the boundary signal

1. **The hook doesn't fire on abnormal exit.** SIGKILL, crash,
   `tmux kill-session`, power loss. The session then never reaches SQLite
   until the reconciliation sweep at the *next* `trakr serve` startup
   (`run_log_reconciliation`). Its cost survives only in the OTEL in-memory
   map — which leads to:
2. **`trakr serve` restart loses the live ledger.** `SessionCosts` is
   `Arc<Mutex<HashMap>>` — memory only. Restart mid-month → every
   still-active session's accumulated OTEL cost vanishes and only reappears
   when that session eventually ends and gets transcript-parsed. The status
   line undercounts in the meantime. (The startup reconciliation only
   recovers sessions whose transcripts exist *and* are complete enough to
   parse; it can't recover OTEL deltas for still-running sessions.)
3. **The hand-off has a unit discontinuity.** OTEL cost is Claude's internal
   estimate; the SQLite figure is trakr's rate card × transcript tokens.
   These will rarely agree to the cent, so the status-line number *jumps* at
   the moment a session completes. Phase 4c already showed how large
   methodology gaps can be ($0.24 vs $281.70 — different bug, same lesson:
   two methodologies for one quantity will diverge).
4. **The `"unknown"` OTEL bucket can never be deduplicated.** When metrics
   arrive without a `session_id` attribute they accumulate under `"unknown"`
   (`otel_receiver.rs:113`), which never matches a completed ID — if those
   metrics belong to a session that later completes, that cost is counted
   twice for the rest of the month. plan.md already flags `session_id`
   presence in OTEL as "needs real-world validation".
5. **Late OTEL exports after completion are handled correctly** (the guard
   filters at read time, so post-completion deltas for a completed session
   are excluded) — but note they are *silently discarded*, which is only
   correct because the transcript parse is treated as strictly more accurate.
6. **Hooks require user-owned config.** A stray edit to
   `~/.claude/settings.json`, a Claude Code update changing hook semantics,
   or a second machine without `init` run — and the boundary signal silently
   disappears for every session there.

The repeated pattern: every failure is on the *hook* side, and every repair
(reconciliation sweep, fallback minimal `session_end` insert) works by going
back to the **transcript file** — which was the ground truth all along.

---

## 2. How AgentsView handles the same problem

AgentsView (see [agentsview-comparison.md](agentsview-comparison.md)) supports
20+ agents, most of which have **no hook system at all**, so it was forced
into a design that never relies on an end signal. Three layers, from its
source (`internal/sync/watcher.go`, `internal/sync/engine.go`,
`internal/sessionwatch/watcher.go`):

### 2.1 Detect change, not state — fsnotify + debounce

A filesystem watcher (fsnotify: `Write`/`Create`/`Remove`/`Rename`) covers the
session directories — recursively where cheap, shallow where the tree is huge
(with a watch budget and `EMFILE`/`ENOSPC` backoff). Events land in a pending
map and a ~100ms debounce ticker batches rapid writes before triggering a
sync. New directories are auto-watched on `Create`. A **periodic full sync
(every 15 min)** backstops anything the watcher missed.

### 2.2 Idempotent incremental sync — the actual dedup mechanism

When a file is flagged, the engine decides what to do from **stored file
metadata, not session state**. Each session row carries `file_path`,
`file_size`, `file_mtime`, `file_inode`, `file_device`, `file_hash`:

- **Skip**: size and mtime match the stored values (and schema version is
  current) → no work. Re-running sync on unchanged files writes nothing —
  idempotency is the dedup.
- **Incremental append** (`tryIncrementalJSONL`): file grew, same
  inode/device → read only the appended bytes, insert new messages with
  ordinals after the stored `MaxOrdinal`.
- **Full reparse + replace**: inode/device changed (file replaced), schema
  stale, or the appended chunk shares a `message.id` with the stored tail
  (a turn was split across syncs) → `ReplaceSessionMessages` atomically
  swaps the session's rows. This is the same shape as trakr's
  `replace_session`.
- A **skip cache** remembers files that failed to parse, keyed on mtime, so
  they're not retried until they change.

### 2.3 "Ended" is a derived timestamp, not an event

There is **no boolean ended state anywhere in the engine**. Sessions carry an
`ended_at` that is simply *the latest activity timestamp seen in the file*,
advanced on every sync. The live-session UI (`sessionwatch`) polls the DB
every 1.5s for a version bump (message count + mtime) and falls back to a
direct file sync if the file changed but the DB didn't within 5s — "live"
just means "changed recently". A session that dies by SIGKILL and a session
that exits cleanly are indistinguishable, *and nothing breaks*, because no
correctness property depends on telling them apart.

### Why double counting is structurally impossible there

Cost is computed **from the indexed messages**. The live cost of an active
session and the final cost of a finished session are the same query at
different times — one ledger, one methodology, monotonically converging on
the final number. There is no second measurement of the same session to
reconcile, so there is nothing to deduplicate and no boundary to detect.

---

## 3. The signal menu, compared

For completeness — every way to detect "session ended" for a Claude Code
session, and what each is actually good for:

| Signal | Latency | Reliability | Catches crash/kill? | Needs config? | Verdict |
|---|---|---|---|---|---|
| `SessionEnd` hook | Immediate | Misses abnormal exits | No | Yes (settings.json) | Best as *accelerator*: gives `transcript_path` + reason instantly when it does fire |
| Transcript mtime quiescence (no writes for N min) | N minutes | High — survives crashes | Yes | No | Ambiguous vs idle-but-open sessions; fine for archival, wrong for "stop counting cost" |
| Process liveness (is the transcript still held open / does the claude PID exist) | Polling interval | High but platform-specific (lsof / `/proc`) | Yes | No | Hacky; only worth it if a true "is it running" bit is ever needed |
| OTEL stream silence | Export interval ×k | Low — silence also means user idle | Yes | Yes (env vars) | Not usable as an end signal |
| `SessionStart(source=resume/clear)` for a *new* session | Immediate | Only covers clear/resume paths | No | Yes | Useful corroboration that the predecessor ended |

No single signal is both immediate and crash-proof. AgentsView's answer —
**stop needing the signal** — beats picking one.

### A note on idle sessions (the tmux reality)

In Jim's multi-session tmux workflow, a Claude Code session can sit open and
idle for hours. Under mtime-quiescence detection it looks ended; under the
hook model it looks active. **Under the single-ledger model the question is
moot**: an idle session's transcript doesn't grow, its parsed cost doesn't
change, and the month-to-date total is correct either way. This is the
strongest practical argument for the AgentsView approach in trakr's exact
use case.

---

## 4. What this means for trakr's live-cost design

### Recommended: single-ledger, watcher-driven (Option B)

Make the transcript the only cost source, parsed incrementally, with hooks
demoted to optional accelerators:

```
~/.claude/projects/**/*.jsonl
        │  (notify watcher, debounced; plus periodic sweep — the
        │   existing run_log_reconciliation, run on a timer, not just at startup)
        ▼
incremental parse from stored byte offset        ← sessions table gains:
  (fall back to full parse_session_log +            file_size, file_mtime,
   replace_session on shrink/replace)               byte_offset (file_inode optional)
        ▼
SQLite: per-session token totals, updated in place, idempotent
        ▼
GET /spend/monthly = SUM(cost of all sessions touched this month)
                     — no active/completed split, no OTEL term, no guard
```

- **Double counting is eliminated by construction**, not guarded against:
  one ledger keyed by `session_id`, updates either append-from-offset or
  atomically replace. The `completed_ids` exclusion logic in `server.rs` is
  deleted, not fixed.
- **`trakr serve` restart loses nothing** — offsets and totals are in
  SQLite, and the startup sweep catches writes that happened while down.
- **No discontinuity** — live and final numbers are the same rate-card
  methodology, so the status line converges instead of jumping.
- **Crash-killed sessions just stop growing.** Their cost-so-far is already
  in the ledger. Nothing to repair.
- **Latency is fine for the use case**: a debounced watcher gives
  ~sub-second freshness; even pure 30s polling beats the OTEL export
  interval (~10–60s) the live view tolerates today.
- The `SessionEnd` hook stays, but its job shrinks to what only it can do:
  immediate final sync, `reason`, and transcript archiving for Phase 4d.
  `SessionStart` keeps providing `source` (startup/resume/clear). If the
  hooks are missing, everything still works — just with watcher latency.
- `sessions.ended_at` becomes AgentsView-style **last-activity timestamp**,
  with a nullable `end_reason` column that only the hook populates. "Ended"
  is then metadata, not a correctness boundary.

What happens to the OTEL receiver? Keep it, repurposed as a
**cross-check, not a source**: Claude's own cost estimate per session vs
trakr's rate-card figure is exactly the calibration signal the "estimates
only" caveat needs (and would catch rate-card rot — see the LiteLLM
recommendation in the comparison doc). It stops contributing to the spend
total, so its known weaknesses (`unknown` bucket, memory-only, http/json
only) stop mattering.

### Alternative considered: harden the current dual-pipeline (Option A)

Persist the OTEL map to SQLite, run reconciliation on a timer, resolve the
`unknown` bucket, accept the unit discontinuity. Every one of these patches
the boundary; none removes it. More code than Option B and still wrong in
the corners. Not recommended.

### Rust implementation notes

- The `notify` crate is the fsnotify equivalent (inotify/FSEvents/kqueue,
  with a built-in poll fallback). A `notify` watcher on `~/.claude/projects`
  plus a 15-min sweep mirrors AgentsView exactly. Given trakr's scale (one
  user, tens of sessions), **a pure 15–30s poll of file sizes/mtimes would
  also be entirely adequate** and dependency-free — the watcher is an
  optimization, not a requirement.
- Incremental parsing is a `File::seek` to the stored offset +
  line-buffered read; on any anomaly (size shrank, parse error mid-line,
  stored offset > size) fall back to the existing
  `parse_session_log` + `replace_session` full path. Start with
  full-reparse-on-change only — at trakr's file sizes that's likely fast
  enough, and incremental offsets can come later.

---

## 5. Open risks to verify (both designs)

1. **Cross-file duplicate usage on resume/branch.** `claude --resume` and
   conversation branching can copy prior entries into a *new* session file.
   ccusage and AgentsView dedupe at message level (`message.id` +
   `requestId`); trakr's `parse_session_log` sums every assistant `usage`
   block in a file with **no message-id dedup** (verified: no `requestId` /
   `message.id` handling in `src/`). If resumed files replay history with
   usage blocks intact, monthly totals double-count regardless of pipeline.
   **Action:** inspect a real resumed session's JSONL; if confirmed, add a
   `(message.id, requestId)`-seen set — global, not per-file.
2. **Sidechain/subagent usage.** Subagent turns appear as `isSidechain`
   entries in the native JSONL — confirm trakr's parser includes their
   `usage` blocks (AgentsView parses them; OTEL's cost metric includes
   them).
3. **Out-of-transcript costs.** Background utility calls (e.g. session
   title generation by Haiku) may appear in OTEL's cost metric but not in
   the session transcript. Likely cents — but it's the one thing the
   single-ledger design genuinely gives up vs OTEL. The cross-check
   receiver (above) quantifies it instead of guessing.
4. **Transcript format drift.** Both designs already depend wholly on the
   JSONL format; the existing "contain it in one parser module" principle
   (claude-integration-options.md) stands. AgentsView's schema-version
   column + full-reparse-on-version-bump is the proven pattern if trakr's
   parser ever needs migration-triggered re-ingestion.

---

## 6. Decision summary

| Question | Answer |
|---|---|
| Can hooks reliably detect session end? | No — they miss every abnormal exit, and the repair path is always "read the transcript anyway" |
| Can file watching detect session end? | Not *precisely* (idle ≈ ended) — but precisely enough for archival, and cost accounting doesn't need it at all |
| How does AgentsView dedupe live vs final cost? | It doesn't have the problem: one source, idempotent incremental sync, `ended_at` = last activity |
| What should trakr do? | Single ledger from watched transcripts (poll first, `notify` later); delete the OTEL term and the double-count guard from `/spend/monthly`; keep hooks as accelerators and OTEL as a calibration cross-check; verify message-id dedup on resumed sessions before trusting monthly totals |
