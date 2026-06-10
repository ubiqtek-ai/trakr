#!/usr/bin/env just --justfile

# Build the project
build:
    cargo build

# Run tests
test:
    cargo test

# Install locally (into ~/.cargo/bin)
install-cli: build
    cargo install --path .

# Clean up test session data
clean-data:
    rm -rf ~/.ctx-trakr/sessions/
    echo "✓ Cleared test data"

# Test tool-use hook with sample bash event
demo-tool-bash:
    echo '{"session_id": "demo-001", "tool_name": "bash", "status": "success", "duration_ms": 150}' \
        | ./target/debug/ctx-trakr hook tool-use
    echo "✓ Logged bash tool execution"

# Test tool-use hook with read event
demo-tool-read:
    echo '{"session_id": "demo-001", "tool_name": "read", "status": "success", "duration_ms": 50}' \
        | ./target/debug/ctx-trakr hook tool-use
    echo "✓ Logged read tool execution"

# Test session start hook
demo-session-start:
    echo '{"session_id": "demo-001", "model": "claude-opus-4-7", "source": "startup"}' \
        | ./target/debug/ctx-trakr hook session-start
    echo "✓ Logged session start"

# Test session end hook
demo-session-end:
    echo '{"session_id": "demo-001"}' \
        | ./target/debug/ctx-trakr hook session-end
    echo "✓ Logged session end"

# Run a full demo session with multiple events
demo: build clean-data
    @echo "▶ Starting demo session..."
    @just demo-session-start
    @sleep 0.1
    @just demo-tool-bash
    @sleep 0.1
    @just demo-tool-read
    @sleep 0.1
    @echo '{"session_id": "demo-001", "tool_name": "write", "status": "success", "duration_ms": 75}' \
        | ./target/debug/ctx-trakr hook tool-use && echo "✓ Logged write tool execution"
    @sleep 0.1
    @just demo-session-end
    @echo ""
    @echo "✓ Demo complete! Session data:"
    @echo "  SQLite:  ~/.ctx-trakr/ctx-trakr.db  (unified DB)"
    @echo "  JSONL:   ~/.ctx-trakr/sessions/demo-001.jsonl"
    @echo ""
    @echo "View events:"
    @echo "  sqlite3 ~/.ctx-trakr/ctx-trakr.db 'SELECT timestamp, event_type, session_id FROM events;'"
    @echo "  cat ~/.ctx-trakr/sessions/demo-001.jsonl | jq ."

# Migrate existing per-session JSONL files into the unified DB
migrate: build
    ./target/debug/ctx-trakr migrate

# View unified DB events
view-db:
    @echo "SQLite events table (unified DB):"
    sqlite3 ~/.ctx-trakr/ctx-trakr.db "SELECT timestamp, event_type, session_id FROM events LIMIT 10;" || echo "No data yet - run 'just demo' first"

# View demo session JSONL data
view-jsonl:
    @echo "JSONL backup:"
    @cat ~/.ctx-trakr/sessions/demo-001.jsonl | jq . 2>/dev/null || echo "No data yet - run 'just demo' first"

# Development: watch for changes and run tests
watch:
    cargo watch -x test

# Set up Claude Code hooks in ~/.claude/settings.json
setup-hooks:
    #!/usr/bin/env bash
    set -euo pipefail

    settings_file="$HOME/.claude/settings.json"

    # Create settings file if it doesn't exist
    if [ ! -f "$settings_file" ]; then
        mkdir -p "$(dirname "$settings_file")"
        echo '{"hooks": {}}' > "$settings_file"
        echo "✓ Created $settings_file"
    fi

    # Add hooks using jq
    jq '.hooks += {
        "PreToolUse": [{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": "ctx-trakr hook tool-use",
                "timeout": 5
            }]
        }],
        "SessionStart": [{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": "ctx-trakr hook session-start",
                "timeout": 5
            }]
        }],
        "SessionEnd": [{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": "ctx-trakr hook session-end",
                "timeout": 5
            }]
        }]
    }' "$settings_file" > "$settings_file.tmp" && mv "$settings_file.tmp" "$settings_file"

    echo "✓ Registered 3 hooks in $settings_file"
    echo "  - PreToolUse (tool-use)"
    echo "  - SessionStart (session-start)"
    echo "  - SessionEnd (session-end)"

# Remove Claude Code hooks from settings.json
uninstall-hooks:
    #!/usr/bin/env bash
    set -euo pipefail

    settings_file="$HOME/.claude/settings.json"

    if [ ! -f "$settings_file" ]; then
        echo "✗ Settings file not found: $settings_file"
        exit 1
    fi

    jq 'del(.hooks.PreToolUse, .hooks.SessionStart, .hooks.SessionEnd)' "$settings_file" > "$settings_file.tmp" && mv "$settings_file.tmp" "$settings_file"

    echo "✓ Removed ctx-trakr hooks from $settings_file"

# Install locally and set up hooks (complete setup)
setup: install-cli setup-hooks
    @echo ""
    @echo "✅ Setup complete!"
    @echo ""
    @echo "ctx-trakr is now:"
    @echo "  • Installed: ~/.cargo/bin/ctx-trakr"
    @echo "  • Connected: Hooks registered in ~/.claude/settings.json"
    @echo ""
    @echo "Your Claude Code sessions will now be tracked."
    @echo "View captured data:"
    @echo "  • SQLite: ~/.ctx-trakr/ctx-trakr.db  (unified DB)"
    @echo "  • JSONL: ~/.ctx-trakr/sessions/{session-id}.jsonl"

# Show all available recipes
help:
    @just --list
