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

## Planning

- **[planning/plan.md](planning/plan.md)** — Living implementation plan:
  phase-by-phase actions with status, session checkpoints, and architecture
  notes. Start here to see what's done and what's next.
