use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;

/// Shared OTEL receiver state: per-session costs plus receive statistics.
///
/// "unknown" session is used when no session.id attribute is present in the metric.
#[derive(Debug, Default)]
pub struct OtelState {
    /// session_id → cumulative estimated cost in USD from OTEL data.
    pub session_costs: HashMap<String, f64>,
    /// Number of OTLP metric batches received (any valid POST to /v1/metrics).
    pub batches_received: u64,
    /// Timestamp of the most recent batch.
    pub last_received: Option<chrono::DateTime<chrono::Utc>>,
}

pub type SessionCosts = Arc<Mutex<OtelState>>;

pub fn new_session_costs() -> SessionCosts {
    Arc::new(Mutex::new(OtelState::default()))
}

/// Start the OTLP HTTP/JSON receiver on the given port.
///
/// Claude Code must be configured with:
///   OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:<otel_port>
///   OTEL_EXPORTER_OTLP_PROTOCOL=http/json
///
/// While running, every raw OTLP batch is also appended to:
///   ~/.trakr/otel-dump-metrics.jsonl
///   ~/.trakr/otel-dump-logs.jsonl
/// These files accumulate indefinitely; delete them once the experiment is done.
pub async fn start_otel_receiver(port: u16, costs: SessionCosts) {
    let app = Router::new()
        .route("/v1/metrics", post(handle_metrics))
        .route("/v1/logs", post(handle_logs))
        .with_state(costs);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{} trakr: OTEL receiver failed to bind on {}: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), addr, e);
            return;
        }
    };
    eprintln!("{} trakr: OTEL receiver listening on {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), addr);
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("{} trakr: OTEL receiver error: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), e);
    }
}

/// Append `body` as a single JSON line to `filename` inside `~/.trakr/`.
/// Best-effort: errors are silently ignored so the receiver never fails on I/O.
fn dump_to_jsonl(filename: &str, body: &[u8]) {
    use std::io::Write;
    let Some(home) = dirs::home_dir() else { return };
    let path = home.join(".trakr").join(filename);
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    let _ = f.write_all(body);
    let _ = f.write_all(b"\n");
}

async fn handle_metrics(
    State(costs): State<SessionCosts>,
    body: axum::body::Bytes,
) -> StatusCode {
    dump_to_jsonl("otel-dump-metrics.jsonl", &body);

    let json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    {
        let mut guard = costs.lock().unwrap();
        guard.batches_received += 1;
        guard.last_received = Some(chrono::Utc::now());
    }

    if let Some(resource_metrics) = json.get("resourceMetrics").and_then(|v| v.as_array()) {
        for rm in resource_metrics {
            // session.id may live in Resource attributes.
            let resource_session_id = extract_session_id(
                rm.get("resource")
                    .and_then(|r| r.get("attributes"))
                    .and_then(|a| a.as_array()),
            );

            if let Some(scope_metrics) = rm.get("scopeMetrics").and_then(|v| v.as_array()) {
                for sm in scope_metrics {
                    if let Some(metrics) = sm.get("metrics").and_then(|v| v.as_array()) {
                        for metric in metrics {
                            let name = metric
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if name != "claude_code.cost.usage" {
                                continue;
                            }
                            process_cost_metric(
                                metric,
                                resource_session_id.as_deref(),
                                &costs,
                            );
                        }
                    }
                }
            }
        }
    }

    StatusCode::OK
}

fn process_cost_metric(
    metric: &serde_json::Value,
    resource_session_id: Option<&str>,
    costs: &SessionCosts,
) {
    // Handle both gauge and sum data point shapes.
    let data_points = metric
        .get("gauge")
        .and_then(|g| g.get("dataPoints"))
        .or_else(|| metric.get("sum").and_then(|s| s.get("dataPoints")))
        .and_then(|v| v.as_array());

    let Some(data_points) = data_points else { return };

    let mut guard = costs.lock().unwrap();
    for dp in data_points {
        let value = dp.get("asDouble").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if value <= 0.0 {
            continue;
        }

        // Prefer data-point-level session.id, fall back to resource-level, then "unknown".
        let dp_attrs = dp.get("attributes").and_then(|a| a.as_array());
        let session_id = extract_session_id(dp_attrs)
            .or_else(|| resource_session_id.map(String::from))
            .unwrap_or_else(|| "unknown".to_string());

        *guard.session_costs.entry(session_id).or_insert(0.0) += value;
    }
}

