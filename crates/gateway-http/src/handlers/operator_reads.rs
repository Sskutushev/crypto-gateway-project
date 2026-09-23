use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use gateway_application::{
    DeadLetter, EvidenceAllocation, EvidenceAttempt, EvidenceFulfillment, EvidenceIntent,
    EvidencePaymentEvent, EvidenceSettlementDecision, EvidenceTransfer, HeldPayment,
    ObservationConflict, Page, PaymentIntentEvidence, ReconciliationDiscrepancy, ReconciliationRun,
    UnmatchedTransfer,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{AppState, auth::OperatorAuth, error::ApiError};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    limit: Option<u32>,
    before: Option<Uuid>,
}

#[derive(Serialize)]
pub(crate) struct ListResponse<T> {
    items: Vec<T>,
    next_before: Option<Uuid>,
}
#[derive(Serialize)]
pub(crate) struct ConflictResponse {
    id: Uuid,
    chain: String,
    network: String,
    environment: String,
    tx_hash: String,
    event_index: i32,
    field: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    items: Vec<ConflictItemResponse>,
}
#[derive(Serialize)]
struct ConflictItemResponse {
    observation_id: Uuid,
    field_value: Value,
}
impl From<ObservationConflict> for ConflictResponse {
    fn from(v: ObservationConflict) -> Self {
        Self {
            id: v.id,
            chain: v.chain,
            network: v.network,
            environment: v.environment,
            tx_hash: v.tx_hash,
            event_index: v.event_index,
            field: v.field,
            created_at: v.created_at,
            items: v
                .items
                .into_iter()
                .map(|i| ConflictItemResponse {
                    observation_id: i.observation_id,
                    field_value: i.field_value,
                })
                .collect(),
        }
    }
}
#[derive(Serialize)]
pub(crate) struct TransferResponse {
    id: Uuid,
    chain: String,
    network: String,
    environment: String,
    tx_hash: String,
    event_index: i32,
    collector_address: String,
    asset_id: Uuid,
    amount_raw: String,
    #[serde(with = "time::serde::rfc3339")]
    block_time: OffsetDateTime,
    finality_state: String,
    #[serde(with = "time::serde::rfc3339")]
    unmatched_at: OffsetDateTime,
}
impl From<UnmatchedTransfer> for TransferResponse {
    fn from(v: UnmatchedTransfer) -> Self {
        Self {
            id: v.id,
            chain: v.chain,
            network: v.network,
            environment: v.environment,
            tx_hash: v.tx_hash,
            event_index: v.event_index,
            collector_address: v.collector_address,
            asset_id: v.asset_id,
            amount_raw: v.amount_raw.to_string(),
            block_time: v.block_time,
            finality_state: v.finality_state,
            unmatched_at: v.unmatched_at,
        }
    }
}
#[derive(Serialize)]
struct DecisionResponse {
    outcome: String,
    risk_decision: String,
    decided_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    decided_at: OffsetDateTime,
}
#[derive(Serialize)]
pub(crate) struct HeldResponse {
    id: Uuid,
    merchant_id: Uuid,
    transfer_ids: Vec<Uuid>,
    latest_decision: Option<DecisionResponse>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}
