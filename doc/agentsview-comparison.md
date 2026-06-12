# AgentsView vs ctx-trakr — research & comparison

*Research date: 2026-06-12*

## Sources

- Product site: https://agentsview.io/ (docs at https://www.agentsview.io/ — quickstart, usage, CLI reference, architecture)
- GitHub: https://github.com/kenn-io/agentsview
- Repo agent guide: https://github.com/kenn-io/agentsview/blob/main/AGENTS.md
- Releases: https://github.com/kenn-io/agentsview/releases

Note: the agentsview.io docs site blocks automated fetches (HTTP 403), so the
analysis below is based on the GitHub repo (README, AGENTS.md, directory
structure, release notes) and search results.

## What AgentsView is

AgentsView bills itself as "local-first session intelligence and analytics for
coding agents" and a "100x faster replacement for ccusage". It indexes the
native session logs of **20+ coding agents** (Claude Code, Codex, Copilot CLI,
Cursor, OpenHands, Gemini CLI, Qwen Code, Kiro, Forge, Warp, Antigravity, …)
into a local SQLite database and serves a web UI on top.

Headline features:

- **Session browser** — full conversation viewer (prompts, replies, tool calls
  with inputs/results), FTS5 full-text search, keyboard-first navigation,
  export to HTML or GitHub Gist
- **Cost & token tracking** — per-model breakdowns including prompt-cache
  creation/read tokens, daily spend charts, `usage daily` / `session usage`
  CLI commands
- **Analytics** — activity heatmaps, tool-usage metrics, velocity, project
  breakdowns, session archetypes (`agentsview stats`)
- **Live updates** — file watcher + server-sent events push new messages from
  active sessions into the UI in real time
- **Team/remote options** — optional PostgreSQL push for read-only team
  dashboards; DuckDB mirror for portable analytics or remote access
- **Distribution** — single Go binary (shell installer, Homebrew, Docker) plus
  a fully bundled Tauri desktop app (macOS/Windows) with auto-update

Project health (as of June 2026): ~1.8k stars, 180 forks, multiple releases per
month (v0.32.1 on Jun 5, 2026), sustained feature velocity (recent additions:
Antigravity session parsing, secret scanning, full-session content search,
Opus 4.8 fallback pricing).

## AgentsView architecture

Tech stack: **Go 1.26+** backend (CGO for SQLite with the `fts5` build tag),
**Svelte 5 + TypeScript** SPA embedded in the binary, **Tauri** desktop
wrapper. Language split is ~75% Go, ~22% TS/Svelte.

Pipeline (from AGENTS.md):

```
agent session dirs ──▶ file watcher ──▶ per-agent parsers ──▶ SQLite (WAL + FTS5)
 (~/.claude/projects/ etc.)   (internal/sessionwatch)   (internal/parser)      (internal/db)
                                                                                  │
                                              optional push ◀─────────────────────┤
                                       PostgreSQL / DuckDB                        ▼
                                                                    HTTP server :8080 (REST + SSE)
                                                                    embedded Svelte SPA
```

Key internal packages: `parser` (registry of per-agent session-file parsers),
`db` (SQLite CRUD, migrations, FTS5), `sessionwatch`/`sync` (file watching and
orchestration, periodic 15-min sync), `server` (REST API, search, SSE),
`pricing` (LiteLLM model-pricing catalog + normalization + hardcoded
fallbacks), `postgres`/`duckdb` (optional mirrors), `insight` (analytics),
`telemetry` (anonymized daemon ping, disabled by default).

Design points worth noting:

1. **No hooks, no instrumentation.** AgentsView never touches the agent's
   config. It discovers and tails each agent's *native* session logs on disk.
   This is what makes 20+ agent support tractable — adding an agent means
   adding a parser, not an integration.
2. **Index once, query forever.** The "100x faster than ccusage" claim comes
   from incremental indexing into SQLite rather than re-parsing raw JSONL on
   every query — the same insight that motivated trakr's unified SQLite DB.
3. **Pricing as data, not code.** Costs use LiteLLM's community-maintained
   model-pricing catalog (`internal/pricing/litellm.go`) with normalization
   and fallback entries for brand-new models. Cache creation/read tokens are
   priced separately, like trakr.
4. **Backend parity contract.** SQLite and PostgreSQL implementations sit
   behind a shared contract (`internal/backendcontract`, `backendbench`),
   keeping the team-dashboard path honest.

## What trakr is (for contrast)

ctx-trakr is a single-purpose Rust CLI (~3.8k LOC, 11 modules, zero frontend)
focused on one question: *"what is my month-to-date Claude Code spend across
all my tmux sessions, and am I inside budget?"*

- **Ingestion**: a `SessionEnd` hook registered in `~/.claude/settings.json`
  parses the session's native JSONL transcript and writes one atomic,
  idempotent record per session (`backfill::parse_session_log` →
  `storage::replace_session`). A reconciliation sweep on `trakr serve` startup
  backfills sessions whose hook was missed. `backfill-logs` does bulk
  historical import from `~/.claude/projects/`.
- **Live sessions**: an embedded OTLP HTTP/JSON receiver (port 4318) accepts
  `claude_code.cost.usage` metrics from active Claude Code sessions, with a
  double-count guard against completed sessions already in SQLite.
- **Storage**: single SQLite DB (WAL, busy_timeout) with `events` and
  `sessions` tables, plus JSONL backups per session.
- **Cost**: hardcoded Anthropic rate card in `src/cost.rs` (Haiku/Sonnet/
  Opus/Fable, June 2026), cache read at 10% of input rate, cache creation at
  full input rate.
- **Output**: `spend`, `list`, `show`, `stats` CLI commands and a
  `GET /spend/monthly` HTTP endpoint designed for tmux status-line polling
  against a configured monthly budget.

