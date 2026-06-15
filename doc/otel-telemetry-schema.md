# Claude Code OTEL Telemetry Schema

Findings from Phase A of the OTEL gap-fill experiment (2026-06-15). Raw payloads
captured by enabling `trakr otel enable` and running a live session.

---

## Overview

Claude Code emits OTLP telemetry over HTTP/JSON (protobuf not supported). Two
endpoints are used:

| Endpoint | Content |
|---|---|
| `POST /v1/metrics` | Cumulative session-level counters (cost, tokens, active time) |
| `POST /v1/logs` | Per-event structured log records (one per API call, hook, tool, etc.) |

Resource attributes on every batch:

```json
{
  "host.arch": "arm64",
  "os.type": "darwin",
  "os.version": "25.5.0",
  "service.name": "claude-code",
  "service.version": "2.1.177"
}
```

Scope: `com.anthropic.claude_code` / `com.anthropic.claude_code.events`.

---

## Metrics

### Common data-point attributes

Every metric data point carries these user/session attributes:

```
user.id            — hashed user identifier
session.id         — Claude Code session UUID
organization.id    — org UUID
user.email         — plaintext email
user.account_uuid  — account UUID
user.account_id    — account ID string (user_01...)
terminal.type      — "tmux", "terminal", etc.
```

### `claude_code.cost.usage` (USD, monotonic sum)

Cost incurred by a block of API calls. Cumulative within the session export interval.

Extra attributes:
```
model         — e.g. "claude-sonnet-4-6"
query_source  — "main" | "auxiliary" (see below)
effort        — "high" | "normal" | "low"
```

Example data point:
```json
{
  "attributes": [
    {"key": "model",        "value": {"stringValue": "claude-sonnet-4-6"}},
    {"key": "query_source", "value": {"stringValue": "auxiliary"}},
    {"key": "effort",       "value": {"stringValue": "high"}}
  ],
  "startTimeUnixNano": "1781540956322000000",
  "timeUnixNano":      "1781540980934000000",
  "asDouble": 0.014071199999999999
}
```

### `claude_code.token.usage` (tokens, monotonic sum)

One data point per token type per query_source block. Extra attributes:

```
model         — model name
query_source  — "main" | "auxiliary"
effort        — "high" | "normal" | "low"
type          — "input" | "output" | "cacheRead" | "cacheCreation"
```

Example (four data points for one auxiliary block):
```
type=input        72
type=output       45
type=cacheRead    43934
type=cacheCreation 0
```

Note: no 1h/5m cache-creation tier split is available via OTEL metrics (unlike the
session transcript JSONL which has `usage.cache_creation.ephemeral_1h_input_tokens`).

### `claude_code.session.count` (monotonic sum)

Fired once at session start.

Extra attributes: `start_type` — `"fresh"` | `"resumed"`.

### `claude_code.active_time.total` (seconds, monotonic sum)

Two data points per batch: `type=user` and `type=cli`.

---

## Logs

### `claude_code.api_request` — the key record for spend tracking

One log record per Anthropic API call. Contains per-request cost and tokens already
computed by Claude Code.

**All attributes:**

```
request_id              — Anthropic API request ID, e.g. "req_011Cc5N2v7Hr1hvHPWa4aBwE"
prompt.id               — Claude Code turn UUID (shared across retries of the same turn)
model                   — model name
query_source            — what triggered the call (see values below)
effort                  — "high" | "normal" | "low"
speed                   — "normal" | "fast"
input_tokens            — integer
output_tokens           — integer
cache_read_tokens       — integer
cache_creation_tokens   — integer
cost_usd                — float (exact, computed by Claude Code)
cost_usd_micros         — integer (cost_usd × 1,000,000)
duration_ms             — request latency
event.name              — "api_request"
event.timestamp         — ISO 8601, e.g. "2026-06-15T16:31:21.073Z"
event.sequence          — int, monotonic within session
+ standard user/session attributes
```

