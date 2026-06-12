# Event-sourced session state: facts, recalculation, and a projection

Status: **draft — captures the design discussion of 2026-06-11**

This supersedes the "assume ended, promote on evidence" proposal in
[session-lifecycle.md](session-lifecycle.md). The three-category model from that
doc still stands; the mechanism below replaces its Q1, and its Q2 (delta vs
cumulative OTEL values) remains the open question.

---

## The problem, restated

Every session falls into one of three categories:

1. **Known complete** — the SessionEnd hook fired and told us. The only category
   we can ever *prove*.
2. **Actually active** — still open in real life, even if untouched for days or
   weeks.
3. **Ended, but unhooked** — really did end, but the hook never reached us.

Our records only distinguish category 1. Everything else is "a session with no
end recorded", which could be 2 or 3 — and from the records alone we cannot tell
which.

## Where the old design went wrong

The event store was being polluted with guesses. When the backfill found a session
with no `session_end`, it *invented* one and wrote it into the event stream,
indistinguishable from a real observation. The stream became a mix of facts and
opinions, and downstream code (the spend query's double-count guard) made
decisions on opinions dressed up as facts. That is what made live sessions vanish
from the spend report whenever the server restarted.

## Principle 1: the event store holds only observed facts

Backfill **never writes a `session_end`**. The transcript proves the session
started, proves the tool uses happened, proves the tokens were spent — backfill
may record all of that. It does not prove the session ended, so no end is
recorded.

Consequence: when a session is rehydrated from the store, it is the **absence**
of a `session_end` event that tells us we are in category 2-or-3. The ambiguity
is no longer papered over — it simply *is* the state of our knowledge, visible in
the data.

## Principle 2: spend does not care about endings

The old spend query only counted sessions that had a `session_end` — which is why
backfill was forced to fake them. But the rule was always wrong: the tokens were
spent whether or not the session ended. Monthly spend is the sum of every
session's recorded costs, ended or not.

Once spend stops caring about endings, the 2-vs-3 distinction stops mattering for
the total: a category 3 session has its spend in the store and will never spend
again; a sleeping category 2 session has its spend in the store and is spending
nothing. Identical, and both correct. The only moment 2 differs from 3 is when it
wakes and spends *more* — and at that moment it announces itself over OTEL anyway.
The classification problem largely dissolves; what remains is making new spending
land on top of the recorded spend without double-counting.

## Principle 3: OTEL messages are events too

An OTEL cost report is an observation, just as real as a hook event, and it
belongs in the event store:

> "At 20:21, session X reported cost £0.42."

Today these live only in the server's memory and vanish on restart. Recording
them as events means live spend survives restarts for free, and the receiver
stops being a special case — it is just another source of facts.

## Principle 4: derived state lives in a projection

Next to the event store sits a **projection table** — a read model holding our
*current best understanding* of each session: its category, total spend so far,
when we last heard from it. The update cycle is classic event sourcing:

1. **Append** the incoming fact to the event store (hook event, OTEL report,
   backfilled transcript facts).
2. **Recalculate** the affected session's state by replaying all its events from
   the start.
3. **Persist** the resulting state to the projection table, overwriting what was
   there.

Spend aggregation reads the projection only. The projection is derived,
disposable, and allowed to change its mind — rewriting it is its job. There is no
discomfort about "erasing history" because history lives in the event store,
which only ever grows. The projection can be deleted and rebuilt from the events
at any time.

The existing `sessions` table is already halfway to being this projection (one
row per session: `ended_at`, `source`, `title`, …). The honest version of the
design simply admits that it is one.

## What this dissolves

- **No more guessing whether a session ended.** We never write a conclusion we
  didn't observe; the recalculation derives the best current interpretation on
  demand.
- **No special-case backfill.** Backfill appends observed transcript facts like
  any other source, then the same recalculation runs.
- **No fragile double-count guard keyed on `session_end`.** Double-counting is
  handled inside the recalculation's arithmetic (see open question).
- **Live spend survives restarts.** OTEL observations are persisted facts.
- **Resumed sessions just work.** New OTEL facts arrive for an "ended" session;
  the fold re-derives its state; the projection updates.

## The open question: what does an OTEL cost number mean? (was Q2)

The recalculation step replays a session's events and must add up its spend.
OTEL cost events make that ambiguous, because there are two things a stream of
reports like `£0.10, £0.25, £0.42` could mean:

- **Increments**: "spent £0.10", then "spent another £0.25", then "spent another
  £0.42" — total **£0.77**. Correct handling: add them up.
- **Running total**: "£0.10 so far", then "£0.25 so far", then "£0.42 so far" —
  total **£0.42**. Correct handling: take the latest, never add.

Add when you should take-latest and you overstate; take-latest when you should
add and you understate. So the recalculation cannot be written until we know
which kind Claude Code sends. (The same answer also determines how OTEL spend
combines with transcript-recorded spend without double-counting.)

This is empirically checkable: capture two consecutive reports from one session
and see whether the second is a small amount or a grown total. Worth doing
immediately — the current receiver *adds* every value, so if Claude Code sends
running totals, today's live spend figure is already overstated.

## Not yet designed

- Event schema for OTEL observations (one event per batch? per data point?
  volume is modest either way — roughly one per active session per minute).
- Projection schema and how it relates to / replaces the current `sessions` table.
- Whether the spend display still labels buckets "completed"/"active", and what
  those words now mean (projection categories, not event-store facts).
- Migration: what to do with the fake `session_end` events already written by
  past backfills (`sessions.source = 'backfill'` identifies the suspects, but the
  events themselves don't carry provenance).
- Fate of the interim mtime guard (`looks_active`, 24 h window) currently in the
  working tree — likely reduced to a noise filter or removed entirely.
