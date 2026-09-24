//! The operator API.
//!
//! These routes carry evidence into the gateway and close a rail when
//! something stops adding up. They are authenticated by an operator key, not a
//! merchant key, and every one of them refuses rather than guesses: a price
//! with too few independent sources, sources that disagree beyond the policy,
//! a reopening without a reason.

use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use gateway_application::{ManualResolution, OperationsError, RiskSubmission};
use gateway_domain::{
    CurrencyCode, ManualResolutionAction, PriceReading, RailHealth, RawAmount,
    RemainderDisposition, RiskDecision,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{AppState, auth::OperatorAuth, error::ApiError};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceSubmission {
    asset_id: Uuid,
    fiat_currency: String,
    readings: Vec<PriceReadingBody>,
}

/// One source's reading. Rates are decimal strings of integers: a rate is a
/// ratio, and it never passes through a JSON number on the way in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceReadingBody {
    source_key: String,
    provider_group: String,
    rate_numerator: String,
    rate_denominator: String,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct PriceSnapshotResponse {
    snapshot_id: Uuid,
    rate_numerator: String,
    rate_denominator: String,
    source_group_count: u32,
    deviation_bps: u32,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
}

pub async fn submit_price(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    payload: Result<Json<PriceSubmission>, JsonRejection>,
) -> Result<(StatusCode, Json<PriceSnapshotResponse>), ApiError> {
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let currency = CurrencyCode::new(body.fiat_currency).map_err(|_| ApiError::InvalidJson)?;
    let mut readings = Vec::with_capacity(body.readings.len());
    for reading in body.readings {
        readings.push(PriceReading {
            source_key: reading.source_key,
            provider_group: reading.provider_group,
            rate_numerator: parse_rate(&reading.rate_numerator)?,
            rate_denominator: parse_rate(&reading.rate_denominator)?,
            observed_at: reading.observed_at,
        });
    }
    let recorded = state
        .operations
        .submit_price(&auth.0, body.asset_id, &currency, readings)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PriceSnapshotResponse {
            snapshot_id: recorded.snapshot_id,
            rate_numerator: recorded.rate_numerator,
            rate_denominator: recorded.rate_denominator,
            source_group_count: recorded.group_count,
            deviation_bps: recorded.deviation_bps,
            observed_at: recorded.observed_at,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RailHealthBody {
    asset_id: Uuid,
    health: RailHealth,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AcceptedResponse {
    id: Uuid,
}

pub async fn submit_rail_health(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    payload: Result<Json<RailHealthBody>, JsonRejection>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let id = state
        .operations
        .submit_rail_health(&auth.0, body.asset_id, body.health, body.detail)
        .await?;
    Ok((StatusCode::CREATED, Json(AcceptedResponse { id })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRailStopBody {
    asset_id: Uuid,
    reason_code: String,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RailStopResponse {
    id: Uuid,
    asset_id: Uuid,
    reason_code: String,
    detail: Option<String>,
    opened_by: String,
    #[serde(with = "time::serde::rfc3339")]
    opened_at: OffsetDateTime,
}

pub async fn open_rail_stop(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    payload: Result<Json<OpenRailStopBody>, JsonRejection>,
) -> Result<(StatusCode, Json<RailStopResponse>), ApiError> {
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let stop = state
        .operations
        .open_rail_stop(
            &auth.0,
            body.asset_id,
            &body.reason_code,
            body.detail.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(RailStopResponse {
            id: stop.id,
            asset_id: stop.asset_id,
            reason_code: stop.reason_code,
            detail: stop.detail,
            opened_by: stop.opened_by,
            opened_at: stop.opened_at,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearRailStopBody {
    reason: String,
}

pub async fn clear_rail_stop(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    Path(asset_id): Path<Uuid>,
    payload: Result<Json<ClearRailStopBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    state
        .operations
        .clear_rail_stop(&auth.0, asset_id, &body.reason)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskEvaluationBody {
    provider: String,
    decision: RiskDecision,
    #[serde(default)]
    score: Option<i32>,
    #[serde(default)]
    reasons: Option<Value>,
    #[serde(with = "time::serde::rfc3339")]
    evaluated_at: OffsetDateTime,
}

pub async fn submit_risk_evaluation(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    Path(transfer_id): Path<Uuid>,
    payload: Result<Json<RiskEvaluationBody>, JsonRejection>,
) -> Result<(StatusCode, Json<AcceptedResponse>), ApiError> {
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let reasons = match body.reasons {
        Some(value @ Value::Object(_)) => value,
        None => Value::Object(serde_json::Map::new()),
        // A screening reason is a document, not a list or a bare string: the
        // column stores an object and a wrong shape is refused at the edge.
        Some(_) => return Err(ApiError::InvalidJson),
    };
    let id = state
        .operations
        .submit_risk_evaluation(
            &auth.0,
            &RiskSubmission {
                transfer_id,
                provider: body.provider,
                decision: body.decision,
                score: body.score,
                reasons,
                evaluated_at: body.evaluated_at,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(AcceptedResponse { id })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualResolutionBody {
    action: ManualResolutionAction,
    transfer_id: Uuid,
    #[serde(default)]
    payment_intent_id: Option<Uuid>,
    #[serde(default)]
    attempt_id: Option<Uuid>,
    #[serde(default)]
    allocate_raw: Option<String>,
    #[serde(default)]
    remainder_raw: Option<String>,
    #[serde(default)]
    disposition: Option<RemainderDisposition>,
    #[serde(default)]
    external_reference: Option<String>,
    reason: String,
}

#[derive(Debug, Serialize)]
pub struct ManualResolutionResponse {
    id: Uuid,
    action: ManualResolutionAction,
    transfer_id: Uuid,
    payment_intent_id: Option<Uuid>,
    attempt_id: Option<Uuid>,
    merchant_id: Option<Uuid>,
    allocated_raw: Option<String>,
    remainder_raw: Option<String>,
    replayed: bool,
}

pub async fn resolve_manual(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    headers: HeaderMap,
    payload: Result<Json<ManualResolutionBody>, JsonRejection>,
) -> Result<(StatusCode, Json<ManualResolutionResponse>), ApiError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::InvalidJson)?;
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let parse_optional = |value: Option<String>| -> Result<Option<RawAmount>, ApiError> {
        value
            .map(|v| v.parse::<RawAmount>().map_err(|_| ApiError::InvalidJson))
            .transpose()
    };
    let result = state
        .operations
        .resolve_manual(
            &auth.0,
            key,
            &ManualResolution {
                action: body.action,
                transfer_id: body.transfer_id,
                payment_intent_id: body.payment_intent_id,
                attempt_id: body.attempt_id,
                allocate_raw: parse_optional(body.allocate_raw)?,
                remainder_raw: parse_optional(body.remainder_raw)?,
                disposition: body.disposition,
                external_reference: body.external_reference,
                reason: body.reason,
            },
        )
        .await?;
    let status = if result.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(ManualResolutionResponse {
            id: result.id,
            action: result.action,
            transfer_id: result.transfer_id,
            payment_intent_id: result.payment_intent_id,
            attempt_id: result.attempt_id,
            merchant_id: result.merchant_id,
            allocated_raw: result.allocated_raw.map(|v| v.to_string()),
            remainder_raw: result.remainder_raw.map(|v| v.to_string()),
            replayed: result.replayed,
        }),
    ))
}

fn parse_rate(value: &str) -> Result<RawAmount, ApiError> {
    value
        .parse::<RawAmount>()
        .map_err(|_| ApiError::InvalidJson)
}

/// Maps the operator service's refusals onto status codes that say which kind
/// of refusal happened.
pub fn status_for(error: &OperationsError) -> (StatusCode, &'static str) {
    match error {
        OperationsError::MissingScope(_) => (StatusCode::FORBIDDEN, "missing_scope"),
        OperationsError::RiskProviderNotAllowed => {
            (StatusCode::FORBIDDEN, "risk_provider_not_allowed")
        }
        OperationsError::NoReadings
        | OperationsError::ReasonRequired
        | OperationsError::InvalidPageLimit
        | OperationsError::InvalidIdempotencyKey
        | OperationsError::InvalidManualResolution => (StatusCode::BAD_REQUEST, "invalid_request"),
        OperationsError::ManualResolutionNotFound => {
            (StatusCode::NOT_FOUND, "manual_resolution_not_found")
        }
        OperationsError::ManualResolutionConflict => {
            (StatusCode::CONFLICT, "manual_resolution_conflict")
        }
        OperationsError::InvalidRiskEvaluation => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_risk_evaluation")
        }
        OperationsError::NoPricePolicy => (StatusCode::CONFLICT, "no_price_policy"),
        OperationsError::NoOpenRailStop => (StatusCode::CONFLICT, "no_open_rail_stop"),
        OperationsError::Price(_) => (StatusCode::UNPROCESSABLE_ENTITY, "price_not_agreed"),
        OperationsError::UnknownScope(_) | OperationsError::SnapshotNotRecorded => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
        OperationsError::Repository(gateway_application::RepositoryError::IdempotencyConflict) => {
            (StatusCode::CONFLICT, "idempotency_conflict")
        }
        OperationsError::Repository(_) => (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}
