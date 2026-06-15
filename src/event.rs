use serde::{Deserialize, Serialize};

/// All event types that ctx-trakr can record from Claude Code hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ToolUse {
        tool_name: String,
        status: String,
        duration_ms: Option<u64>,
        error: Option<String>,
    },
    SessionStart {
        model: String,
        source: String,
    },
    SessionEnd,
    SubagentStart {
        name: String,
        agent_type: String,
    },
    SubagentStop {
        name: String,
    },
    ContextCompression {
        before_tokens: u64,
        after_tokens: u64,
    },
    TokenUsage {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
        /// Subset of `cache_creation_input_tokens` written at the 1-hour TTL tier (billed at 2× input rate).
        /// Zero for old DB records that pre-date this field; those are priced at the 5-min rate as before.
        #[serde(default)]
        cache_creation_1h_input_tokens: u64,
        total_tokens: u64,
    },
    /// A background API call made by Claude Code that does not appear in session transcripts.
    ///
    /// Examples: title generation, compact-summary generation. These are captured via the OTEL
    /// log stream (`claude_code.api_request` records where `query_source != "repl_main_thread"`)
    /// and stored here so spend queries pick them up automatically.
    ///
    /// `request_id` is the Anthropic API request ID and serves as a dedup key — stored in the
    /// `events.request_id` column with a unique partial index.
    BackgroundApiCall {
        request_id: String,
        model: String,
        cost_usd: f64,
        query_source: String,
    },
    /// A manual spend adjustment applied to a specific day.
    ///
    /// `amount_usd` may be negative (to reduce the spend shown for a month) or positive (to add
    /// costs that are otherwise invisible, e.g. pre-installation gap or a price-change correction).
    /// Stored under session_id `"__adjustments__"` with `timestamp = <day>T00:00:00Z`.
    CostAdjustment {
        day: String,
        amount_usd: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Other {
        hook_event_name: String,
        payload: serde_json::Value,
    },
}

impl Event {
    /// Returns a short string label for the event type, used as the `event_type` column in SQLite.
    pub fn event_type_label(&self) -> &'static str {
        match self {
            Event::ToolUse { .. } => "tool_use",
            Event::SessionStart { .. } => "session_start",
            Event::SessionEnd => "session_end",
            Event::SubagentStart { .. } => "subagent_start",
            Event::SubagentStop { .. } => "subagent_stop",
            Event::ContextCompression { .. } => "context_compression",
            Event::TokenUsage { .. } => "token_usage",
            Event::BackgroundApiCall { .. } => "background_api_call",
            Event::CostAdjustment { .. } => "cost_adjustment",
            Event::Other { .. } => "other",
        }
    }

    /// Returns the sum of all token fields for TokenUsage events, or None for other event types.
    pub fn total_tokens(&self) -> Option<u64> {
        match self {
            Event::TokenUsage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            } => Some(input_tokens + output_tokens + cache_creation_input_tokens + cache_read_input_tokens), // cache_creation_1h is a subset, not additive
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_use_round_trip() {
        let event = Event::ToolUse {
            tool_name: "bash".to_string(),
            status: "success".to_string(),
            duration_ms: Some(42),
            error: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        match back {
            Event::ToolUse { tool_name, status, duration_ms, error } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(status, "success");
                assert_eq!(duration_ms, Some(42));
                assert!(error.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn session_start_round_trip() {
        let event = Event::SessionStart {
            model: "claude-sonnet-4-6".to_string(),
            source: "claude-code".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        match back {
            Event::SessionStart { model, source } => {
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(source, "claude-code");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn session_end_round_trip() {
        let event = Event::SessionEnd;
        let json = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, Event::SessionEnd));
    }

    #[test]
    fn other_round_trip() {
        let payload = serde_json::json!({"foo": "bar", "count": 3});
        let event = Event::Other {
            hook_event_name: "unknown_hook".to_string(),
            payload: payload.clone(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        match back {
            Event::Other { hook_event_name, payload: p } => {
                assert_eq!(hook_event_name, "unknown_hook");
                assert_eq!(p, payload);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_type_labels() {
        assert_eq!(Event::SessionEnd.event_type_label(), "session_end");
        assert_eq!(
            Event::ToolUse {
                tool_name: "x".into(),
                status: "ok".into(),
                duration_ms: None,
                error: None
            }
            .event_type_label(),
            "tool_use"
        );
    }
}