## Head-to-head

| Dimension | AgentsView | ctx-trakr |
|---|---|---|
| Core purpose | Session browser + analytics platform for all coding agents | Month-to-date Claude Code spend vs budget |
| Agents supported | 20+ (parser registry) | Claude Code only |
| Ingestion | File watcher over native session dirs; no agent config changes | `SessionEnd` hook + native JSONL parse; OTEL for live; backfill for history |
| Live-session data | File watching + SSE to the UI | OTLP receiver consuming Claude Code's OTEL metrics |
| Storage | SQLite (WAL + FTS5); optional PostgreSQL and DuckDB mirrors | SQLite (WAL) + JSONL backups |
| Search | FTS5 full-text over session content | None (Phase 4d plans title/summary extraction) |
| Pricing | LiteLLM catalog, normalized, with fallbacks; cache tokens priced | Hardcoded Anthropic rate card; cache tokens priced |
| Budgeting | Daily spend charts, no budget concept found | Explicit `monthly_budget_usd`, spend-vs-budget endpoint |
| UI | Embedded Svelte SPA, Tauri desktop app | CLI + JSON HTTP endpoint (tmux status line) |
| Team features | PostgreSQL read-only team dashboards, Gist publishing | None (single user) |
| Stack | Go + Svelte + Tauri, CGO SQLite | Rust, rusqlite (bundled), axum, ~10 deps |
| Distribution | Shell installer, Homebrew, Docker, desktop app, auto-update | `cargo install` (crates.io publication pending) |
| Maturity | v0.32.x, 1.8k stars, releases ~weekly | v0.1.0, pre-release |

## Where the designs converge

Both projects independently arrived at the same load-bearing conclusions:

1. **The agent's native session log is ground truth.** trakr learned this the
   hard way in Phase 4c — per-hook token capture recorded last-turn-only
   tokens ($0.24 vs the true $281.70) until `handle_session_end` was rewritten
   to parse the full transcript. AgentsView's entire architecture is built on
   this premise from day one.
2. **Index into SQLite, don't re-parse.** AgentsView's headline performance
   claim vs ccusage is exactly the unified-DB design trakr already has.
3. **Cache tokens matter for cost accuracy.** Both price cache creation and
   cache read separately rather than lumping them into input tokens.
4. **WAL mode + non-destructive migrations** for a DB written by background
   processes.

## Where they genuinely differ

**Hooks vs file watching.** This is the biggest architectural divergence.
trakr's hook approach needs a `settings.json` edit, can miss events (hence the
reconciliation sweep), and is Claude-Code-specific. AgentsView's watcher needs
no setup, can't miss a completed session, and generalizes to any agent — but
it must run as a daemon to be live, and it can't see things that never reach
the transcript. trakr's hook path is effectively converging on the watcher
model already: the `SessionEnd` handler is now just a trigger to parse the
native log, and the reconciliation sweep is a poll-based watcher in all but
name.

**OTEL for live spend.** trakr's OTLP receiver is something AgentsView doesn't
have — it gets in-flight cost from Claude Code's own metrics rather than
inferring from partially written transcripts. This is trakr's most distinctive
piece and directly serves the tmux status-line use case.

**Budget as a first-class concept.** AgentsView shows you spend; trakr answers
"am I over budget this month" in a single endpoint. Nothing in AgentsView's
README or release notes mentions budgets or alerts.

**Scope and surface.** AgentsView is a product (UI, desktop app, team
dashboards, telemetry, installers). trakr is a focused tool. Competing with
AgentsView on session browsing would mean building a frontend, a parser
registry, and a release pipeline — a different project.

## Takeaways for trakr

Things worth adopting, roughly in order of value-for-effort:

1. **Replace the hook trigger with (or back it up by) a file watcher.**
   `trakr serve` already runs a reconciliation sweep at startup; running it on
   an interval (AgentsView syncs every 15 min) or via inotify/FSEvents on
   `~/.claude/projects/` would eliminate the missed-`SessionEnd` failure mode
   entirely and make the hooks optional rather than required. This removes
   trakr's most fragile dependency (users editing `settings.json`).
2. **Pricing as data.** The hardcoded rate card in `cost.rs` will rot with
   every model release (AgentsView shipped an Opus 4.8 fallback-pricing patch
   within days of that model's launch). Fetching LiteLLM's
   `model_prices_and_context_window.json` with the current table kept as the
   offline fallback would keep estimates honest with near-zero maintenance.
3. **FTS5 is cheap and already in reach.** rusqlite's bundled SQLite supports
   FTS5; once Phase 4d lands transcript archiving plus `title`/`summary`/
   `last_prompt` columns, a contentless FTS index over those fields would give
   `trakr list --search <term>` for very little code.
4. **Validate the "100x" framing.** trakr already has the indexed-SQLite
   design; if/when it's published, "no re-parsing on query" is a selling point
   worth stating, as AgentsView demonstrates.

Things to deliberately *not* chase:

- **20+ agent support, web UI, desktop app, team Postgres** — these define
  AgentsView's product scope. trakr's value is the opposite: a tiny Rust
  binary with a budget number in your tmux status line. If full session
  browsing is ever wanted, running AgentsView *alongside* trakr is cheaper
  than rebuilding it.
- **DuckDB/Postgres mirrors** — single-user SQLite is sufficient for the
  stated requirements (doc/requirements.md).

The honest summary: AgentsView is a mature, well-executed product occupying
the "session intelligence platform" space. trakr should not compete head-on;
its defensible niche is the budget-aware, OTEL-live, zero-UI spend tracker —
and it should steal AgentsView's two best ideas (watch the filesystem, treat
pricing as data) to harden that niche.
