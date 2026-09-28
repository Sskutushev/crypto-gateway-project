//! Two-operator honors over HTTP: propose, list, approve, reject.

use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use gateway_application::{HonorProposal, ManualResolution};
use gateway_domain::{ManualResolutionAction, RawAmount};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    operator::{ManualResolutionResponse, idempotency_key},
    operator_reads::{ListResponse, PageQuery},
};
use crate::{AppState, auth::OperatorAuth, error::ApiError};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeHonorBody {
    transfer_id: Uuid,
    payment_intent_id: Uuid,
    attempt_id: Uuid,
    allocate_raw: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionBody {
    reason: String,
}

#[derive(Debug, Serialize)]
pub struct HonorProposalResponse {
    id: Uuid,
    status: &'static str,
    transfer_id: Uuid,
    payment_intent_id: Uuid,
    attempt_id: Uuid,
    merchant_id: Uuid,
    allocate_raw: String,
    threshold_raw: String,
    reason: String,
    evidence: Value,
    proposed_by_key_id: Uuid,
    proposed_by_label: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    decided_by_key_id: Option<Uuid>,
    decided_by_label: Option<String>,
    decision_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    decided_at: Option<OffsetDateTime>,
    resolution_id: Option<Uuid>,
    replayed: bool,
}

impl From<HonorProposal> for HonorProposalResponse {
    fn from(v: HonorProposal) -> Self {
        Self {
            id: v.id,
            status: v.status.as_str(),
            transfer_id: v.transfer_id,
            payment_intent_id: v.payment_intent_id,
            attempt_id: v.attempt_id,
            merchant_id: v.merchant_id,
            allocate_raw: v.allocate_raw.to_string(),
            threshold_raw: v.threshold_raw.to_string(),
            reason: v.reason,
            evidence: v.evidence,
            proposed_by_key_id: v.proposed_by_key_id,
            proposed_by_label: v.proposed_by_label,
            created_at: v.created_at,
            expires_at: v.expires_at,
            decided_by_key_id: v.decided_by_key_id,
            decided_by_label: v.decided_by_label,
            decision_reason: v.decision_reason,
            decided_at: v.decided_at,
            resolution_id: v.resolution_id,
            replayed: v.replayed,
        }
    }
}

pub async fn propose(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    headers: HeaderMap,
    payload: Result<Json<ProposeHonorBody>, JsonRejection>,
) -> Result<(StatusCode, Json<HonorProposalResponse>), ApiError> {
    let key = idempotency_key(&headers)?;
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let allocate = body
        .allocate_raw
        .parse::<RawAmount>()
        .map_err(|_| ApiError::InvalidJson)?;
    let proposal = state
        .operations
        .propose_honor(
            &auth.0,
            key,
            &ManualResolution {
                action: ManualResolutionAction::Honor,
                transfer_id: body.transfer_id,
                payment_intent_id: Some(body.payment_intent_id),
                attempt_id: Some(body.attempt_id),
                allocate_raw: Some(allocate),
                remainder_raw: None,
                disposition: None,
                external_reference: None,
                reason: body.reason,
            },
        )
        .await?;
    let status = if proposal.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(proposal.into())))
}

pub async fn pending(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    Query(query): Query<PageQuery>,
) -> Result<Json<ListResponse<HonorProposalResponse>>, ApiError> {
    let page = state
        .operations
        .pending_honor_proposals(&auth.0, query.limit, query.before)
        .await?;
    Ok(Json(ListResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_before: page.next_before,
    }))
}

pub async fn approve(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    Path(proposal_id): Path<Uuid>,
    headers: HeaderMap,
    payload: Result<Json<DecisionBody>, JsonRejection>,
) -> Result<(StatusCode, Json<ManualResolutionResponse>), ApiError> {
    let key = idempotency_key(&headers)?;
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let result = state
        .operations
        .approve_honor(&auth.0, proposal_id, key, &body.reason)
        .await?;
    let status = if result.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(result.into())))
}

pub async fn reject(
    State(state): State<AppState>,
    Extension(auth): Extension<OperatorAuth>,
    Path(proposal_id): Path<Uuid>,
    headers: HeaderMap,
    payload: Result<Json<DecisionBody>, JsonRejection>,
) -> Result<Json<HonorProposalResponse>, ApiError> {
    let key = idempotency_key(&headers)?;
    let Json(body) = payload.map_err(|_| ApiError::InvalidJson)?;
    let proposal = state
        .operations
        .reject_honor(&auth.0, proposal_id, key, &body.reason)
        .await?;
    Ok(Json(proposal.into()))
}
