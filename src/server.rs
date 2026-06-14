use std::net::SocketAddr;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde::Serialize;

use crate::otel_receiver::SessionCosts;
use crate::storage;

#[derive(Clone)]
pub struct AppState {
    pub costs: SessionCosts,
    pub budget_usd: f64,
}

#[derive(Serialize)]
struct SpendResponse {
    period: String,
    spent_estimated_usd: f64,
    budget_usd: f64,
    sources: SpendSources,
    note: &'static str,
}

#[derive(Serialize)]
struct SpendSources {
    sessions_usd: f64,
    sessions_count: usize,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
struct StatusResponse {
    otel: OtelStatus,
}

#[derive(Serialize)]
struct OtelStatus {
    batches_received: u64,
    last_received: Option<String>,
    active_sessions: usize,
    active_usd: f64,
}

fn round2(v: f64) -> f64 {
    (v.abs() * 100.0).round() / 100.0
}

pub async fn start_server(port: u16, state: AppState) {
    let app = Router::new()
        .route("/spend/monthly", get(handle_spend_monthly))
        .route("/status", get(handle_status))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{} trakr: API server failed to bind on {}: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), addr, e);
            return;
        }
    };
    eprintln!("{} trakr: API server listening on http://{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), addr);
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("{} trakr: API server error: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"), e);
    }
}

async fn handle_spend_monthly(
    State(state): State<AppState>,
) -> Result<Json<SpendResponse>, (StatusCode, Json<ErrorResponse>)> {
    let year_month = Utc::now().format("%Y-%m").to_string();

    let (sessions_usd, sessions_count) = storage::get_monthly_spend_usd(&year_month)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("DB error: {}", e) }),
            )
        })?;

    Ok(Json(SpendResponse {
        period: year_month,
        spent_estimated_usd: round2(sessions_usd),
        budget_usd: state.budget_usd,
        sources: SpendSources {
            sessions_usd: round2(sessions_usd),
            sessions_count,
        },
        note: "Costs are estimates based on the published Anthropic rate card.",
    }))
}

async fn handle_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let guard = state.costs.lock().unwrap();
    Json(StatusResponse {
        otel: OtelStatus {
            batches_received: guard.batches_received,
            last_received: guard.last_received.map(|t| t.to_rfc3339()),
            active_sessions: guard.session_costs.len(),
            active_usd: round2(guard.session_costs.values().sum()),
        },
    })
}