### `query_source` values observed

| value | meaning | visible in transcript? |
|---|---|---|
| `repl_main_thread` | Normal user-driven request (the main conversation turn) | Yes |
| `generate_session_title` | Background Haiku call to generate the session title | **No** |
| `auxiliary` | Other background/preload calls (seen on metrics; likely matches `auxiliary` in logs too) | **No** |

The `repl_main_thread` requests appear in the session JSONL as assistant messages and
are fully accounted for by trakr's transcript-based tracking. All other `query_source`
values represent calls **invisible to transcripts** and are the source of the spend gap.

### Example — main thread request (Sonnet)

```json
{
  "body": {"stringValue": "claude_code.api_request"},
  "attributes": {
    "prompt.id":             "432b91c9-6f35-42aa-9923-21a0a1cc76ac",
    "model":                 "claude-sonnet-4-6",
    "input_tokens":          3,
    "output_tokens":         458,
    "cache_read_tokens":     30906,
    "cache_creation_tokens": 19688,
    "cost_usd":              0.1342818,
    "cost_usd_micros":       134282,
    "duration_ms":           7522,
    "request_id":            "req_011Cc5N3RnjURswELQzvAFQ7",
    "speed":                 "normal",
    "query_source":          "repl_main_thread",
    "effort":                "high",
    "session.id":            "a40c3aae-3dd6-42c5-a5ca-1b5939742c8c"
  }
}
```

### Example — background Haiku title-generation call

```json
{
  "body": {"stringValue": "claude_code.api_request"},
  "attributes": {
    "prompt.id":             "df29b7ab-7d23-44a1-a485-1f38b83259e3",
    "model":                 "claude-haiku-4-5-20251001",
    "input_tokens":          532,
    "output_tokens":         19,
    "cache_read_tokens":     0,
    "cache_creation_tokens": 0,
    "cost_usd":              0.0006270000000000001,
    "cost_usd_micros":       627,
    "duration_ms":           998,
    "request_id":            "req_011Cc5N7mp7SfnJnR2CgM4Ha",
    "speed":                 "normal",
    "query_source":          "generate_session_title",
    "session.id":            "a40c3aae-3dd6-42c5-a5ca-1b5939742c8c"
  }
}
```

### Other log event types observed

| event body | purpose |
|---|---|
| `claude_code.hook_registered` | Fires at session start for each configured hook |
| `claude_code.hook_execution_start` | Hook invocation started |
| `claude_code.hook_execution_complete` | Hook invocation finished (`success`, `duration_ms`) |
| `claude_code.tool_decision` | Tool use approved/blocked (`decision`, `decision_source`) |
| `claude_code.tool_result` | Tool result returned (`tool_name`, `tool_result_size_bytes`) |
| `claude_code.user_prompt` | User prompt submitted (`prompt_length`) |

---

## Implications for trakr gap-fill

The `query_source` attribute makes the gap-fill straightforward — no dedup against
transcripts and no token subtraction required.

**Rule:** any `claude_code.api_request` log record where `query_source !=
"repl_main_thread"` is a background call that will never appear in the session JSONL.

**Implementation approach:**
- Parse `/v1/logs` in `otel_receiver.rs` for `claude_code.api_request` records
- Filter to non-`repl_main_thread` records
- Emit as a new `Event::BackgroundApiCall { request_id, session_id, model, cost_usd, query_source, ts }`
- Store in the existing events table — no new table, no separate reconciliation loop
- Spend query sums `BackgroundApiCall.cost_usd` alongside `TokenUsage` events
- `request_id` as a unique field provides natural idempotency (upsert or skip-if-exists)

This means `trakr spend` gains the background line purely from appended events —
consistent with the existing event-sourced architecture.

**Known `query_source` values that are background (will grow as more sessions are observed):**
- `generate_session_title`
- `auxiliary`
