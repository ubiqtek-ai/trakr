# Transcript structure: what's in Claude's logs, and how it maps to our event store

Status: **findings + analysis, 2026-06-12.** Empirical companion to
[event-sourced-sessions.md](event-sourced-sessions.md) and
[session-end-detection.md](session-end-detection.md). Everything below was
measured against the real corpus at `~/.claude/projects/` on 2026-06-12
(126 JSONL files, 61 MB, 23 projects).

## Decision recorded (2026-06-12)

**OTEL and hooks are parked.** Claude's transcripts are the single source for
all spend and analysis. The OTEL receiver and hook handlers stay in the
codebase — functional, harmless — but nothing depends on them. This removes
the dual-pipeline complexity (double-count guard, delta-vs-cumulative
question, hook reliability worries) in one move. They can be revived later as
accelerators/cross-checks per session-end-detection.md §4.

---

## 1. File layout

```
~/.claude/projects/
└── -Users-jmdb-Code-github-jimbarritt-athena/        ← project dir (encoded cwd)
    ├── 73e42b5f-….jsonl                              ← main session transcript
    └── 73e42b5f-…/                                   ← per-session subdir
        └── subagents/
            ├── agent-a409bdf5b2446e4dd.jsonl         ← one file per subagent
            └── …
```

Two findings that matter immediately:

- **Subagent transcripts are separate files** in a `subagents/` subdir. They
  carry the *parent's* `sessionId` and every entry has `isSidechain: true`.
  Main session files contain **zero** sidechain entries.
- Of this corpus's 126 files, **68 are subagent files** holding **1,172 of the
  6,268 usage-bearing assistant entries (~19%)**. trakr's discovery scans
  depth 1 only — it has never seen any of them.

## 2. Entry type census

Counts across the whole corpus. Every line is a JSON object with a `type`:

| type | count | what it is |
|---|---|---|
| `assistant` | 6,281 | One *content block* of an assistant API response (see §3 — not one per response!). Carries `message.usage`, `message.model`, `message.id`, `requestId`. |
| `user` | 4,174 | User prompts **and** tool results (`toolUseResult` set on 2,284). Flags: `isMeta` (147), `isSidechain`, `isCompactSummary` (3). |
| `file-history-snapshot` | 1,260 | File state snapshots keyed by `messageId` (undo/rewind support). No session id, no timestamp. |
| `attachment` | 1,008 | Injected context: `task_reminder` (348), `hook_success` (327), `skill_listing` (121), `deferred_tools_delta` (103), `queued_command` (37)… |
| `system` | 954 | Subtypes: `turn_duration` (764), `away_summary` (98), `api_error` (76), `local_command` (11), **`compact_boundary` (3)**, `informational` (2). |
| `ai-title` | 801 | Cheap session title (already harvested into `sessions.title`). |
| `permission-mode` | 759 | Permission mode changes. |
| `last-prompt` | 752 | Last user prompt + `leafUuid` (already harvested). |
| `mode` | 647 | Mode changes. |
| `queue-operation` | 184 | Queued prompt add/remove. |

Common envelope on conversational entries: `uuid`, `parentUuid` (the entries
form a **DAG, not a list** — branching is real), `sessionId`, `timestamp`,
`cwd`, `gitBranch`, `version`, `userType`, `isSidechain`.

## 3. Finding: usage is duplicated per content block — we over-count ~2×

**One API response is written as multiple `assistant` lines** — one per
content block (`thinking`, `text`, `tool_use`…). Each line repeats the same
`message.id`, `requestId`, **and the same `message.usage` object**. Observed
up to **21 lines for a single response** (a subagent turn with many tool
calls).

- 1,888 of 6,268 usage-bearing entries (30%) are repeats of an earlier
  `(requestId, message.id)` in the same file.
- `parse_session_log` (`backfill.rs:251`) sums `message.usage` from **every**
  assistant line, with no message-id tracking (confirmed in source).

Measured impact, whole corpus:

| | output tokens | input + cache-create | cache-read |
|---|---|---|---|
| trakr today (sum every line, main files only) | 5,047,940 | 28,917,969 | 461,115,446 |
| truth (once per `message.id`, incl. subagents) | 2,164,011 | 15,191,594 | 263,115,728 |
| **overstatement** | **2.3×** | **1.9×** | **1.75×** |

The two errors partially offset (over-counting duplicates, under-counting
missing subagents) and over-counting wins by a wide margin. The ~$330
month-to-date figure is plausibly ~$150–170 in reality.

**Rule: count `message.usage` once per `message.id`.** (Within a message the
repeated usage objects are identical, so "first occurrence wins" is enough.)

### Cross-file duplication (the resume/branch worry): not observed

Zero `(requestId, message.id)` pairs appear in more than one file in this
corpus — `--resume`/branching does **not** appear to replay usage-bearing
entries into new files. A global (not per-file) seen-set is still cheap
insurance, but it is not the urgent bug. The urgent bug is §3.

### Also wrong today: one model per session

`parse_session_log` takes the model from the *first* assistant entry and
prices the whole session with it. `message.model` is per-entry, and sessions
mix models (subagents, background Haiku calls). Pricing should be per-message:
group usage by `message.model`, or emit one `TokenUsage` per (model) group.

## 4. Transcript ⇄ our typed events

### The three layers

Getting the vocabulary right matters here. A *projection* is what you get by
folding events into **state** ("this session has spent £4.20"). Turning
Claude's raw lines into our `Event` enum is not that — it is event-to-event:
a **transformation**. So the architecture has three layers:

