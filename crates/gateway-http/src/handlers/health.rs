use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use gateway_application::{SelfCheckReport, SelfCheckResult};
use serde::Serialize;
use time::{Duration, OffsetDateTime};

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: &'static str,
}

pub async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// The readiness answer carries when it was evaluated, so a cached report is
/// visibly a cached report and never an unexplained stale "ready".
#[derive(Debug, Serialize)]
pub struct ReadinessResponse {
    ready: bool,
    #[serde(with = "time::serde::rfc3339")]
    evaluated_at: OffsetDateTime,
    checks: Vec<SelfCheckResult>,
}

pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let now = OffsetDateTime::now_utc();
    let cached = state.self_check_cache.lock().await.clone();
    let report = if let Some(report) =
        cached.filter(|report| report.evaluated_at + Duration::seconds(10) >= now)
    {
        report
    } else if let Some(service) = &state.self_check {
        match service.run().await {
            Ok(report) => {
                *state.self_check_cache.lock().await = Some(report.clone());
                report
            }
            Err(error) => SelfCheckReport::new(
                now,
                vec![SelfCheckResult {
                    name: "storage".to_owned(),
                    passed: false,
                    detail: error.to_string(),
                }],
            ),
        }
    } else {
        SelfCheckReport::new(
            now,
            vec![SelfCheckResult {
                name: "configuration".to_owned(),
                passed: false,
                detail: "startup self-check configuration was not installed".to_owned(),
            }],
        )
    };
    let status = if report.passed {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(ReadinessResponse {
            ready: report.passed,
            evaluated_at: report.evaluated_at,
            checks: report.checks,
        }),
    )
}
