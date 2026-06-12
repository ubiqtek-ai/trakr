# ctx-trakr

A Rust CLI (binary name: `trakr`) that tracks Claude Code context usage and estimates spend across all your active sessions. Designed for multi-session tmux workflows where you want a single aggregated month-to-date cost view.

## Features

- **Hook integration** — `SessionStart`/`SessionEnd` hooks record sessions; at session end the full Claude transcript is parsed for ground-truth token counts (summed across all turns) and archived
- **Live OTEL metrics** — embedded OTLP HTTP receiver ingests `claude_code.cost.usage` metrics from all running Claude Code sessions, so spend updates while sessions are still active
- **Cost estimation** — token counts × published Anthropic rate card, per model (Haiku / Sonnet / Opus / Fable)
- **Month-to-date spend** — aggregates completed sessions (SQLite) + live active sessions (OTEL) against a configurable monthly budget, with double-count protection
- **Pipeline health check** — `trakr status` verifies the whole chain: settings, hooks, env vars, DB, server, OTEL receiver
- **Backfill & reconciliation** — imports historical sessions from Claude Code's native logs; a reconciliation sweep on server startup self-heals any missed `SessionEnd` hooks
- **Runs on login** — `trakr install-service` registers a macOS LaunchAgent
- **SQLite storage** — persistent event log, session metadata (title, summary, project), and archived transcripts

> Costs are estimates based on the published Anthropic rate card. Only the Anthropic Admin Cost API gives billed truth.

---

## Installation

### From source

```bash
git clone https://github.com/ubiqtek/ctx-trakr.git
cd ctx-trakr
cargo install --path .
```

### From crates.io (once published)

```bash
cargo install ctx-trakr
```

Both install a binary named **`trakr`**.

---

## Quick start

### 1. Initialise

```bash
trakr init
```

This does everything in one step:

- Creates `~/.trakr/` with the SQLite DB (`trakr.db`), `sessions/`, `transcripts/`, and `config.toml`
- Registers the `SessionStart` and `SessionEnd` hooks in `~/.claude/settings.json` (idempotent merge — your existing settings are preserved)
- Writes the OTEL telemetry env vars into the `env` block of `~/.claude/settings.json`:

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

Scoping the env vars to Claude Code's settings means no shell profile changes are needed. `CLAUDE_CODE_ENABLE_TELEMETRY` and `OTEL_METRICS_EXPORTER` are both required — without them Claude Code exports no telemetry at all.

### 2. Start the server

```bash
trakr install-service   # macOS LaunchAgent — starts now and on every login
# or, for a foreground run:
trakr serve
```

### 3. Restart your Claude Code sessions

**Env var changes only apply to newly started sessions.** Already-running Claude Code sessions will not export metrics. Start a new session, and expect the first metrics batch roughly a minute in — Claude Code exports OTEL metrics on an interval (~60 s by default).

### 4. Verify

```bash
trakr status
```

Checks the full pipeline and lists any problems with suggested fixes:

```
Claude Code settings  (~/.claude/settings.json)
------------------------------------------------------------
  ✓ SessionStart hook                trakr hook session-start
  ✓ SessionEnd hook                  trakr hook session-end
  ✓ CLAUDE_CODE_ENABLE_TELEMETRY     1
  ✓ OTEL_METRICS_EXPORTER            otlp
  ✓ OTEL_EXPORTER_OTLP_ENDPOINT      http://localhost:4318
  ✓ OTEL_EXPORTER_OTLP_PROTOCOL      http/json
...
  ✓ OTEL receiver                    1 batches, 1 active session(s), $0.27
```

---

## Month-to-date spend

```bash
trakr spend
```

```
Spend  2026-06  (budget $200.00)
------------------------------------------
  Completed sessions (42)        $  329.69
  Active sessions (1)            $    0.27
------------------------------------------
  Total                          $  329.96
```

`trakr spend` queries the live API first (completed + active sessions); if the server isn't running it falls back to SQLite and shows completed sessions only, with a note.

### HTTP API

```bash
curl -s http://localhost:8788/spend/monthly
```

