use axum::{Json, extract::State};
use serde::Serialize;

use crate::{AppState, error::ApiError};

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: &'static str,
}

pub async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn ready(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .map_err(|_| ApiError::NotReady)?;
    Ok(Json(HealthResponse { status: "ready" }))
}
