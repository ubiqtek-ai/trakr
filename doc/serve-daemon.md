# Serve Daemon

`trakr serve` is a long-running background process installed as a macOS LaunchAgent, so it starts on login and is restarted automatically if it crashes.

## What it does

| Responsibility | Detail |
|---|---|
| HTTP API | Serves `GET /spend/monthly` and related endpoints on `:8788` |
| OTEL receiver | Accepts telemetry from Claude Code on `:4318` (currently informational — parked) |
| Reconciliation loop | Re-parses changed Claude transcripts every 30 s, keeps the DB current |
| Archive sweep | Copies Claude transcripts to `~/.trakr/archive/` once per day |

## Threading model

All work runs inside a single tokio async runtime. The three background tasks are spawned concurrently and `tokio::join!` keeps the process alive until all of them exit (which they never do — they loop forever).

```
trakr serve
├── tokio async thread pool
│   ├── HTTP server (axum)       — handles API requests
│   ├── OTEL receiver            — handles telemetry batches
│   ├── reconciliation loop      — sleeps 30 s, then spawn_blocking ──►
│   │                                blocking thread: scan files, write DB
│   └── archive loop             — sleeps 24 h, then spawn_blocking ──►
│                                    blocking thread: copy transcript files
```

The reconciliation and archive work is CPU/IO-bound (file reads, SQLite writes), so both use `spawn_blocking`. This moves them off the async thread pool onto dedicated blocking threads, which means the HTTP server stays responsive while files are being scanned or copied.

## The reconciliation loop

Each 30 s iteration:

1. **Scan** `~/.claude/projects/*/` for all JSONL session files
2. **Peek** at the first line of each file to extract `session_id` cheaply (no full parse)
3. **Skip if unchanged** — compare current `(file_size, mtime)` against the values stored in the DB from the last parse. If they match, skip. This is the common case; most iterations do almost nothing
4. **Re-parse if changed** — full JSONL parse: dedupe events by `message.id`, sum token counts across all turns, extract `title`, `summary`, `last_prompt`. Write atomically to DB via `replace_session`
5. **Update file meta** — store the new `(file_size, mtime)` so the next iteration can skip unchanged files
6. Sleep 30 s and repeat

### Why this is safe with concurrent DB access

The HTTP server reads from the same SQLite DB while the reconciliation loop writes to it. This works without explicit locking because:

- `PRAGMA journal_mode=WAL` — readers and the writer operate on separate WAL and main files; reads never block writes and writes never block reads
- `PRAGMA busy_timeout=5000` — if a lock *is* contested (e.g. two writes collide), SQLite retries for up to 5 s before returning an error, rather than failing immediately

## Logs

The daemon writes to `~/.trakr/serve.log`. View with `trakr logs`.

Currently logs:
- Startup: API and OTEL listener addresses
- Reconciliation: errors and warnings only (silent on success)
- Archive: logs when files are copied; silent otherwise

## Service management

```bash
trakr install-service    # installs and starts the LaunchAgent
trakr uninstall-service  # stops and removes it
trakr logs               # tail serve.log
trakr status             # check pipeline health
```