```json
{
  "period": "2026-06",
  "spent_estimated_usd": 329.96,
  "budget_usd": 200.0,
  "sources": {
    "completed_sessions_usd": 329.69,
    "completed_sessions_count": 42,
    "active_sessions_usd": 0.27,
    "active_sessions_count": 1
  },
  "note": "Costs are estimates based on the published Anthropic rate card."
}
```

Sessions that already have a `session_end` in SQLite are excluded from the OTEL live total, so a session is never counted twice as it transitions from active to completed.

`GET /status` reports OTEL receiver health (batches received, last receive time, active sessions and their live cost) — this is what `trakr status` uses.

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

- **`SessionEnd` hook** is the source of truth for completed sessions. It parses Claude Code's native session log (the `transcript_path` from the hook payload), sums token usage across **all** turns, replaces any partial data for that session atomically, archives the full transcript to `~/.trakr/transcripts/`, and extracts the session title, compact summary, and last prompt into the `sessions` table.
- **OTEL receiver** covers the gap the hooks can't: sessions that are still running. Claude Code pushes `claude_code.cost.usage` metrics to `localhost:4318`, which the server aggregates per session ID.
- **Reconciliation sweep** on `trakr serve` startup backfills any sessions whose `SessionEnd` hook was missed (crash, force-quit) from Claude's native logs.
- Hook handlers always exit 0 — they never block Claude Code, even on error.

---

## Configuration

Edit `~/.trakr/config.toml` (created by `trakr init`):

```toml
# Monthly spend budget in USD.
monthly_budget_usd = 50.0

# Port for the HTTP API server (GET /spend/monthly, GET /status).
api_port = 8788

# Port for the OTLP HTTP receiver.
otel_port = 4318
```

Port overrides are also available as CLI flags:

```bash
trakr serve --api-port 9090 --otel-port 5318
```

If you change `otel_port`, update `OTEL_EXPORTER_OTLP_ENDPOINT` in `~/.claude/settings.json` to match, then start a new Claude Code session.

---

## All commands

| Command | Description |
|---|---|
| `trakr init` | Set up `~/.trakr/`, register hooks and OTEL env vars in Claude Code settings |
| `trakr status` | Health-check the full pipeline: settings, hooks, env vars, DB, server, OTEL |
| `trakr spend` | Month-to-date spend (live API with SQLite fallback) |
| `trakr serve` | Run the HTTP API server and OTEL receiver in the foreground |
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
| `trakr hook <event>` | Hook dispatcher used by Claude Code (`session-start`, `session-end`) |

---

## Data storage

```
~/.trakr/
├── trakr.db          unified SQLite store (events + sessions tables, schema-versioned)
├── config.toml       budget and port config
├── serve.log         server log (when running as a LaunchAgent)
├── sessions/         legacy JSONL backups per session
└── transcripts/
    ├── <session-id>.jsonl    full Claude transcript, archived at SessionEnd
    └── ...
```

Events recorded: `tool_use`, `session_start`, `session_end`, `token_usage`, `subagent_start`, `subagent_stop`, `context_compression`. The `sessions` table additionally holds `project_path`, `started_at`, `ended_at`, `model`, `title`, `summary`, and `last_prompt` per session.

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

**`trakr status` says the OTEL receiver has never received metrics**
The env vars in `~/.claude/settings.json` only take effect in *new* Claude Code sessions. Start a fresh session and wait ~60 s for the first export interval. Verify all four env vars are present (`trakr status` checks them individually).

**OTEL endpoint conflicts**
trakr speaks OTLP **HTTP/JSON** only — `OTEL_EXPORTER_OTLP_PROTOCOL` must be `http/json`, not `grpc` or `http/protobuf`. If another collector already owns port 4318, change `otel_port` in config and the endpoint env var together.

**Spend looks too low**
If the server isn't running, `trakr spend` shows completed sessions only. Sessions ended while nothing was tracking are recovered by the reconciliation sweep the next time `trakr serve` starts, or manually via `trakr backfill-logs`.

**Server running an old binary**
After upgrading, restart the service: `trakr uninstall-service && trakr install-service`.

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
