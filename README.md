# ctx-trakr

A Rust CLI that attaches to Claude Code hooks to track context usage and estimate spend across all your active sessions. Designed for multi-session tmux workflows where you want a single aggregated month-to-date cost view.

## Features

- **Hook integration** — records tool use, session start/end, token usage, subagent spawns, and context compression events via Claude Code hooks
- **Cost estimation** — token counts × published Anthropic rate card, per model (Haiku / Sonnet / Opus / Fable)
- **Month-to-date spend** — aggregates completed sessions (SQLite) + live active sessions (OTEL) against a configurable monthly budget
- **HTTP API** — `GET /spend/monthly` for tmux status-line polling
- **OTEL receiver** — embedded OTLP HTTP endpoint accepts `claude_code.cost.usage` metrics from all active Claude Code sessions
- **SQLite storage** — persistent event log with JSONL backup files per session

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

---

## Quick start

### 1. Initialise

```bash
ctx-trakr init
```

Creates `~/.ctx-trakr/` with:
- `ctx-trakr.db` — unified SQLite event store
- `sessions/` — JSONL backup files per session
- `config.toml` — budget and port config (see [Configuration](#configuration))

Prints the hook config snippet to add to `~/.claude/settings.json`.

### 2. Register hooks

Add this to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "ctx-trakr hook tool-use" }]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [{ "type": "command", "command": "ctx-trakr hook session-end" }]
      }
    ]
  }
}
```

### 3. Start tracking

Use Claude Code normally. Events are recorded automatically on every tool use and session end.

---

## Month-to-date spend

### Quick check (no server needed)

```bash
ctx-trakr spend
# $0.24 / $50.00  (7 completed session(s) in 2026-06)
# (SQLite only — start `ctx-trakr serve` for live active-session data)
```

This reads directly from SQLite. It shows completed sessions only — it does not include costs from currently active sessions.

### Full live view (with OTEL)

For live multi-session aggregation, run the background server:

```bash
ctx-trakr serve
# ctx-trakr: API server listening on http://127.0.0.1:8787
# ctx-trakr: OTEL receiver listening on 127.0.0.1:4318
```

Then configure Claude Code to emit OTEL metrics to ctx-trakr. Add to your shell profile or Claude Code environment:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_EXPORTER_OTLP_PROTOCOL=http/json
```

Query the API:

```bash
curl -s http://localhost:8787/spend/monthly
```

```json
{
  "period": "2026-06",
  "spent_estimated_usd": 8.42,
  "budget_usd": 50.0,
  "sources": {
    "completed_sessions_usd": 7.10,
    "completed_sessions_count": 12,
    "active_sessions_usd": 1.32
  },
  "note": "Costs are estimates based on the published Anthropic rate card."
}
```

The server excludes completed sessions from the OTEL live total to avoid double-counting.

### tmux status line

Add a script that polls the API and formats the result for your status line. Example using `jq`:

```bash
#!/bin/bash
# ~/.local/bin/ctx-trakr-status
result=$(curl -sf http://localhost:8787/spend/monthly 2>/dev/null) || exit 0
spent=$(echo "$result" | jq -r '"$\(.spent_estimated_usd)"')
budget=$(echo "$result" | jq -r '"$\(.budget_usd)"')
echo "$spent / $budget"
```

In `.tmux.conf`:

```tmux
set -g status-right "#(~/.local/bin/ctx-trakr-status) | %H:%M"
set -g status-interval 30
```

---

## Configuration

Edit `~/.ctx-trakr/config.toml` (created by `ctx-trakr init`):

```toml
# Monthly spend budget in USD.
monthly_budget_usd = 50.0

# Port for the HTTP API server (GET /spend/monthly).
api_port = 8787

# Port for the OTLP HTTP receiver.
otel_port = 4318
```

Port overrides are also available as CLI flags:

```bash
ctx-trakr serve --api-port 9090 --otel-port 5318
```

---

## All commands

| Command | Description |
|---|---|
| `ctx-trakr init` | Initialise `~/.ctx-trakr/`, create config, print hook snippet |
| `ctx-trakr hook tool-use` | Handle a PostToolUse hook event (reads JSON from stdin) |
| `ctx-trakr hook session-end` | Handle a Stop hook event (reads JSON from stdin) |
| `ctx-trakr spend` | Print month-to-date spend from SQLite (no server required) |
| `ctx-trakr serve` | Start the HTTP API server and OTEL receiver |
| `ctx-trakr list` | List all recorded sessions with event counts |
| `ctx-trakr show <session-id>` | Show a timeline of all events in a session |
| `ctx-trakr stats` | Aggregate stats: top tools, token totals, model distribution |
| `ctx-trakr migrate` | Import existing JSONL files into the unified SQLite DB |
| `ctx-trakr reset` | Clear all recorded data (prompts for confirmation) |

---

## Data storage

```
~/.ctx-trakr/
├── ctx-trakr.db      unified SQLite event store
├── config.toml       budget and port config
└── sessions/
    ├── <session-id>.jsonl    JSONL backup per session
    └── ...
```

Events recorded: `tool_use`, `session_start`, `session_end`, `token_usage`, `subagent_start`, `subagent_stop`, `context_compression`.

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

## Development

```bash
cargo build
cargo test
cargo doc --open
```

---

## License

MIT
