# ctx-trakr documentation index

## Project definition

- **[requirements.md](requirements.md)** — What ctx-trakr is for: hook
  integration, telemetry collection, context tracking, data organization,
  reporting, and technical requirements.

## Claude Code integration reference

- **[claude-hooks.md](claude-hooks.md)** — Hook event reference
  (`SessionStart` / `SessionEnd` / `PreToolUse`): when each fires, payload
  fields, correct `settings.json` config, and what trakr captures from each.
- **[claude-session-logs.md](claude-session-logs.md)** — Format of Claude
  Code's native session JSONL at `~/.claude/projects/`: record types, token
  usage fields, titles/summaries, and how trakr's backfill parses them.
- **[claude-integration-options.md](claude-integration-options.md)** —
  Decided architecture and its rationale: the three data layers (OTEL live,
  SessionEnd→SQLite, Admin APIs), the tmux status-line design, rate-card
  cost computation, and key principles (hooks are hot-path, estimates vs
  billed truth, double-counting guard).

## Research

- **[agentsview-comparison.md](agentsview-comparison.md)** — Analysis of
  AgentsView (agentsview.io / kenn-io/agentsview): architecture, feature
  set, and head-to-head comparison with trakr; takeaways (file watching,
  pricing as data, FTS5) and non-goals.
- **[session-end-detection.md](session-end-detection.md)** — Deep dive on
  hooks vs file watching for detecting session end, and why the live-cost
  double-counting problem dissolves under a single-ledger,
  watcher-driven design. Recommends the architecture change and lists open
  risks to verify (message-id dedup on resume, sidechain usage).
- **[session-lifecycle.md](session-lifecycle.md)** — The three-category
  session model (known-complete / active / ended-unhooked) and the 2026-06-11
  reconciliation bug that motivated it. Superseded by the event-sourced
  design but the model still stands.
- **[event-sourced-sessions.md](event-sourced-sessions.md)** — Design
  principles: the event store holds only observed facts (backfill never
  writes a `session_end`), spend doesn't care about endings, derived state
  lives in a rebuildable projection.
- **[transcript-structure.md](transcript-structure.md)** — Empirical analysis
  of the native transcript format (entry census, subagent files, usage
  duplicated per content block → ~2× spend overstatement), and the
  three-layer architecture: raw transcripts → per-agent adapter →
  agent-agnostic typed events → projections. Records the 2026-06-12 decision
  to park OTEL and hooks.

## Planning

- **[planning/plan.md](planning/plan.md)** — Living implementation plan:
  phase-by-phase actions with status, session checkpoints, and architecture
  notes. Start here to see what's done and what's next.
- **[planning/single-ledger-plan.md](planning/single-ledger-plan.md)** —
  Self-contained execution plan for the single-ledger redesign (parser
  dedupe, subagent inclusion, spend without endings, sampling loop, archive
  sweep), written to hand to an implementation agent.