/// Receive OTLP log batches (POST /v1/logs).
///
/// Parses `claude_code.api_request` records and stores any with a non-main `query_source`
/// (title generation, context preloading, etc.) as `BackgroundApiCall` events in the DB.
/// These are the calls that Claude Code bills but never writes to session transcripts.
///
/// The raw payload is also appended to `~/.trakr/otel-dump-logs.jsonl` for debugging.
async fn handle_logs(
    State(_costs): State<SessionCosts>,
    body: axum::body::Bytes,
) -> StatusCode {
    dump_to_jsonl("otel-dump-logs.jsonl", &body);

    let json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let records = extract_background_api_calls(&json);
    if !records.is_empty() {
        tokio::task::spawn_blocking(move || {
            for (session_id, ts, event) in records {
                let _ = crate::storage::insert_background_api_call(&session_id, &event, ts);
            }
        });
    }

    StatusCode::OK
}

/// Parse a raw OTLP logs payload and return background API call records.
///
/// Returns only `claude_code.api_request` log records whose `query_source` is not
/// `"repl_main_thread"` — those are transcript-invisible background calls.
fn extract_background_api_calls(
    json: &serde_json::Value,
) -> Vec<(String, chrono::DateTime<chrono::Utc>, crate::event::Event)> {
    let mut out = Vec::new();
    let Some(resource_logs) = json.get("resourceLogs").and_then(|v| v.as_array()) else {
        return out;
    };
    for rl in resource_logs {
        let Some(scope_logs) = rl.get("scopeLogs").and_then(|v| v.as_array()) else { continue };
        for sl in scope_logs {
            let Some(log_records) = sl.get("logRecords").and_then(|v| v.as_array()) else { continue };
            for rec in log_records {
                let body = rec.get("body")
                    .and_then(|b| b.get("stringValue"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if body != "claude_code.api_request" {
                    continue;
                }

                let attrs_arr = rec.get("attributes").and_then(|a| a.as_array());
                let get_str = |key: &str| -> Option<String> {
                    extract_string_attr(attrs_arr, key)
                };
                let get_f64 = |key: &str| -> f64 {
                    attrs_arr
                        .and_then(|arr| {
                            arr.iter().find(|a| a.get("key").and_then(|v| v.as_str()) == Some(key))
                        })
                        .and_then(|a| a.get("value"))
                        .and_then(|v| v.get("doubleValue").and_then(|v| v.as_f64())
                            .or_else(|| v.get("intValue").and_then(|v| v.as_i64()).map(|i| i as f64)))
                        .unwrap_or(0.0)
                };

                let query_source = get_str("query_source").unwrap_or_default();
                // "repl_main_thread" calls are in the transcript; skip them.
                if query_source == "repl_main_thread" {
                    continue;
                }

                let Some(request_id) = get_str("request_id") else { continue };
                let Some(session_id) = get_str("session.id") else { continue };
                let model = get_str("model").unwrap_or_else(|| "unknown".to_string());
                let cost_usd = get_f64("cost_usd");

                if cost_usd <= 0.0 {
                    continue;
                }

                // Parse timestamp from `event.timestamp` attribute (ISO 8601).
                let ts = get_str("event.timestamp")
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(chrono::Utc::now);

                out.push((
                    session_id,
                    ts,
                    crate::event::Event::BackgroundApiCall {
                        request_id,
                        model,
                        cost_usd,
                        query_source,
                    },
                ));
            }
        }
    }
    out
}

/// Claude Code attaches the session identifier as `session.id` (OTel semantic
/// convention style). Accept the legacy `session_id` spelling as a fallback.
fn extract_session_id(attrs: Option<&Vec<serde_json::Value>>) -> Option<String> {
    extract_string_attr(attrs, "session.id")
        .or_else(|| extract_string_attr(attrs, "session_id"))
}

/// Extract a string attribute value from an OTLP attributes array.
///
/// OTLP JSON attributes look like: [{"key": "k", "value": {"stringValue": "v"}}]
fn extract_string_attr(
    attrs: Option<&Vec<serde_json::Value>>,
    key: &str,
) -> Option<String> {
    let attrs = attrs?;
    for attr in attrs {
        if attr.get("key").and_then(|v| v.as_str()) == Some(key) {
            let val = attr.get("value")?;
            // stringValue
            if let Some(s) = val.get("stringValue").and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_costs() -> SessionCosts {
        new_session_costs()
    }

    #[test]
    fn extracts_string_attr() {
        let attrs = vec![json!({"key": "session.id", "value": {"stringValue": "abc123"}})];
        let result = extract_session_id(Some(&attrs));
        assert_eq!(result.as_deref(), Some("abc123"));
    }

    #[test]
    fn extracts_legacy_session_id_spelling() {
        let attrs = vec![json!({"key": "session_id", "value": {"stringValue": "abc123"}})];
        let result = extract_session_id(Some(&attrs));
        assert_eq!(result.as_deref(), Some("abc123"));
    }

    #[test]
    fn extracts_missing_attr_returns_none() {
        let attrs = vec![json!({"key": "model", "value": {"stringValue": "claude-sonnet"}})];
        let result = extract_session_id(Some(&attrs));
        assert!(result.is_none());
    }

    #[test]
    fn process_gauge_metric_accumulates() {
        let costs = make_costs();
        let metric = json!({
            "name": "claude_code.cost.usage",
            "gauge": {
                "dataPoints": [
                    {
                        "attributes": [{"key": "session.id", "value": {"stringValue": "sess1"}}],
                        "asDouble": 0.05
                    }
                ]
            }
        });
        process_cost_metric(&metric, None, &costs);
        process_cost_metric(&metric, None, &costs);
        let guard = costs.lock().unwrap();
        assert!((guard.session_costs["sess1"] - 0.10).abs() < 1e-9);
    }

    #[test]
    fn process_metric_uses_resource_session_id_as_fallback() {
        let costs = make_costs();
        let metric = json!({
            "name": "claude_code.cost.usage",
            "gauge": {
                "dataPoints": [{"asDouble": 0.03}]
            }
        });
        process_cost_metric(&metric, Some("resource_sess"), &costs);
        let guard = costs.lock().unwrap();
        assert!((guard.session_costs["resource_sess"] - 0.03).abs() < 1e-9);
    }

    #[test]
    fn process_metric_unknown_when_no_session() {
        let costs = make_costs();
        let metric = json!({
            "name": "claude_code.cost.usage",
            "gauge": {
                "dataPoints": [{"asDouble": 0.01}]
            }
        });
        process_cost_metric(&metric, None, &costs);
        let guard = costs.lock().unwrap();
        assert!((guard.session_costs["unknown"] - 0.01).abs() < 1e-9);
    }

    #[test]
    fn sum_metric_also_handled() {
        let costs = make_costs();
        let metric = json!({
            "name": "claude_code.cost.usage",
            "sum": {
                "dataPoints": [
                    {
                        "attributes": [{"key": "session.id", "value": {"stringValue": "sum_sess"}}],
                        "asDouble": 0.07
                    }
                ]
            }
        });
        process_cost_metric(&metric, None, &costs);
        let guard = costs.lock().unwrap();
        assert!((guard.session_costs["sum_sess"] - 0.07).abs() < 1e-9);
    }

    fn make_api_request_log(
        request_id: &str,
        session_id: &str,
        model: &str,
        query_source: &str,
        cost_usd: f64,
    ) -> serde_json::Value {
        json!({
            "resourceLogs": [{
                "scopeLogs": [{
                    "logRecords": [{
                        "body": {"stringValue": "claude_code.api_request"},
                        "attributes": [
                            {"key": "request_id",     "value": {"stringValue": request_id}},
                            {"key": "session.id",     "value": {"stringValue": session_id}},
                            {"key": "model",          "value": {"stringValue": model}},
                            {"key": "query_source",   "value": {"stringValue": query_source}},
                            {"key": "cost_usd",       "value": {"doubleValue": cost_usd}},
                            {"key": "event.timestamp","value": {"stringValue": "2026-06-15T16:31:21.073Z"}}
                        ]
                    }]
                }]
            }]
        })
    }

    #[test]
    fn extract_background_skips_main_thread() {
        let payload = make_api_request_log(
            "req_001", "sess1", "claude-sonnet-4-6", "repl_main_thread", 0.05,
        );
        let records = extract_background_api_calls(&payload);
        assert!(records.is_empty(), "repl_main_thread should be skipped");
    }

    #[test]
    fn extract_background_captures_title_gen() {
        let payload = make_api_request_log(
            "req_002", "sess2", "claude-haiku-4-5-20251001", "generate_session_title", 0.000627,
        );
        let records = extract_background_api_calls(&payload);
        assert_eq!(records.len(), 1);
        let (session_id, _ts, event) = &records[0];
        assert_eq!(session_id, "sess2");
        match event {
            crate::event::Event::BackgroundApiCall { request_id, model, cost_usd, query_source } => {
                assert_eq!(request_id, "req_002");
                assert_eq!(model, "claude-haiku-4-5-20251001");
                assert!((cost_usd - 0.000627).abs() < 1e-9);
                assert_eq!(query_source, "generate_session_title");
            }
            _ => panic!("expected BackgroundApiCall"),
        }
    }

    #[test]
    fn extract_background_captures_auxiliary() {
        let payload = make_api_request_log(
            "req_003", "sess3", "claude-sonnet-4-6", "auxiliary", 0.014,
        );
        let records = extract_background_api_calls(&payload);
        assert_eq!(records.len(), 1);
        match &records[0].2 {
            crate::event::Event::BackgroundApiCall { query_source, .. } => {
                assert_eq!(query_source, "auxiliary");
            }
            _ => panic!("expected BackgroundApiCall"),
        }
    }

    #[test]
    fn extract_background_skips_zero_cost() {
        let payload = make_api_request_log(
            "req_004", "sess4", "claude-haiku-4-5-20251001", "generate_session_title", 0.0,
        );
        let records = extract_background_api_calls(&payload);
        assert!(records.is_empty(), "zero-cost records should be skipped");
    }
}
