# Requirements

## Overview
A Rust tool that integrates with Claude Code hooks to track and analyze context usage patterns, telemetry, and agent activity.

## Core Functionality

### Hook Integration
- Attach to Claude Code hooks for relevant events
- Capture tool invocations, model switches, subagent spawns
- Record context window usage and state transitions

### Telemetry Collection
- Track which tools are used and how frequently
- Record model selections and reasoning mode toggles
- Monitor subagent creation and messaging patterns
- Capture error rates and permission denials

### Context Tracking
- Timeline of context-affecting events within a session
- Context compression events and delta tracking
- Message volume and token consumption estimates
- Plan mode entries/exits

### Data Organization
- Session-level aggregations
- Per-project statistics
- Tool usage patterns and frequency
- Model/reasoning mode distribution

### Output & Reporting
- CLI queries for session data
- JSON export for analysis
- Summary statistics for recent activity
- Optional local database for historical data

## Technical Requirements
- Written in Rust
- Publishable to crates.io
- Integrates with Claude Code settings.json hooks
- Minimal performance overhead
- Cross-platform (Darwin, Linux)
