# Backfilling from Claude Code Session Logs

## Background

Claude Code writes a native session log for every conversation at:

```
~/.claude/projects/<encoded-project-path>/<session-id>.jsonl
```

Each line is a newline-delimited JSON object. These logs exist regardless of whether
ctx-trakr hooks are installed, which means they are the ideal source for backfilling
historical data — particularly for machines or projects where hooks were never set up.

The `backfill-logs` command reads these files and populates the ctx-trakr SQLite DB with
the same event stream that hooks would have produced.

---

## Log format

### Entry types

Every line has a `type` field and a `sessionId` field. The types we care about:

| `type`      | What it contains |
|-------------|-----------------|
| `assistant` | `message.model`, `message.usage` (per-turn token counts), `message.content[]` (may include `tool_use` blocks) |
| `user`      | User messages — used only for session boundary detection |
| all others  | `mode`, `permission-mode`, `file-history-snapshot`, `system`, `last-prompt`, `attachment`, `ai-title`, `queue-operation` — ignored |

### Assistant entry (abbreviated)

```json
{
  "type": "assistant",
  "sessionId": "67545be0-15f1-45e0-9c2a-09a9974c0baf",
  "timestamp": "2026-04-29T20:39:30.839Z",
  "message": {
    "model": "claude-sonnet-4-6",
    "usage": {
      "input_tokens": 3,
      "cache_creation_input_tokens": 6185,
      "cache_read_input_tokens": 10838,
      "output_tokens": 104
    },
    "content": [
      {
        "type": "tool_use",
        "id": "toolu_013F1KEb8d8d3JGecPrurcHu",
        "name": "Read",
        "caller": { "type": "direct" }
      }
    ]
  }
}
```

### Key observations

- `message.usage` is **per-turn**, not cumulative. To get accurate session totals we must
  sum across all `assistant` entries for the session.
- Tool uses live inside `message.content[]`, not as top-level entries. An assistant turn
  may contain zero or more tool_use blocks.
- `timestamp` is present on every entry, in ISO 8601 UTC.
- The session ID is stable across all entries in a file (and matches the filename).

---

## Event stream mapping

The backfill produces the same `Event` variants that hooks produce:

| Source in log | Produced event |
|---------------|----------------|
| First entry in file | `SessionStart { model, source: "backfill" }` |
| Each `tool_use` block in any `assistant.message.content[]` | `ToolUse { tool_name, status: "unknown", duration_ms: None, error: None }` |
| Sum of all `assistant.message.usage` across the session | `TokenUsage { model, input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, total_tokens }` |
| Last entry in file | `SessionEnd` |

Notes:

- `model` for `SessionStart` and `TokenUsage` is taken from the first `assistant` entry
  that has a non-empty `message.model`. If no assistant entry exists, falls back to `"unknown"`.
- A single `TokenUsage` event is inserted per session, at the same timestamp as `SessionEnd`,
  representing the summed totals. This matches what the hook captures (one event at stop time).
- Tool uses have no timing information (`duration_ms: None`) — hooks get this from the
  `PostToolUse` payload, but logs don't record execution time.
- `status` for tool uses defaults to `"unknown"` — hooks capture success/failure from the
  hook payload, logs don't expose this.

---

## Idempotency and reconciliation

### The rule

A session is skipped if the DB already contains a `session_end` event for that `session_id`.

**Why**: `session_end` means the session was fully tracked by hooks. Hook data is richer
(it has tool timings, real success/failure status) and should not be overwritten.

Sessions with no `session_end` in the DB are fair game — either they were never captured,
or hooks were running but the session ended abnormally (crash, kill). In both cases,
backfill replaces whatever partial data exists with a complete picture from the log.

### Partial-session replacement

If a session has some hook events but no `session_end`:
1. Delete all existing events for that `session_id` from the DB (and its JSONL backup).
2. Insert the full backfilled event stream.

This avoids a mix of hook-captured and log-derived events for the same session, which
would produce inaccurate token totals (partial hook events + a summed log-derived total).

### Idempotent re-runs

Running `backfill-logs` twice on the same DB is safe:
- Completed sessions (have `session_end`) are skipped both times.
- Backfilled sessions (produced by a prior backfill run) have a `session_end`, so they
  are also skipped on re-run.

---

## Discovery

`backfill-logs` scans `~/.claude/projects/` for all `.jsonl` files at depth 1 inside
each project subdirectory:

```
~/.claude/projects/
  -Users-jmdb-Code-github-ubiqtek-ctx-trakr/
    67545be0-15f1-45e0-9c2a-09a9974c0baf.jsonl   ← session log
    b1e1187f-fb30-4271-869e-5211d278ca5a.jsonl   ← session log
    memory/                                       ← ignored (not .jsonl at depth 1)
  -Users-jmdb-Code-github-jimbarritt-athena/
    ...
```

A `--project` flag (optional) filters to a single project directory by substring match on
the encoded path, e.g. `--project ctx-trakr` or `--project athena`.

A `--since` flag (optional) skips session files whose last-modified time is before the
given date (`YYYY-MM-DD`), useful for limiting a first backfill to recent history.

---

## Output

```
ctx-trakr backfill-logs

Scanning ~/.claude/projects/ ...
  Found 47 session(s) across 6 project(s)

  [skip]  67545be0  (already complete in DB)
  [skip]  b1e1187f  (already complete in DB)
  [new]   10943bb1  →  1 tool uses, 3 assistant turns  →  SessionStart + 1 ToolUse + TokenUsage + SessionEnd
  [new]   57d375c2  →  36 tool uses, 12 assistant turns →  SessionStart + 36 ToolUse + TokenUsage + SessionEnd
  [replace] a3f9...  (had 2 partial hook events, replaced with full log)

Done. 2 new session(s), 1 replaced, 44 skipped.
```

---

## What backfill cannot recover

- **Tool execution time** — `PostToolUse` hooks receive elapsed ms; logs do not record it.
- **Tool success/failure** — hooks capture the actual exit status; logs don't expose it.
- **Active (in-progress) sessions** — a session log that is still being written will
  produce a `SessionEnd` based on the last line at scan time. Re-running backfill after
  the session ends will skip it (it now has a `session_end` in DB). This is fine — the
  incomplete backfill acts as a snapshot.
- **Sessions older than the log retention** — Claude Code may prune old session logs.
  ctx-trakr cannot backfill what no longer exists.

---

## Implementation sketch

### New code

- `src/backfill.rs` — all backfill logic:
  - `discover_sessions(projects_dir, project_filter, since_filter) → Vec<SessionLogFile>`
  - `parse_session_log(path) → BackfilledSession` — walks entries, accumulates tool uses and token totals
  - `backfill_session(session: BackfilledSession, dry_run: bool) → BackfillResult` — applies the idempotency check, deletes partials, inserts events

### Changes to existing code

- `src/storage.rs` — add `delete_events_for_session(session_id)` and expose it (already needed by backfill for partial replacement)
- `src/transcript.rs` — currently only extracts the last assistant entry. Extend to support summing all turns (or keep it as-is and do the summing in backfill.rs directly)
- `src/main.rs` — add `BackfillLogs` subcommand with `--project`, `--since`, `--dry-run` flags
- `src/lib.rs` — export `backfill` module

### Flags

| Flag | Type | Description |
|------|------|-------------|
| `--project <substr>` | `Option<String>` | Filter to projects whose encoded path contains this substring |
| `--since <YYYY-MM-DD>` | `Option<String>` | Skip sessions with last-modified before this date |
| `--dry-run` | `bool` | Print what would be done without writing anything |
