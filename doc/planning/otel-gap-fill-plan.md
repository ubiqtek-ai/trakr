# OTEL Gap-Fill Plan

**Goal:** Close the remaining ~9% spend gap ($23/month on a $255 bill) caused by background
API calls (title generation, compact-summary generation — predominantly Haiku) that Claude
Code bills but never writes to session transcripts.

**Positioning:** OTEL is an **optional enhancement**. `trakr spend` remains fully functional
without it; OTEL adds an estimated "background spend" line to make the figure more accurate.
Enabled via `trakr otel install`; disabled via `trakr otel uninstall`.

---

## Phase A — Attribute Dump Experiment

**Must be done first.** The gap-fill design (Option 2 or 3) depends entirely on which
attributes Claude Code emits on its OTEL metric/log data points. This experiment takes ~30
minutes and produces the evidence needed to choose a branch.

### A.1 — Instrument the receiver for raw logging (`src/otel_receiver.rs`) ✓ DONE

- `dump_to_jsonl()` helper appends raw bytes (best-effort) to `~/.trakr/<filename>`
- `handle_metrics` calls `dump_to_jsonl("otel-dump-metrics.jsonl", &body)` before parsing
- `POST /v1/logs` → `handle_logs` dumps to `otel-dump-logs.jsonl`

### A.2 — Enable OTEL env vars ✓ DONE (via `trakr otel enable`)

Run `trakr otel enable` — it writes the env vars, sets `otel_enabled = true` in config,
and restarts the daemon. Previously described as a manual step; now automated.

Env vars written to `~/.claude/settings.json` `env` block:
```json
"CLAUDE_CODE_ENABLE_TELEMETRY": "1",
"OTEL_METRICS_EXPORTER": "otlp",
"OTEL_LOGS_EXPORTER": "otlp",
"OTEL_EXPORTER_OTLP_ENDPOINT": "http://localhost:4318",
"OTEL_EXPORTER_OTLP_PROTOCOL": "http/json"
```
Start a fresh Claude Code session (env vars only apply to new sessions). Run for a few
minutes, make some tool calls, let background tasks fire (title generation usually fires
within 10–30 s of the first response).

### A.3 — Read the dump and answer these questions

From `otel-dump-metrics.jsonl`:
- Which attribute keys appear on `claude_code.cost.usage` data points?
- Does `claude_code.token.usage` exist? If so, which attributes? (model? token_type?)
- Is there any per-request/message identifier on a metric data point?

From `otel-dump-logs.jsonl` (if non-empty):
- Do Claude Code logs/spans carry a request or message identifier (`message.id`,
  `request.id`, `anthropic.message_id`, etc.)?
- Which attributes accompany it (model, session.id, cost, tokens)?

### A.3 — Decision gate

| What the dump shows | Implementation branch |
|---|---|
| Log stream carries per-request ID (any field) | **Option 2** — per-request-id dedup |
| Only `session.id` + `model` + `token_type` on metric | **Option 3** — per-model token subtraction |
| Only `session.id` + cost | **Abort** — cost-only subtraction is rate-card drift noise; document gap and stop |

---

## Phase B — Config flag & `trakr otel enable/disable` ✓ DONE

### B.1 — Config (`src/config.rs`) ✓ DONE
- `otel_enabled: bool` (`#[serde(default)]`, default false) added to `Config`
- `otel_port: u16` (`default_otel_port() = 4318`) added to `Config`
- `save_config(config: &Config)` added — serialises full struct to TOML, overwrites file
- `write_default_config()` updated to include `otel_enabled = false` and `otel_port = 4318`
- `Config` gains `Serialize` derive

