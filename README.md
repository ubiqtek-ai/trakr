# ctx-trakr

A Rust tool that attaches to Claude Code hooks to track context usage, telemetry, and agent activity. Monitor which tools you're using, which models are active, subagent spawns, and context window dynamics in real time.

## Features

- **Hook Integration**: Seamlessly integrates with Claude Code's settings.json hooks
- **Tool Tracking**: Records all tool invocations with timestamps and outcomes
- **Model Monitoring**: Tracks model switches, reasoning mode toggles, and usage patterns
- **Agent Telemetry**: Captures subagent creation, messaging, and performance
- **Context Insights**: Understand context compression events and token consumption
- **Query Interface**: CLI for exploring sessions and analyzing patterns
- **JSON Export**: Export session data for custom analysis pipelines

## Installation

### From crates.io (when published)
```bash
cargo install ctx-trakr
```

### From source
```bash
git clone https://github.com/ubiqtek/ctx-trakr.git
cd ctx-trakr
cargo install --path .
```

## Quick Start

### 1. Initialize ctx-trakr
```bash
ctx-trakr init
```
This sets up `~/.ctx-trakr/` and creates hook entries in your Claude Code settings.

### 2. Integrate with Claude Code
Add the following to your `~/.claude/settings.json` (or let `ctx-trakr init` do it):
```json
{
  "hooks": {
    "onToolUse": "ctx-trakr hook tool",
    "onModelSwitch": "ctx-trakr hook model",
    "onAgentSpawn": "ctx-trakr hook agent"
  }
}
```

### 3. Start tracking
Use Claude Code normally — ctx-trakr captures everything in the background.

## Usage

```bash
# List recent sessions
ctx-trakr list

# Show details of a session
ctx-trakr show <session-id>

# Get statistics for a session
ctx-trakr stats <session-id>

# Filter by tool
ctx-trakr list --tool bash --tool grep

# Export as JSON
ctx-trakr show <session-id> --json > session.json

# View context timeline
ctx-trakr timeline <session-id>
```

## Data Storage

Session data is stored in `~/.ctx-trakr/sessions/` as JSON files with this structure:
```
~/.ctx-trakr/
├── sessions/
│   ├── 2026-04-19T14-23-45.json
│   └── 2026-04-19T15-10-12.json
└── config.json
```

Each session file contains a complete log of events with timestamps, types, and metadata.

## Development

### Building
```bash
cargo build
```

### Testing
```bash
cargo test
```

### Generating docs
```bash
cargo doc --open
```

## License

MIT