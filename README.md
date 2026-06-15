# trakr

A Rust CLI that tracks Claude Code context usage and estimates spend across all your active sessions. Designed for multi-session tmux workflows where you want a single aggregated month-to-date cost view.

## Features

- **Transcript-driven tracking** — Claude Code's native session transcripts (`~/.claude/projects/`) are the single source of truth for all spend; no OTEL pipeline required
- **Cost estimation** — token counts × published Anthropic rate card, per model (Haiku / Sonnet / Opus / Fable); spend is summed from all `TokenUsage` events regardless of whether a session has ended
- **Month-to-date spend** — aggregates all recorded sessions against a configurable monthly budget
- **OTEL gap-fill** — `trakr init` enables OTEL by default; background API calls (title/summary generation) that are invisible in transcripts are captured from the OTEL log stream and shown as a separate "Background" line in `trakr spend`
- **Manual adjustments** — `trakr adjust <day> <amount>` records a signed correction (positive or negative) attributed to a specific day; useful for pre-installation gaps or price-change corrections
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

Creates `~/.trakr/` with the SQLite DB (`trakr.db`), `sessions/`, `transcripts/`, `archive/`, and `config.toml`. Also writes the OTEL telemetry env vars to `~/.claude/settings.json` so background API call tracking is active from the first session (port 4318; disable with `trakr otel disable`).

### 2. Start the service

```bash
trakr install-service   # macOS LaunchAgent — starts now and on every login
# or, for a foreground run:
trakr serve
```

On startup the daemon runs a reconciliation sweep that imports all existing Claude Code sessions from `~/.claude/projects/` automatically — no separate backfill step needed.

### 3. Check spend

```bash
trakr spend
```

### 4. Verify pipeline health

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

### Manual spend adjustments

Record a one-off correction attributed to a specific day:

```bash
trakr adjust 2026-05-15 -48.00 --reason "Pre-installation gap — background Haiku calls"
trakr adjust 2026-06-01 3.50   --reason "Price change correction"
```

`amount` may be positive (to add spend) or negative (to subtract). Month attribution follows the `day` argument, not the insertion time. Multiple adjustments for the same month are summed.

When adjustments exist for the current month, `trakr spend` shows them as a separate line:

```
  Transcripts     $182.89
  Adjustment       -48.00
  Total           $134.89
```

Adjustments are stored as events in the DB with a full audit trail — they are never silently modified by reconciliation sweeps.

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
| `trakr init` | Set up `~/.trakr/` (DB, directories, config); enables OTEL by default |
| `trakr status` | Health-check: DB freshness, server reachability, OTEL state |
| `trakr spend` | Month-to-date spend (runs inline sweep; no server required) |
| `trakr adjust <day> <amount>` | Record a signed spend adjustment for a specific day (`--reason` optional) |
| `trakr sync` | Manually trigger a reconciliation sweep and print a summary |
| `trakr sync-rates` | Fetch current model pricing from LiteLLM and cache to `~/.trakr/rates.json` |
| `trakr serve` | Run the 30 s reconciliation loop and daily archive sweep (foreground) |
| `trakr archive` | Mirror `~/.claude/projects/` → `~/.trakr/archive/` incrementally (also runs daily via `serve`) |
| `trakr repair` | Rebuild spend for sessions with stale parser output (`--dry-run` to preview) |
| `trakr install-service` | Install `trakr serve` as a macOS LaunchAgent (starts on login) |
| `trakr uninstall-service` | Stop and remove the LaunchAgent |
| `trakr restart-service` | Restart the LaunchAgent (picks up a new binary after upgrade) |
| `trakr logs` | Tail the server log (`~/.trakr/serve.log`) |
| `trakr otel enable` | Enable OTEL: write env vars to `~/.claude/settings.json`, restart daemon |
| `trakr otel disable` | Disable OTEL: remove env vars from `~/.claude/settings.json`, restart daemon |
| `trakr list` | List all recorded sessions with event counts |
| `trakr show <session-id>` | Show a timeline of all events in a session |
| `trakr stats` | Aggregate stats: top tools, token totals, model distribution (`--month YYYY-MM`, `--verbose`) |
| `trakr inspect` | Read-only diagnostic of Claude's native logs vs the trakr DB (`--verbose` for per-session table) |
| `trakr backfill-logs` | Import sessions from Claude Code's native logs (`--project`, `--since`, `--dry-run`) |
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

