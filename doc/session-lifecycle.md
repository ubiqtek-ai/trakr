# Session lifecycle: classifying sessions correctly for spend tracking

Status: **superseded** — the discussion moved on to an event-sourced design; see
[event-sourced-sessions.md](event-sourced-sessions.md). The three-category model
below still stands; the "assume ended, promote on evidence" proposal and Q1/Q3
are replaced. Q2 (delta vs cumulative) remains open and is carried forward.

This doc captures the session-classification problem discovered on 2026-06-11, the
interim fix, and a proposed evidence-based design. The open questions at the end are
to be discussed and resolved one by one before further implementation.

---

## Background: what happened this session

The OTEL pipeline was verified end-to-end for the first time:

1. A fresh Claude Code session picked up the env vars written by `trakr init`
   (`CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_METRICS_EXPORTER=otlp`,
   `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318`,
   `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`).
2. The receiver ingested live batches and `trakr spend` showed an active-session
   line for the first time, correctly summed with completed sessions.
3. An `active_sessions_count` field was added to `/spend/monthly`, and the spend
   table label changed to `Active sessions (N)`.

Then the active line **vanished** from `trakr spend`, which exposed a design bug.

## The bug

A test instance of `trakr serve` (started on alternate ports to verify the count
change) ran the **startup reconciliation sweep** against the real DB. The sweep's
rule was:

> any session log with no `session_end` in the DB ⇒ the SessionEnd hook must have
> been missed ⇒ backfill the session, **stamping a `session_end`**.

But a session that is **still running** also has no `session_end` — it hasn't ended
yet. The sweep cannot tell "ended but unrecorded" apart from "still in progress",
so it stamped the two live sessions as completed (`source=backfill`).

Downstream effect: `/spend/monthly` excludes completed session IDs from the live
OTEL total (the double-count guard), so both running sessions disappeared from the
"Active sessions" line. Their spend up to the stamp time was counted in the
completed bucket; spend after it was invisible and would stay so until each session
really ended (the real SessionEnd hook does a full replace, so the data self-heals).

The test run only *exposed* the bug. The same thing happens on every restart of the
real server (reboot, upgrade) while sessions are open — which, in a
leave-sessions-open-for-days workflow, is essentially always.

## The three categories of session

For spend purposes there are three distinct states (only the first is ever known
with certainty):

| # | State | How we know |
|---|-------|-------------|
| 1 | **Known complete** | The `SessionEnd` hook fired and trakr recorded it. This is the *only* definite signal — everything else is inference. |
| 2 | **Actually active** | Still open in real life. May have been idle for days or weeks — idleness does not mean ended. |
| 3 | **Ended, but unhooked** | Really did end (crash, kill, machine shutdown) but the SessionEnd hook never reached trakr. |

The whole problem is distinguishing 2 from 3: both look identical in the DB (a
`session_start`, no `session_end`) and in the logs (a transcript with no end
marker). Category 3 must be backfilled and counted as completed; category 2 must be
left alone and fed from the live OTEL stream.

## Interim fix (implemented, in working tree)

A liveness *prediction* based on log file mtime:

- `backfill::looks_active(path)` — true if the session log was modified within
  `ACTIVE_LOG_WINDOW` (24 h).
- The startup reconciliation sweep skips such sessions (reported as
  "left N possibly-active session(s) alone").
- `trakr backfill-logs` skips them too, printing `[live?]`, with a `--force`
  override.
- 3 new unit tests; 58 passing. Verified by dry run: 16 possibly-active sessions
  correctly left alone, 42 skipped as already complete.

**Why this is not good enough:** it *predicts* liveness from recency. Any threshold
fails for genuinely-open-but-idle sessions older than the window — exactly the
leave-it-open-for-weeks workflow this tool is built for. Those sessions would still
be wrongly stamped as ended.

## Proposed design: assume ended, promote on evidence

> Assume a session is in category 3 (ended-unhooked) until an OTEL message arrives
> for it, at which point it returns to category 2 (active).

Instead of predicting liveness, classify pessimistically and **correct on
evidence**. This is safe because the failure mode is harmless by construction:

- Misclassifying an *idle* open session as "ended" costs nothing — its spend so far
  is captured in the DB snapshot, and it is currently spending nothing. The spend
  total remains accurate even for sessions idle for weeks.
- The moment the session is used again, it emits OTEL metrics. That is hard
  evidence it is alive, and it is reclassified as active.
- When it genuinely ends, the real SessionEnd hook replaces the record with final
  ground-truth numbers.

A bonus: this also correctly handles a session that was genuinely hook-ended and
later *resumed* — fresh OTEL evidence reclassifies it regardless of what the DB
recorded.

## Open questions (to discuss one by one)

### Q1 — How "promotion back to active" should work mechanically

Proposal: make it a **query-time decision, not a DB rewrite**. The `sessions` table
already stores `ended_at` and `source`. The spend endpoint's rule would become:

> a session counts as *active* if the OTEL receiver has heard from it **after** its
> recorded `ended_at` (or it has no `ended_at` at all).

The DB keeps its snapshot untouched; fresh OTEL evidence simply overrides the
interpretation at read time. No state flipping, no extra writes.

Required change: the receiver currently keeps only one global `last_received`
timestamp. It would need a per-session `last_seen` map alongside the per-session
cost map.

### Q2 — Delta vs cumulative: how Claude Code reports cost over OTEL

The receiver currently **adds** every incoming data point to the session's running
total (`session_costs[id] += value`). That is only correct if Claude Code exports
cost as **deltas** (each batch = spend since the previous batch).

If Claude Code instead exports **cumulative totals** (each batch = total spend since
session start — the OTLP default temporality for sums), then:

- today's receiver is *already overstating* live spend on every batch, and
- the arithmetic for a promoted session changes.

How a promoted session's spend is counted in each case:

- **Deltas:** DB snapshot (spend up to the backfill stamp) and OTEL accumulation
  (spend since the server started) cover disjoint time ranges — simply add them.
- **Cumulative:** the OTEL value already includes everything since session start,
  so adding double-counts — take `max(DB snapshot, OTEL value)` per session
  instead, and the receiver should *replace* the stored value, not add.

This is empirically checkable: log two consecutive batches from one session and see
whether the values are small increments or a growing total. Worth doing regardless
of the promotion design, since it affects live-spend accuracy today.

### Q3 — Grace window for brand-new sessions

A just-started session has no OTEL evidence until its first export (~60 s, the
default metrics export interval). A startup sweep running in that window would
stamp it as ended, then promotion would rescue it ~a minute later — correct but
noisy (the session flickers through the completed bucket).

Proposal: keep a *short* mtime guard purely as a noise filter — skip logs written
in the last few minutes. This is the only remnant of the interim 24 h guard worth
keeping; it demotes mtime from classification mechanism to debounce.

---

## Current state of the code (end of session 2026-06-11)

- Working tree (uncommitted): `active_sessions_count` in API + plural spend label;
  interim mtime guard (`looks_active`, sweep skip, `[live?]`, `--force`); README
  rewritten for current architecture; 58 tests passing.
- The running launchd service still has the previous binary — none of today's
  changes are live yet.
- The two sessions wrongly stamped on 2026-06-11 (~20:16 UTC) will self-heal when
  they genuinely end.
