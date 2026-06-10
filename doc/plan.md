# Implementation Plan

## Phase 1: Project Foundation

### Action 1.1: Initialize Rust project
- ✓ DONE - Create Cargo.toml with base dependencies (serde, serde_json, chrono, rusqlite)
- ✓ DONE - Set up project structure: src/main.rs, src/lib.rs, src/hooks.rs, src/event.rs, src/storage.rs, src/transcript.rs
- ✓ DONE - Configure for crates.io publishing (metadata, license, docs)

### Action 1.2: Hook interface design
- ✓ DONE - Define data structures for captured events (ToolUse, SessionStart, SessionEnd, SubagentStart, SubagentStop, ContextCompression, TokenUsage, Other)
- ✓ DONE - Create JSON schema for hook payloads (implicit via serde)
- ✓ DONE - Design session storage format (SQLite unified DB + JSONL backups)

### Action 1.3: Core types & serialization
- ✓ DONE - Implement Event enum with all variants in src/event.rs
- ✓ DONE - Add serde derives for JSON interchange
- ✓ DONE - Add unit tests for event serialization/deserialization

## Phase 2: Hook Integration

### Action 2.1: Hook listener
- ✓ DONE - Build hook command wrapper (src/main.rs handles `hook` subcommand)
- ✓ DONE - Parse JSON from Claude Code hooks (src/hooks.rs)
- ✓ DONE - Append events to session storage (src/storage.rs with dual SQLite + JSONL)
- ✓ DONE - Handle transcript parsing for token usage (src/transcript.rs)

### Action 2.2: Session management
- ✓ DONE - Implement session initialization and directory structure (cmd_init)
- ✓ DONE - Track sessions by ID (string-based session identifiers)
- ✓ DONE - Store per-session event log with metadata in unified SQLite DB
- ✓ DONE - Create JSONL backup files for each session
- ✓ DONE - Implement migration from JSONL to unified DB (cmd_migrate)

### Action 2.3: Hook documentation
- PARTIAL - Suggested hook config printed by `ctx-trakr init` command
- ✓ DONE - Hook types documented in code comments (tool-use, session-start, session-end)
- ✓ DONE - Installation instructions in README.md
- TODO - Detailed hook setup guide (in-code output sufficient for MVP)

## Phase 3: Querying & Analysis

### Action 3.1: Query CLI
- ✓ DONE - Implement `ctx-trakr list` command (lists all sessions with event counts)
- PARTIAL - Implement `ctx-trakr show <session>` (data exists in DB, CLI not exposed)
- TODO - Implement `ctx-trakr stats` command
- TODO - Add filtering by tool, model, date range
- TODO - Output as JSON or human-readable text

### Action 3.2: Statistics engine
- TODO - Calculate tool frequency, model distribution
- TODO - Aggregate context window estimates
- TODO - Subagent spawn patterns

### Action 3.3: Export/reporting
- TODO - JSON export for analysis pipelines (raw DB query possible but no dedicated CLI)
- TODO - Summary report generation
- TODO - Session timeline visualization (text-based)

## Phase 4: Polish & Release

### Action 4.1: Testing
- ✓ DONE - Unit tests for event parsing (src/event.rs has 5 tests)
- ✓ DONE - Hook handler tests (src/hooks.rs has 11 tests)
- ✓ DONE - Storage integration tests (src/storage.rs has 7 tests)
- ✓ DONE - Transcript parsing tests (src/transcript.rs has 7 tests)
- TODO - CLI command integration tests

### Action 4.2: Documentation
- PARTIAL - API docs (cargo doc should work)
- ✓ DONE - README with quick start and basic usage examples
- TODO - Troubleshooting guide
- TODO - Detailed examples for each command

### Action 4.3: Crates.io publication
- TODO - Final dependency audit
- TODO - Version 0.1.0 release
- TODO - GitHub Actions CI/CD setup

## Implementation Notes

### Completed Features
- Unified SQLite database (ctx-trakr.db) for persistent storage
- Dual storage: events stored in both SQLite and JSONL backup files
- Idempotent migration from JSONL to unified DB
- Hook integration for tool-use, session-start, session-end
- Transcript parsing to extract token usage metrics (input, output, cache)
- Event type detection and labeling
- Graceful error handling (hooks always exit 0 to not block Claude Code)
- Comprehensive test coverage (30+ tests)

### Remaining Work
- Advanced CLI commands (show, stats, with filtering)
- Statistics aggregation and reporting
- Timeline visualization
- Additional hook types (currently limited to tool-use, session-start/end)
- CI/CD setup
- Crates.io publication workflow

## Delta Summary
- **Binaries added**: ctx-trakr CLI tool
- **Libraries added**: ctx-trakr as a dependency
- **Data added**: ~/.ctx-trakr/ with unified DB and sessions directory
- **Configuration added**: Hook suggestions via `ctx-trakr init`
