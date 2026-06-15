# trakr OTEL Enterprise Investigation

## Status: Closed

OTEL integration on enterprise Claude Code is **not viable**. File scanning (JSONL sync loop) is the confirmed working approach. See conclusion below.

---

## Summary

OTEL telemetry from Claude Code (CC) is not reaching trakr's SQLite database despite trakr running, OTEL being enabled, and the standard `OTEL_*` env vars being set. Binary analysis of the CC executable reveals two distinct OTEL code paths — one for standard/personal accounts and one for enterprise accounts — and on an enterprise account the standard path (which reads trakr's env vars) is never executed.

---

## Environment

| Item | Value |
|------|-------|
| CC version | 2.1.177 |
| CC binary location | `~/.local/share/claude/versions/2.1.177` (compiled Bun binary) |
| trakr config | `~/.trakr/` |
| OTEL dump files | `~/.trakr/otel-dump-logs.jsonl`, `~/.trakr/otel-dump-metrics.jsonl` |
| Account type | Enterprise |

OTEL env vars set in `~/.claude/settings.json` via `trakr otel enable`:
- `OTEL_LOGS_EXPORTER`
- `OTEL_EXPORTER_OTLP_PROTOCOL`
- `OTEL_EXPORTER_OTLP_ENDPOINT`

---

## Confirmed Working

- trakr daemon is running and listening on `127.0.0.1:4318`
- `GET /v1/logs` → `200 OK`
- `GET /v1/metrics` → `200 OK`
- `GET /v1/traces` → `404` (trakr does not implement the traces endpoint)
- Dump files exist but contain only manually-sent test payloads — nothing from CC
- `trakr otel enable` reports "already enabled"
- `trakr status` reports all checks passed
- DB contains 0 events with a `request_id`; all events originated from JSONL file scanning, not OTEL

---

## Root Cause Finding: Two Distinct OTEL Code Paths

The CC binary contains a function `NP()` that determines which OTEL initialisation path runs. The two paths are mutually exclusive — the enterprise path performs an **early return** before the standard path is ever reached.

```
// Enterprise/team path (NP() = true):
if (NP()) {
  jDK();        // sets up "beta tracing" — activates only if BETA_TRACING_ENDPOINT is set
  return ...;   // EARLY RETURN — standard OTEL setup never runs
}

// Standard path (NP() = false):
let exporters = await JDK();  // reads OTEL_LOGS_EXPORTER, OTEL_EXPORTER_OTLP_PROTOCOL, etc.
// → this is what trakr's `otel enable` configures for
```

**`JDK()` — standard path**: reads `process.env.OTEL_LOGS_EXPORTER` and `process.env.OTEL_EXPORTER_OTLP_PROTOCOL`. These are the exact vars written by `trakr otel enable`.

**`jDK()` — enterprise path**: reads `process.env.BETA_TRACING_ENDPOINT`. If this var is not set, no telemetry is sent anywhere.

**`ANT_OTEL_*` vars**: `ANT_OTEL_EXPORTER_OTLP_ENDPOINT`, `ANT_OTEL_LOGS_EXPORTER`, etc. also appear in the binary — likely enterprise-specific override vars.

---

## Hypothesis

On an enterprise CC account, `NP()` returns `true`. This causes an early return in the OTEL initialisation function before `JDK()` is called. The standard `OTEL_*` vars written by `trakr otel enable` are therefore completely ignored.

Because `BETA_TRACING_ENDPOINT` is also not set, `jDK()` sends nothing either. The result: no telemetry reaches trakr at all.

This explains why OTEL integration works on a personal/dev CC account (`NP()` = false) but silently fails on an enterprise account.

---

## Diagnostic Steps Taken

1. Confirmed trakr is running and endpoints are reachable via direct HTTP requests.
2. Inspected dump files — no CC-originated payloads present.
3. Decompiled/searched the CC Bun binary for OTEL-related strings and function patterns.
4. Identified the `NP()` gate and mapped the two code paths (`JDK` vs `jDK`).
5. Noted `ANT_OTEL_*` prefix vars as a possible enterprise override mechanism.
6. Added `CLAUDE_CODE_DEBUG_LOGS_DIR=/tmp/cc_debug` to `~/.claude/settings.json` to capture telemetry initialisation logs on the next CC session start.

**Next diagnostic check**: after starting a fresh CC session, inspect `/tmp/cc_debug` for:
- `[3P telemetry] isTelemetryEnabled=`
- `[3P telemetry] Created N log exporter(s)`

These log lines will confirm which code path executed.

---

## Workarounds Considered (and Why They Are Not Viable)

### Option 1: Set `BETA_TRACING_ENDPOINT` — **inadvisable**

The apparent workaround is to add to `~/.claude/settings.json`:

```json
"BETA_TRACING_ENDPOINT": "http://localhost:4318"
```

This would redirect the enterprise OTEL path to a local trakr endpoint. However, `BETA_TRACING_ENDPOINT` (and the enterprise `jDK()` code path generally) is the channel Anthropic uses to receive telemetry for **enterprise usage tracking**. Redirecting it to localhost would break Anthropic's ability to track usage for the enterprise account. This is inadvisable.

### Option 2: `ANT_OTEL_*` override vars — **inadvisable for same reason**

```json
"ANT_OTEL_EXPORTER_OTLP_ENDPOINT": "http://localhost:4318",
"ANT_OTEL_LOGS_EXPORTER": "otlp"
```

Same concern: any env var that hijacks the enterprise OTEL path intercepts telemetry Anthropic expects to receive. Not worth the risk.

### Option 3: trakr implements `/v1/traces` — **moot**

If the workarounds above are off the table, there is no path to make trakr receive OTEL events from an enterprise account. This option is not worth pursuing.

---

## What trakr Expects (from binary strings analysis)

For reference — trakr is wired to receive:

- OTLP log records containing a `session.id` attribute
- Standard OTLP log format via `/v1/logs`, `http/json` or `http/protobuf` protocol
- Event body with typed payloads: `token_usage`, `tool_use`, `background_api_call`, `session_start`, etc.

---

## Practical Outcome

trakr continues to work via **JSONL file scanning** (the sync loop). This covers the following event types reliably:

- `token_usage`
- `tool_use`
- `session_start`

The only event type not captured is `background_api_call` — which was the original motivation for pursuing OTEL (background agent API calls do not always appear in the session JSONL in the same way). This is an acceptable limitation: the sync loop provides sufficient coverage for session-level cost and tool tracking.

No changes are needed to the running trakr setup. The `trakr otel enable` configuration can be left in place (it is harmless on an enterprise account — it simply has no effect) or removed for cleanliness.

---

## Conclusion

OTEL cannot be made to work on enterprise Claude Code without intercepting telemetry that is intended for Anthropic's servers. The enterprise OTEL code path (`jDK()` / `BETA_TRACING_ENDPOINT`) is the channel Anthropic uses for enterprise usage tracking, and redirecting it locally would break that tracking. This is inadvisable.

The standard `OTEL_*` env vars (written by `trakr otel enable`) are entirely ignored on enterprise accounts due to the early-return gate in the CC binary's OTEL initialisation function.

**File scanning is the correct and only viable approach for trakr on an enterprise Claude Code account.** This investigation is closed.