Events recorded: `tool_use`, `session_start`, `session_end`, `token_usage`, `subagent_start`, `subagent_stop`, `context_compression`, `background_api_call` (OTEL), `cost_adjustment` (manual). The `sessions` table additionally holds `project_path`, `started_at`, `ended_at`, `model`, `title`, `summary`, `last_prompt`, `last_activity_at`, `file_size`, and `file_mtime` per session.

---

## Cost estimation

Rates are fetched daily from the [LiteLLM price list](https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json) and cached to `~/.trakr/rates.json`. Run `trakr sync-rates` to refresh manually. A hardcoded fallback is used if the cache is absent.

Fallback rates (June 2026 Anthropic published pricing):

| Model | Input /MTok | Output /MTok | Cache read /MTok | Cache write 5m /MTok | Cache write 1h /MTok |
|---|---|---|---|---|---|
| Haiku 4.5 | $1.00 | $5.00 | $0.10 | $1.25 | $2.00 |
| Sonnet 4.6 | $3.00 | $15.00 | $0.30 | $3.75 | $6.00 |
| Opus 4.7/4.8 | $5.00 | $25.00 | $0.50 | $6.25 | $10.00 |
| Fable 5 | $10.00 | $50.00 | $1.00 | $12.50 | $20.00 |

Claude Code uses two cache TTL tiers. The 1-hour tier (dominant in Claude Code — typically 70–85% of all cache writes) costs **2× the input rate**. The 5-minute tier costs **1.25×**. trakr reads the per-tier split from `usage.cache_creation.{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}` in each session transcript and prices them separately.

Unknown models fall back to Sonnet rates.

---

## Spend accuracy

trakr reads token usage directly from Claude Code's session transcripts — the same files Claude Code writes locally. This makes it accurate for everything that happens inside a session, including subagents.

**What trakr counts:**
- All `assistant` turns in main session files (deduped by `message.id` to avoid double-counting multi-block responses)
- All subagent files at `~/.claude/projects/<slug>/<uuid>/subagents/agent-*.jsonl`
- Both 1-hour and 5-minute cache creation tiers, priced separately

**Known gap (~5–10% of monthly spend):**
Claude Code makes background API calls — for session title generation (`ai-title`), compact summary generation, and similar housekeeping — that are billed by Anthropic but **never written to the local session transcript**. These are typically Haiku calls and account for the remaining gap between trakr's figure and the Anthropic dashboard. There is no way to capture these from transcripts alone.

**Comparison with other local trackers**
Some local trackers apply a calibration factor (e.g. 0.71×) to reconcile raw computed cost with Anthropic billing. This factor compensates for bugs in those tools — specifically, not deduplicating API responses by `message.id` (each multi-block assistant response is counted 2–3×) and using incorrect/outdated model prices. trakr fixes both issues at the source, so no calibration factor is needed. The remaining ~9% gap is from genuinely invisible background calls, not from overcounting.

**Future: Anthropic Analytics API**
Anthropic exposes an org-level Analytics API (`GET /v1/organizations/analytics/cost_report`) that returns pre-calculated spend figures in cents — authoritative billing data with no token multiplication required. Users with an org admin API key (`read:analytics` scope) could use this to close the gap entirely. This is planned as an optional "exact mode" for trakr (see roadmap in `doc/planning/plan.md`).

If you need exact figures today, the [Anthropic usage dashboard](https://console.anthropic.com) or a tool with Analytics API access is the authoritative source.

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

## Optional: OTEL gap-fill

OTEL is enabled by default when you run `trakr init`. It captures background API calls (session title generation, compact summary generation) that Claude Code bills but never writes to local transcripts — typically Haiku calls worth ~5–10% of monthly spend.

When OTEL data is present, `trakr spend` shows a breakdown:

```
Spend for June 2026 (36 sessions)
-------------------------
  Transcripts     $182.89
  Background        $0.09
  Total           $182.98
  Budget          $200.00
  Used              91.5%
-------------------------
```

To manage OTEL manually:

```bash
trakr otel enable    # write env vars to ~/.claude/settings.json, restart daemon
trakr otel disable   # remove env vars, restart daemon
```

OTEL env vars take effect in **newly started** Claude Code sessions (not the current one). The first data batch arrives ~60 s after a new session begins. trakr speaks OTLP **HTTP/JSON** only — `OTEL_EXPORTER_OTLP_PROTOCOL` must be `http/json`.

If port 4318 is already in use, change `otel_port` in `~/.trakr/config.toml` and run `trakr otel enable` again to update `~/.claude/settings.json`.

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
