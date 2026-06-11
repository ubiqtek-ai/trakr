# Claude Code Hooks Reference

## Session lifecycle hooks

Claude Code exposes three hook event types relevant to session lifecycle:

| Event | When it fires | Notes |
|-------|--------------|-------|
| `SessionStart` | Once when Claude Code begins a session | Covers new sessions, resumed sessions (`claude --resume`), and after `/clear`. The `source` field distinguishes them: `startup`, `resume`, or `clear`. |
| `SessionEnd` | Once when Claude Code exits | Fires on `/exit`, terminal close, or normal process termination. Does **not** fire after every assistant turn. |
| `PreToolUse` | Before each tool execution | Can be filtered by tool name via a matcher string. |

There is no separate hook for resume — `SessionStart` covers all entry points.

## Payload fields

### SessionStart

```json
{
  "session_id": "67545be0-...",
  "source": "startup"
}
```

`source` values: `startup` (new session), `resume` (resumed via `--resume`), `clear` (after `/clear` command).

### SessionEnd

```json
{
  "session_id": "67545be0-..."
}
```

### PreToolUse

```json
{
  "session_id": "67545be0-...",
  "tool_name": "Bash",
  "tool_input": { ... }
}
```

## Hook configuration format (`~/.claude/settings.json`)

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "ctx-trakr hook session-start", "timeout": 5 }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "ctx-trakr hook session-end", "timeout": 5 }] }
    ],
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [{ "type": "command", "command": "ctx-trakr hook tool-use", "timeout": 5 }]
      }
    ]
  }
}
```

## ctx-trakr's current hook config

As of 2026-06-10, `~/.claude/settings.json` has:

| Event | Matcher | Command |
|-------|---------|---------|
| `SessionStart` | — | `ctx-trakr hook session-start` |
| `SessionEnd` | — | `ctx-trakr hook session-end` |
| `PreToolUse` | `*` | `ctx-trakr hook tool-use` |
| `PreToolUse` | `Bash` | `rtk hook claude` |
| `PreToolUse` | `WebFetch` | echo allow JSON |

## What ctx-trakr captures

- `SessionStart` → stored as a `session_start` event with `source` field (`startup` / `resume` / `clear`)
- `SessionEnd` → stored as a `session_end` event; marks a session as fully tracked
- `PreToolUse` → stored as a `tool_use` event with tool name and (where available) duration

A session is considered **complete** when it has both `session_start` and `session_end` in the DB. Sessions with only a `session_start` are **partial** — the session either ended abnormally or the hook did not fire (e.g., process killed). The `backfill-logs` command can fill in partial or missing sessions from Claude's native session logs.

## Recommended `ctx-trakr init` config

The `init` command should suggest all three lifecycle hooks:

```json
"SessionStart": [
  { "hooks": [{ "type": "command", "command": "ctx-trakr hook session-start", "timeout": 5 }] }
],
"SessionEnd": [
  { "hooks": [{ "type": "command", "command": "ctx-trakr hook session-end", "timeout": 5 }] }
],
"PreToolUse": [
  {
    "matcher": "*",
    "hooks": [{ "type": "command", "command": "ctx-trakr hook tool-use", "timeout": 5 }]
  }
]
```

Note: earlier versions of `ctx-trakr init` suggested `PostToolUse` and `Stop` — these are **incorrect** event names. The correct names are `PreToolUse` and `SessionEnd`.