```
raw transcripts        Claude's native JSONL, copied verbatim.
(per agent, native)    The only irreplaceable layer — everything below
        │              is re-derivable from it.
        │  transform   (one ADAPTER per agent: the Claude adapter is
        ▼               today's parser; a future agent = a new adapter)
typed events           OUR domain language: SessionStarted, TokensSpent,
        │              ToolUsed, SubagentSpawned, ContextCompressed…
        │  fold        Agent-agnostic: folds and projections never know
        ▼               which agent produced the events.
projections            Derived state: per-session totals, monthly spend,
                       "last heard from". Disposable, rebuildable.
```

The coupling to Claude's format doesn't disappear — it gets **quarantined
into the adapter**. Claude's particular shapes (`attachment`,
`file-history-snapshot`, usage-repeated-per-content-block, `isMeta`) never
leak past it. The design discipline for the typed-event vocabulary: *"is this
a concept any agent would have, or is this Claude leaking through?"* —
`ContextCompression` is universal; `isMeta` is Claude.

This is the existing "contain the format in one parser module" principle
(claude-integration-options.md) promoted to an architectural rule, and it is
how multi-agent tools like AgentsView support 20+ agents.

### The Claude adapter's mapping

Typed events are still worth materialising — "what did my sessions do?"
analysis wants typed events, not raw JSONL. What the Claude adapter can
derive:

| Our `Event` | Transcript source | Status |
|---|---|---|
| `SessionStart` | First entry's `timestamp`; `cwd`, `gitBranch`, `version` from envelope. No explicit start record exists. | ✓ derivable (hook's `source` field is the only loss) |
| `SessionEnd` | **Nothing.** No end marker exists in the format. | per event-sourced-sessions.md: never fabricate it |
| `ToolUse` | `assistant` `tool_use` content blocks (name, input, id) joined to the `user` entry whose `toolUseResult` answers them; `system/turn_duration` gives timing. | ✓ derivable, richer than the hook version |
| `TokenUsage` | `assistant` `message.usage`, **deduped by `message.id`**, grouped by `message.model` | ✓ derivable once §3 is fixed |
| `SubagentStart`/`SubagentStop` | Existence + first/last timestamps of `subagents/agent-*.jsonl`; the spawning `Task` tool_use in the main file | ✓ derivable (today these only came from hooks) |
| `ContextCompression` | `system/compact_boundary` + `isCompactSummary` user entry | ✓ derivable |
| `Other` | — | retire or use for `api_error` etc. |

Things the transcript has that our events don't yet capture (candidate new
event types or fields): `api_error` (76 occurrences — failed/retried calls),
`turn_duration` (per-turn latency), `away_summary`, permission/mode changes,
queued prompts, thinking blocks, the `parentUuid` DAG (branch detection),
per-entry `gitBranch`.

## 5. What this means for the archive

Of the three layers, only the raw one is irreplaceable: typed events and
projections can always be rebuilt from archived transcripts, but Claude's
own logs are Claude's — it can prune or reformat them at will. The verbatim
copy is therefore the true asset, and the backup bar is *completeness*:
`archive_transcript` copies only the main JSONL today — **it must also copy
the session's `subagents/` directory**, or the one layer we cannot rebuild
silently loses ~19% of the record.

## 6. Archive strategy: two loops, fully decoupled

Sampling and archiving serve different masters and want different rhythms:

| | Sampling | Archiving |
|---|---|---|
| Serves | spend freshness (status line) | durability (the irreplaceable raw layer) |
| Deadline | seconds | "before Claude prunes the file" — weeks |
| Touches | **reads in place**, never copies | **copies**, never parses |
| Cadence | ~30 s poll of sizes/mtimes | daily incremental sweep |

The sampling loop reads transcripts where they already live — Claude's
directory is the working source; no copy is needed for spend.

The archive loop is an rsync-style incremental sweep: walk
`~/.claude/projects/`, compare each file's size + mtime against the archived
copy, copy only what differs, mirroring the directory structure (which picks
up `subagents/` for free). At 61 MB corpus size, a nothing-changed sweep
costs essentially nothing and a typical day copies a handful of files.

Why this is safe and sufficient:

- **Append-only files make mid-write copies harmless.** Catching a session
  mid-write just means the archive copy is a few lines short until the next
  sweep tops it up — copies only ever grow towards the truth.
- **No "session ended" concept needed.** We don't archive *when sessions
  end*; we archive *whatever changed since yesterday*. Early and late are
  equally harmless, so the idle-vs-ended ambiguity that poisons end
  detection costs nothing here.
- **Daily cadence vs a weeks-long pruning deadline** leaves enormous margin;
  the loop frequency could be wrong by an order of magnitude in either
  direction and still lose nothing.

Neither loop knows the other exists.

## 7. Action list distilled

1. **Dedupe usage by `message.id`** in `parse_session_log` — fixes the ~2×
   overstatement. Highest priority; wrong money today.
2. **Scan `subagents/*.jsonl`** and attribute to the parent session (same
   `sessionId`, so attribution is trivial).
3. **Price per `message.model`**, not first-model-wins.
4. **Archive `subagents/` alongside the main transcript.**
5. Park OTEL + hooks (decision above): remove them from the spend path,
   leave the code.
6. Later, with the projection design: extend typed events with the §4
   candidates (api_error, turn_duration, branching…).