### B.2 — CLI subcommands (`src/main.rs`) ✓ DONE
- `trakr otel enable` (was `install` in original plan) — sets flag, writes env vars, restarts daemon
- `trakr otel disable` (was `uninstall`) — clears flag, removes env vars, restarts daemon
- Env vars written: `CLAUDE_CODE_ENABLE_TELEMETRY`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`

### B.3 — Daemon (`src/main.rs` → `cmd_serve`) ✓ DONE
- `start_otel_receiver` spawned only when `cfg.otel_enabled = true`; uses `cfg.otel_port`
- Startup log line includes `otel=enabled(:4318)` or `otel=disabled`

### B.4 — `trakr status` ✓ DONE
- Shows `ℹ OTEL receiver  disabled — run trakr otel enable to start gap-fill` when off
- Shows `ℹ OTEL receiver  enabled on port 4318` when on

### B.5 — `trakr init`
No change needed — `write_default_config()` already includes the new fields.

---

## Phase C — Gap-fill logic (branch chosen after Phase A)

### Option 2 — Per-request-id dedup (if log stream carries request IDs)

**New DB table: `otel_background_events`**
```sql
CREATE TABLE IF NOT EXISTS otel_background_events (
    message_id      TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    model           TEXT NOT NULL,
    cost_usd        REAL NOT NULL,
    first_seen_at   TEXT NOT NULL,   -- ISO 8601, when OTEL batch arrived
    confirmed_at    TEXT             -- NULL = unconfirmed; set when age > CONFIRM_AFTER
);
```

**`src/otel_receiver.rs` — extended log handler:**
- Parse `/v1/logs` batch; for each log record with a request/message ID and model=haiku:
  - Look up `message_id` in the existing `seen_message_ids` set (persisted from transcript
    parsing — see note below).
  - Present → drop (transcript already counts it).
  - Absent → `INSERT OR IGNORE INTO otel_background_events (message_id, session_id, model,
    cost_usd, first_seen_at)`.

**Note on `seen_message_ids` persistence:** The dedup set currently lives only in memory
during a single `parse_session_log` call. To make cross-call lookup work we need a
lightweight persistent index. Options:
  - New `seen_message_ids` table in `trakr.db` (simplest — one row per ID, session FK).
  - Or query the JSONL directly at lookup time (slower but no schema change).
  Recommend the DB table.

**Reconciliation (in the 30s sync loop):**
```rust
// For each unconfirmed otel_background_events row:
//   if message_id now in seen_message_ids → DELETE (transcript claimed it)
//   else if age(first_seen_at) > CONFIRM_AFTER (10 min) → SET confirmed_at = now()
```

**`trakr spend` output:**
```
Spend for June 2026 (33 sessions)
---------------------------------
  Cost (transcripts)   $232.01
  Background (OTEL)     $22.80   ← sum of confirmed otel_background_events for the month
  ─────────────────────────────
  Total (estimated)    $254.81
  Budget               $200.00
  Used                   127%
```

---

### Option 3 — Per-model token subtraction (if only model + token-type on metric)

**Requires:** `claude_code.token.usage` metric with `model` + `token_type` attributes on
each data point. If absent, abort (see decision gate in A.3).

**`src/otel_receiver.rs` — extended metric handler:**
- Parse `claude_code.token.usage` data points alongside `claude_code.cost.usage`.
- Accumulate into a new shared state map: `HashMap<(session_id, model), TokenCounts>`
  (input, output, cache_creation, cache_read — no 1h/5m split available via OTEL).

**`src/storage.rs` — new function `upsert_otel_token_background`:**
- Called from the 30s sync loop after every transcript resample.
- For each `(session_id, model)` pair with OTEL token data:
  - Fetch transcript token totals for the same `(session_id, model)` from `events` table.
  - `background_tokens = max(0, otel_tokens − transcript_tokens)` per token type.
  - Price with our rate card: `compute_cost_usd_with_card(model, background_tokens...)`.
    Note: no 1h/5m split available from OTEL tokens, so all cache_creation priced at 5m
    rate (1.25×) — slight undercount but acceptable and documented.
  - Upsert result into a `otel_background_spend` table:
    ```sql
    CREATE TABLE IF NOT EXISTS otel_background_spend (
        session_id       TEXT NOT NULL,
        model            TEXT NOT NULL,
        background_usd   REAL NOT NULL,
        updated_at       TEXT NOT NULL,
        PRIMARY KEY (session_id, model)
    );
    ```

**`trakr spend` output:** same format as Option 2 — transcript line + background line +
total.

**Timing / convergence:** because both sides are read at the same point in the 30s sync
loop (after transcript resample), the race window is minimal. `max(0, …)` absorbs any
ordering artefact where OTEL has arrived but transcript hasn't caught up yet.

---

## Phase D — UX polish

- `trakr spend --json`: add `background_otel_usd` and `total_estimated_usd` fields.
- `trakr inspect`: add a "Background (OTEL)" row in the spend summary when OTEL is enabled
  and has data.
- README: add "Optional: OTEL gap-fill" section explaining the install command, what it
  improves, and the known limitation (cache_creation priced at 5m rate in Option 3).
- `trakr status`: when OTEL enabled and no batches received in >90 s, warn
  `⚠ OTEL — no data yet (start a new Claude Code session)`.

---

## Implementation sequence

```
A.1–A.3  Dump experiment         (do first — gates C)
B.1–B.5  Config + CLI            (parallel with A; no dump dependency)
C        Gap-fill logic          (after A; branch chosen by dump)
D        UX polish               (after C)
```

Estimated scope: A ≈ 1h, B ≈ 2h, C ≈ 3–4h (either branch), D ≈ 1h.
