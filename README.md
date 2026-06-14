# ctx-trakr

A Rust CLI (binary name: `trakr`) that tracks Claude Code context usage and estimates spend across all your active sessions. Designed for multi-session tmux workflows where you want a single aggregated month-to-date cost view.

## Features

- **Transcript-driven tracking** — Claude Code's native session transcripts (`~/.claude/projects/`) are the single source of truth for all spend; no OTEL pipeline required
- **Cost estimation** — token counts × published Anthropic rate card, per model (Haiku / Sonnet / Opus / Fable); spend is summed from all `TokenUsage` events regardless of whether a session has ended
- **Month-to-date spend** — aggregates all recorded sessions against a configurable monthly budget
- **Pipeline health check** — `trakr status` checks DB freshness and server; OTEL/env-var indicators are informational
- **Backfill & reconciliation** — imports historical sessions from Claude Code's native logs; a 30 s reconciliation loop in `trakr serve` catches sessions in progress and any sessions missed due to crash or force-quit
- **Archive** — `trakr archive` mirrors `~/.claude/projects/` to `~/.trakr/archive/` incrementally; `trakr serve` runs this daily
- **Runs on login** — `trakr install-service` registers a macOS LaunchAgent
- **SQLite storage** — persistent event log, session metadata (title, summary, project), and archived transcripts

> Costs are estimates based on the published Anthropic rate card. Only the Anthropic Admin Cost API gives billed truth.

---

## Installation

### From crates.io

```bash
cargo install trakr
```

### From source

```bash
git clone https://github.com/ubiqtek-ai/trakr.git
cd trakr
cargo install --path .
```

Both install a binary named **`trakr`**.

---

## Quick start

### 1. Initialise

```bash
trakr init
```

This does everything in one step:

