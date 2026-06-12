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
pub async fn start_otel_receiver(port: u16, costs: SessionCosts) {
    let app = Router::new()
        .route("/v1/metrics", post(handle_metrics))
        .with_state(costs);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("trakr: OTEL receiver failed to bind on {}: {}", addr, e);
            return;
        }
    };
    eprintln!("trakr: OTEL receiver listening on {}", addr);
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("trakr: OTEL receiver error: {}", e);
    }
}

async fn handle_metrics(
    State(costs): State<SessionCosts>,
    body: axum::body::Bytes,
) -> StatusCode {
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
}
