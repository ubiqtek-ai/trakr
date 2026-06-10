use anyhow::Result;
use std::fs::File;
use std::io::{BufRead, BufReader};

/// Token usage extracted from a Claude Code transcript assistant entry.
pub struct Usage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// Parse a Claude Code transcript JSONL file and return token usage from the latest assistant entry.
///
/// Returns `Ok(None)` if the file does not exist, cannot be read, or contains no assistant entries.
/// Ignores individual line parse errors rather than failing.
pub fn parse_transcript(transcript_path: &str) -> Result<Option<Usage>> {
    let file = match File::open(transcript_path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .filter_map(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .collect();

    // Iterate in reverse to find the latest assistant entry first.
    for line in lines.iter().rev() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if json.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }

        let message = match json.get("message") {
            Some(m) => m,
            None => continue,
        };

        let model = message
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let usage = message.get("usage").cloned().unwrap_or(serde_json::Value::Null);

        let input_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_creation_input_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read_input_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        return Ok(Some(Usage {
            model,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        }));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_transcript(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("temp file");
        f.write_all(content.as_bytes()).expect("write");
        f
    }

    #[test]
    fn test_parse_assistant_entry() {
        let transcript = r#"{"type":"user","content":"test"}
{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
        let f = write_transcript(transcript);
        let result = parse_transcript(f.path().to_str().unwrap())
            .expect("no error")
            .expect("some usage");

        assert_eq!(result.model, "claude-opus-4-7");
        assert_eq!(result.input_tokens, 100);
        assert_eq!(result.output_tokens, 50);
        assert_eq!(result.cache_creation_input_tokens, 0);
        assert_eq!(result.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_parse_latest_assistant_when_multiple() {
        let transcript = r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
{"type":"user","content":"next turn"}
{"type":"assistant","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":200,"output_tokens":80,"cache_creation_input_tokens":1000,"cache_read_input_tokens":500}}}
"#;
        let f = write_transcript(transcript);
        let result = parse_transcript(f.path().to_str().unwrap())
            .expect("no error")
            .expect("some usage");

        // Should return the LAST assistant entry.
        assert_eq!(result.model, "claude-sonnet-4-6");
        assert_eq!(result.input_tokens, 200);
        assert_eq!(result.output_tokens, 80);
        assert_eq!(result.cache_creation_input_tokens, 1000);
        assert_eq!(result.cache_read_input_tokens, 500);
    }

    #[test]
    fn test_parse_missing_transcript() {
        let result = parse_transcript("/nonexistent/path/to/transcript.jsonl")
            .expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_no_assistant_entry() {
        let transcript = r#"{"type":"user","content":"hello"}
{"type":"system","content":"ready"}
"#;
        let f = write_transcript(transcript);
        let result = parse_transcript(f.path().to_str().unwrap())
            .expect("no error");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_missing_usage_fields() {
        // Assistant entry without usage object at all.
        let transcript = r#"{"type":"assistant","message":{"model":"claude-haiku-4","content":"hi"}}
"#;
        let f = write_transcript(transcript);
        let result = parse_transcript(f.path().to_str().unwrap())
            .expect("no error")
            .expect("some usage");

        assert_eq!(result.model, "claude-haiku-4");
        assert_eq!(result.input_tokens, 0);
        assert_eq!(result.output_tokens, 0);
        assert_eq!(result.cache_creation_input_tokens, 0);
        assert_eq!(result.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_parse_partial_usage_fields() {
        // Usage object present but missing some fields — should default to 0.
        let transcript = r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":42,"output_tokens":8}}}
"#;
        let f = write_transcript(transcript);
        let result = parse_transcript(f.path().to_str().unwrap())
            .expect("no error")
            .expect("some usage");

        assert_eq!(result.input_tokens, 42);
        assert_eq!(result.output_tokens, 8);
        assert_eq!(result.cache_creation_input_tokens, 0);
        assert_eq!(result.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_parse_skips_malformed_lines() {
        // Malformed lines should be ignored; valid assistant line should still be found.
        let transcript = r#"not valid json
{"type":"user","content":"ok"}
{broken
{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":7,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
        let f = write_transcript(transcript);
        let result = parse_transcript(f.path().to_str().unwrap())
            .expect("no error")
            .expect("some usage");

        assert_eq!(result.model, "claude-opus-4-7");
        assert_eq!(result.input_tokens, 7);
    }
}