- Creates `~/.trakr/` with the SQLite DB (`trakr.db`), `sessions/`, `transcripts/`, `archive/`, and `config.toml`
- Optionally writes OTEL telemetry env vars into `~/.claude/settings.json` (see [Optional: OTEL cross-check](#optional-otel-cross-check) below — these are **not** required for accurate spend tracking)

Scoping any env vars to Claude Code's settings means no shell profile changes are needed.

### 2. Start the server

```bash
trakr install-service   # macOS LaunchAgent — starts now and on every login
# or, for a foreground run:
trakr serve
```

### 3. Verify

```bash
trakr status
```

Checks the full pipeline and lists any problems with suggested fixes:

```
  ✓ DB                               trakr.db — last activity 12s ago
  ✓ Server                           http://localhost:8788 — reachable
  i OTEL receiver                    not configured (informational only)
```

OTEL indicators marked with `i` are informational — DB freshness is the health signal.

---

## Month-to-date spend

```bash
trakr spend
```

```
Spend  2026-06  (budget $200.00)
------------------------------------------
  All sessions (43)              $  104.82
------------------------------------------
  Total                          $  104.82
```

`trakr spend` runs an inline reconciliation sweep before reading SQLite, so results are always up to date whether or not the server is running. Spend is summed from all `token_usage` events keyed on the last activity timestamp — no `session_end` required.

### HTTP API

```bash
curl -s http://localhost:8788/spend/monthly
```

```json
{
  "period": "2026-06",
  "spent_estimated_usd": 104.82,
  "budget_usd": 200.0,
  "sources": {
    "all_sessions_usd": 104.82,
    "all_sessions_count": 43,
    "active_sessions_count": 1
  },
  "note": "Costs are estimates based on the published Anthropic rate card."
}
```

Active sessions (those with `last_activity_at` within the last hour) are identified in the response for informational purposes, but spend is sourced from transcripts for all sessions — completed or not.

`GET /status` reports server health and DB freshness — this is what `trakr status` uses.

### tmux status line

Poll the API and format the result for your status line. Example using `jq`:

```bash
#!/bin/bash
# ~/.local/bin/trakr-status
result=$(curl -sf http://localhost:8788/spend/monthly 2>/dev/null) || exit 0
spent=$(echo "$result" | jq -r '"$\(.spent_estimated_usd)"')
budget=$(echo "$result" | jq -r '"$\(.budget_usd)"')
echo "$spent / $budget"
```

In `.tmux.conf`:

```tmux
set -g status-right "#(~/.local/bin/trakr-status) | %H:%M"
set -g status-interval 30
```

---

## How tracking works

Claude Code's transcripts (`~/.claude/projects/`) are the **single source** for all spend. There is no separate OTEL pipeline for accuracy.

- **30 s reconciliation loop** in `trakr serve` re-parses all recently active transcripts, catching sessions still in progress and any sessions missed due to crash or force-quit. This is the sole update mechanism.
- **`trakr spend`** runs an inline sweep before reading SQLite, so the result is always current even if the server is not running.
- **Spend is summed from all `token_usage` events** regardless of whether a session has ended — there is no completed/active split in the core accounting.
- **Subagent files** in the same `~/.claude/projects/<slug>/` directory are included in the parse, so agent-spawned sub-sessions are counted.

---

## Configuration

Edit `~/.trakr/config.toml` (created by `trakr init`):

```toml
# Monthly spend budget in USD.
monthly_budget_usd = 50.0

# Port for the HTTP API server (GET /spend/monthly, GET /status).
api_port = 8788

# Port for the OTLP HTTP receiver (optional — used only for OTEL cross-check).
otel_port = 4318
```

Port overrides are also available as CLI flags:

```bash
trakr serve --api-port 9090 --otel-port 5318
```

If you change `otel_port` and are using the optional OTEL cross-check, update `OTEL_EXPORTER_OTLP_ENDPOINT` in `~/.claude/settings.json` to match, then start a new Claude Code session.

---

## All commands

| Command | Description |
|---|---|
| `trakr init` | Set up `~/.trakr/` (DB, directories, config) |
| `trakr status` | Health-check: DB freshness, server reachability; OTEL/env-var checks are informational |
| `trakr spend` | Month-to-date spend (runs inline sweep; no server required) |
| `trakr serve` | Run the HTTP API server, 30 s reconciliation loop, and daily archive sweep in the foreground |
| `trakr archive` | Mirror `~/.claude/projects/` → `~/.trakr/archive/` incrementally (also runs daily via `serve`) |
| `trakr repair` | Rebuild spend for sessions with synthetic `session_end` events from old backfill versions (`--dry-run` / `--run`) |
| `trakr install-service` | Install `trakr serve` as a macOS LaunchAgent (starts on login) |
| `trakr uninstall-service` | Stop and remove the LaunchAgent |
| `trakr logs` | Tail the server log (`~/.trakr/serve.log`) |
| `trakr list` | List all recorded sessions with event counts |
| `trakr show <session-id>` | Show a timeline of all events in a session |
| `trakr stats` | Aggregate stats: top tools, token totals, model distribution |
| `trakr backfill-logs` | Import sessions from Claude Code's native logs (`--project`, `--since`, `--dry-run`) |
| `trakr inspect-logs` | Read-only diagnostic of Claude's native logs vs the trakr DB |
| `trakr show-prompts <session-id>` | Print the user prompts from a Claude session log |
| `trakr migrate` | Import legacy per-session JSONL files into the unified DB |
| `trakr reset` | Clear all recorded data (prompts for confirmation) |

---

## Data storage

```
~/.trakr/
├── trakr.db          unified SQLite store (events + sessions tables, schema-versioned)
├── config.toml       budget and port config
├── serve.log         server log (when running as a LaunchAgent)
├── sessions/         legacy JSONL backups per session
├── transcripts/
│   ├── <session-id>.jsonl    full Claude transcript, archived at SessionEnd
│   └── ...
└── archive/          incremental mirror of ~/.claude/projects/ (canonical backup)
    └── <project-slug>/
        └── <session-id>.jsonl
```

Events recorded: `tool_use`, `session_start`, `session_end`, `token_usage`, `subagent_start`, `subagent_stop`, `context_compression`. The `sessions` table additionally holds `project_path`, `started_at`, `ended_at`, `model`, `title`, `summary`, `last_prompt`, `last_activity_at`, `file_size`, and `file_mtime` per session.

---

## Cost estimation

Rates used (June 2026 Anthropic rate card):

| Model | Input /MTok | Output /MTok |
|---|---|---|
| Haiku 4.5 | $1.00 | $5.00 |
| Sonnet 4.6 | $3.00 | $15.00 |
| Opus 4.7 / 4.8 | $5.00 | $25.00 |
| Fable 5 | $10.00 | $50.00 |

Cache read is billed at 10% of the input rate. Cache creation is billed at the full input rate. Unknown models fall back to Sonnet rates.

---

## Troubleshooting

**Spend looks too low**
Run `trakr spend` — it does an inline sweep before reading SQLite, so the server being down doesn't matter. If old sessions are missing, run `trakr backfill-logs` to re-import them from Claude's native logs. If you upgraded from a version that wrote synthetic `session_end` events into backfill output, run `trakr repair --dry-run` to see affected sessions, then `trakr repair --run` to rebuild them from the surviving transcripts.

**`trakr status` shows OTEL warnings**
OTEL is a cross-check only — spend is accurate from transcripts alone. If you want OTEL as a secondary confirmation, see [Optional: OTEL cross-check](#optional-otel-cross-check) below. OTEL warnings do not affect spend accuracy.

**OTEL endpoint conflicts**
trakr speaks OTLP **HTTP/JSON** only — `OTEL_EXPORTER_OTLP_PROTOCOL` must be `http/json`, not `grpc` or `http/protobuf`. If another collector already owns port 4318, change `otel_port` in config and the endpoint env var together.

**Server running an old binary**
After upgrading, restart the service: `trakr uninstall-service && trakr install-service`.

---

## Optional: OTEL cross-check

OTEL is **not required** for accurate spend tracking — transcripts are the single source of truth. If you want OTEL as a secondary confirmation, add these env vars to the `env` block of `~/.claude/settings.json`:

```json
{
  "env": {
    "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
    "OTEL_METRICS_EXPORTER": "otlp",
    "OTEL_EXPORTER_OTLP_ENDPOINT": "http://localhost:4318",
    "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json"
  }
}
```

These take effect in newly started Claude Code sessions. The first metrics batch arrives roughly 60 s in (Claude Code's default export interval). `trakr status` shows OTEL receiver state, but a missing or unconfigured OTEL receiver is normal and expected in the single-ledger architecture.

---

## Development

```bash
cargo build
cargo test
cargo doc --open
```

---

## License

MIT
