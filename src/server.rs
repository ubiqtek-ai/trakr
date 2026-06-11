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
    completed_sessions_usd: f64,
    completed_sessions_count: usize,
    active_sessions_usd: f64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

fn round2(v: f64) -> f64 {
    (v.abs() * 100.0).round() / 100.0
}

pub async fn start_server(port: u16, state: AppState) {
    let app = Router::new()
        .route("/spend/monthly", get(handle_spend_monthly))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("trakr: API server failed to bind on {}: {}", addr, e);
            return;
        }
    };
    eprintln!("trakr: API server listening on http://{}", addr);
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("trakr: API server error: {}", e);
    }
}

async fn handle_spend_monthly(
    State(state): State<AppState>,
) -> Result<Json<SpendResponse>, (StatusCode, Json<ErrorResponse>)> {
    let year_month = Utc::now().format("%Y-%m").to_string();

    let (completed_usd, completed_count) = storage::get_monthly_spend_usd(&year_month)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: format!("DB error: {}", e) }),
            )
        })?;

    let completed_ids = storage::get_completed_session_ids().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: format!("DB error: {}", e) }),
        )
    })?;

    // Sum OTEL costs for sessions not yet completed.
    let active_usd: f64 = {
        let guard = state.costs.lock().unwrap();
        guard
            .iter()
            .filter(|(session_id, _)| !completed_ids.contains(*session_id))
            .map(|(_, cost)| cost)
            .sum()
    };

    let total = completed_usd + active_usd;

    Ok(Json(SpendResponse {
        period: year_month,
        spent_estimated_usd: round2(total),
        budget_usd: state.budget_usd,
        sources: SpendSources {
            completed_sessions_usd: round2(completed_usd),
            completed_sessions_count: completed_count,
            active_sessions_usd: round2(active_usd),
        },
        note: "Costs are estimates based on the published Anthropic rate card.",
    }))
}