impl From<HeldPayment> for HeldResponse {
    fn from(v: HeldPayment) -> Self {
        Self {
            id: v.id,
            merchant_id: v.merchant_id,
            transfer_ids: v.transfer_ids,
            latest_decision: v.latest_decision.map(|d| DecisionResponse {
                outcome: d.outcome,
                risk_decision: d.risk_decision,
                decided_reason: d.decided_reason,
                decided_at: d.decided_at,
            }),
            created_at: v.created_at,
        }
    }
}
#[derive(Serialize)]
struct DeliveryResponse {
    attempt: i32,
    response_status: Option<i32>,
    error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    delivered_at: OffsetDateTime,
}
#[derive(Serialize)]
pub(crate) struct DeadResponse {
    id: Uuid,
    merchant_id: Option<Uuid>,
    event_type: String,
    channel: String,
    aggregate_type: String,
    aggregate_id: Uuid,
    attempts: i32,
    last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    dead_lettered_at: OffsetDateTime,
    deliveries: Vec<DeliveryResponse>,
}
impl From<DeadLetter> for DeadResponse {
    fn from(v: DeadLetter) -> Self {
        Self {
            id: v.id,
            merchant_id: v.merchant_id,
            event_type: v.event_type,
            channel: v.channel,
            aggregate_type: v.aggregate_type,
            aggregate_id: v.aggregate_id,
            attempts: v.attempts,
            last_error: v.last_error,
            dead_lettered_at: v.dead_lettered_at,
            deliveries: v
                .deliveries
                .into_iter()
                .map(|d| DeliveryResponse {
                    attempt: d.attempt,
                    response_status: d.response_status,
                    error: d.error,
                    delivered_at: d.delivered_at,
                })
                .collect(),
        }
    }
}
#[derive(Serialize)]
pub(crate) struct RunResponse {
    id: Uuid,
    kind: String,
    #[serde(with = "time::serde::rfc3339")]
    window_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    window_end: OffsetDateTime,
    transfers_examined: i32,
    intents_examined: i32,
    discrepancy_count: i32,
    money_discrepancy_count: i32,
    status: String,
    #[serde(with = "time::serde::rfc3339")]
    started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    finished_at: Option<OffsetDateTime>,
}
impl From<ReconciliationRun> for RunResponse {
    fn from(v: ReconciliationRun) -> Self {
        Self {
            id: v.id,
            kind: v.kind,
            window_start: v.window_start,
            window_end: v.window_end,
            transfers_examined: v.transfers_examined,
            intents_examined: v.intents_examined,
            discrepancy_count: v.discrepancy_count,
            money_discrepancy_count: v.money_discrepancy_count,
            status: v.status,
            started_at: v.started_at,
            finished_at: v.finished_at,
        }
    }
}
#[derive(Serialize)]
pub(crate) struct DiscrepancyResponse {
    id: Uuid,
    run_id: Uuid,
    kind: String,
    money_affected: bool,
    transfer_id: Option<Uuid>,
    payment_intent_id: Option<Uuid>,
    asset_id: Option<Uuid>,
    detail: Value,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    resolved_at: Option<OffsetDateTime>,
    resolved_by: Option<String>,
    resolution: Option<String>,
}
impl From<ReconciliationDiscrepancy> for DiscrepancyResponse {
    fn from(v: ReconciliationDiscrepancy) -> Self {
        Self {
            id: v.id,
            run_id: v.run_id,
            kind: v.kind,
            money_affected: v.money_affected,
            transfer_id: v.transfer_id,
            payment_intent_id: v.payment_intent_id,
            asset_id: v.asset_id,
            detail: v.detail,
            created_at: v.created_at,
            resolved_at: v.resolved_at,
            resolved_by: v.resolved_by,
            resolution: v.resolution,
        }
    }
}
#[derive(Serialize)]
pub(crate) struct EvidenceResponse {
    intent: EvidenceIntent,
    attempts: Vec<EvidenceAttempt>,
    allocations: Vec<EvidenceAllocation>,
    settlement_decisions: Vec<EvidenceSettlementDecision>,
    fulfillment: Option<EvidenceFulfillment>,
    payment_events: Vec<EvidencePaymentEvent>,
    transfers: Vec<EvidenceTransfer>,
}
impl From<PaymentIntentEvidence> for EvidenceResponse {
    fn from(v: PaymentIntentEvidence) -> Self {
        Self {
            intent: v.intent,
            attempts: v.attempts,
            allocations: v.allocations,
            settlement_decisions: v.settlement_decisions,
            fulfillment: v.fulfillment,
            payment_events: v.payment_events,
            transfers: v.transfers,
        }
    }
}

fn list<T, U: From<T>>(page: Page<T>) -> Json<ListResponse<U>> {
    Json(ListResponse {
        items: page.items.into_iter().map(U::from).collect(),
        next_before: page.next_before,
    })
}

pub async fn overview(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
) -> Result<Json<crate::metrics::OverviewResponse>, ApiError> {
    Ok(Json(crate::metrics::OverviewResponse::from(
        s.operator_reads.overview(&a.0).await?,
    )))
}
pub async fn conflicts(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ListResponse<ConflictResponse>>, ApiError> {
    Ok(list::<_, ConflictResponse>(
        s.operator_reads.conflicts(&a.0, q.limit, q.before).await?,
    ))
}
pub async fn unmatched(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ListResponse<TransferResponse>>, ApiError> {
    Ok(list::<_, TransferResponse>(
        s.operator_reads
            .unmatched_transfers(&a.0, q.limit, q.before)
            .await?,
    ))
}
pub async fn held(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ListResponse<HeldResponse>>, ApiError> {
    Ok(list::<_, HeldResponse>(
        s.operator_reads
            .held_payments(&a.0, q.limit, q.before)
            .await?,
    ))
}
pub async fn dead(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ListResponse<DeadResponse>>, ApiError> {
    Ok(list::<_, DeadResponse>(
        s.operator_reads
            .dead_letters(&a.0, q.limit, q.before)
            .await?,
    ))
}
pub async fn runs(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ListResponse<RunResponse>>, ApiError> {
    Ok(list::<_, RunResponse>(
        s.operator_reads
            .reconciliation_runs(&a.0, q.limit, q.before)
            .await?,
    ))
}
pub async fn discrepancies(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ListResponse<DiscrepancyResponse>>, ApiError> {
    Ok(list::<_, DiscrepancyResponse>(
        s.operator_reads
            .open_discrepancies(&a.0, q.limit, q.before)
            .await?,
    ))
}
pub async fn evidence(
    State(s): State<AppState>,
    Extension(a): Extension<OperatorAuth>,
    Path(id): Path<Uuid>,
) -> Result<Json<EvidenceResponse>, ApiError> {
    s.operator_reads
        .payment_intent_evidence(&a.0, id)
        .await?
        .map(EvidenceResponse::from)
        .map(Json)
        .ok_or(ApiError::PaymentIntentNotFound)
}
