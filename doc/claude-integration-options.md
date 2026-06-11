# ctx-trakr: Session Summary
> Paste this into Claude Code as context for continuing work on ctx-trakr.

---

## Project

**ctx-trakr** (repo: `ubiqtek-ai/ctx-trakr`) — a usage tracking tool for Claude Code.
Goal: understand not just *how much* Claude is used, but *how* it is used, with a near-term deliverable of a tmux status line showing month-to-date spend against a monthly budget.

Built under the **Ubiqtek** name. TypeScript, pnpm.

---

## Decided Architecture

### Data layers (three sources, different roles)

| Source | Role | Notes |
|---|---|---|
| **OTEL** (`claude_code.cost.usage`) | Live cost across all active sessions | Emits per API request; configurable export interval (min ~10s); model attribute available |
| **SessionEnd hook → JSONL parse → SQLite** | Completed session persistence | JSONL at `~/.claude/projects/<encoded-path>/<session-id>.jsonl`; richest source for behavioural "how" data |
| **Anthropic Admin/Analytics APIs** | Billing ground truth | Not available at work (enterprise on Bedrock/no org Admin API access); use published rate card as approximation instead |

### Why OTEL for the status line
Claude Code has a built-in per-session cost display. But Jim runs a tmux multi-session workflow — multiple concurrent Claude Code sessions, each showing their own native cost. The status line needs to **aggregate across all active sessions**, which the native display can't do. OTEL solves this because every active session emits to the same OTLP endpoint.

### Hybrid design
- **OTEL** handles all *active* sessions (live running total)
- **SQLite** (populated via SessionEnd hook) handles *completed* sessions (month-to-date history)
- The two sources don't overlap: when a session ends, the SessionEnd hook triggers JSONL parse → SQLite write, and trakr stops counting that session from OTEL to avoid double-counting
- **Status line query**: `GET /spend/monthly` → sums SQLite (completed) + OTEL (active) → compares against configured monthly budget

---

## Status Line Design

```
$8.42 / $50.00
```

Simple. trakr exposes a lightweight HTTP endpoint. tmux status line script polls it.

**trakr owns:**
1. Budget config — monthly cap, per environment (work vs home). Stored in config file or trakr's SQLite.
2. Cost computation — token counts from JSONL × published API rate card per model (since no Admin API access at work).
3. OTLP receiver — accepts OTEL emissions from all active Claude Code sessions.
4. `GET /spend/monthly` endpoint — aggregates completed (SQLite) + active (OTEL) spend.

---

## Cost Computation (work/enterprise)

**Context:** Jim's work uses an enterprise seat plan. As of March 2026, Anthropic moved all enterprise to `$20/seat/month + metered API usage at standard rates` (unbundled). The old bundled token discounts are gone. So costs can be approximated using the published rate card.

**Published rate card (June 2026):**
| Model | Input /MTok | Output /MTok |
|---|---|---|
| Haiku 4.5 | $1.00 | $5.00 |
| Sonnet 4.6 | $3.00 | $15.00 |
| Opus 4.7 / 4.8 | $5.00 | $25.00 |
| Fable 5 | $10.00 | $50.00 |

- Cache read: 10% of input price (90% discount)
- Cache creation: full input price
- Batch: 50% off all rates
- Extended thinking tokens: billed at output rates
- No long-context surcharge on Sonnet 4.6 or Opus 4.7+ (dropped March 2026)

**JSONL token fields per message:** `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` — all present in the `usage` block of assistant messages.

**Caveats on approximation:**
- Extended thinking tokens may not be surfaced distinctly in OTEL (visible in JSONL)
- If org routes through Bedrock, there's a 10% regional surcharge not visible locally
- Some enterprise contracts may have negotiated volume discounts — no way to know without an invoice to compare against

---

## Anthropic Admin APIs (reference — not available at work)

Three distinct APIs, for completeness:

1. **Usage & Cost Admin API** (`/v1/organizations/usage_report/messages` + `/cost_report`) — requires `sk-ant-admin` key, org-only, not available for individual accounts or Claude Platform on AWS. Gives token breakdowns by model/workspace/key (1m/1h/1d buckets) and billed cost in cents (daily only).

2. **Claude Code Analytics API** (`/v1/organizations/usage_report/claude_code`) — same Admin key. Per-user per-day: sessions, lines added/removed, commits, PRs, tool accept/reject rates (Edit/MultiEdit/Write/NotebookEdit), model breakdown with estimated cost in cents. Daily only, ~1hr delay. API org only — Bedrock/Vertex invisible.

3. **Enterprise Analytics API** — different auth (Analytics API key, Enterprise plans only). Per-user engagement across Claude.ai, Claude Code, Cowork. Richest server-side "how" data, but gated behind Enterprise seat plans.

**For trakr:** local JSONL + OTEL is the only available layer for Jim's personal Max setup and work setup. The Admin APIs are noted for future if org access becomes available.

---

## JSONL Format Reference

Sessions stored at: `~/.claude/projects/<encoded-project-path>/<session-id>.jsonl`

Key record types:
- `assistant` records contain `message.content[]` with `tool_use` blocks and `message.usage` with token counts
- `user` records contain tool results
- Also: compaction boundaries, summary insertions, hook output, file snapshots, subagent/team coordination entries

Useful jq for token extraction:
```bash
jq -c 'select(.type=="assistant") | .message.usage' ~/.claude/projects/<path>/<session>.jsonl
```

Format evolves with Claude Code versions — version-drift is a known risk, contain it in one parser module.

---

## Repo State

`https://github.com/ubiqtek-ai/ctx-trakr` — early stage. README + LICENSE only as of this session (may have src/ added — Claude Code should check).

---

## Key Principles (from broader session)

- **Hooks are hot-path scarce resource** — don't do heavy work in hooks. SessionEnd hook = trigger only; JSONL parse happens async.
- **Estimated vs billed cost** — OTEL and JSONL give estimated cost. Only the Admin Cost API gives billed truth. trakr must label estimates clearly.
- **Double-counting guard** — once SessionEnd JSONL parse writes to SQLite, drop that session from the OTEL running total.
- **v1 pragmatism** — status line endpoint first, behavioural "how" analysis later once the plumbing is in place.
