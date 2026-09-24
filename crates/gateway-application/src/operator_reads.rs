//! Read-only evidence for people operating the gateway.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::RawAmount;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    ComponentStatus, OperationsError, OperatorCredential, OperatorScope, RailStop, RepositoryError,
};

pub const DEFAULT_PAGE_LIMIT: u32 = 50;
pub const MAX_PAGE_LIMIT: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRequest {
    pub limit: u32,
    pub before: Option<Uuid>,
}

impl PageRequest {
    /// Builds a bounded keyset request.
    ///
    /// # Errors
    /// Returns [`OperationsError::InvalidPageLimit`] when `limit` is zero.
    pub fn new(limit: Option<u32>, before: Option<Uuid>) -> Result<Self, OperationsError> {
        let requested = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
        if requested == 0 {
            return Err(OperationsError::InvalidPageLimit);
        }
        Ok(Self {
            limit: requested.min(MAX_PAGE_LIMIT),
            before,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_before: Option<Uuid>,
}

impl<T: Identified> Page<T> {
    fn from_items(mut items: Vec<T>, limit: u32) -> Self {
        // Repositories read one sentinel row past the public limit. That makes
        // the cursor evidence of another page instead of a guess from fullness.
        let has_more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_before = has_more.then(|| items.last().map(Identified::id)).flatten();
        Self { items, next_before }
    }
}

pub trait Identified {
    fn id(&self) -> Uuid;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationRunSummary {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscrepancyAggregate {
    pub kind: String,
    pub money_affected: bool,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCount {
    pub state: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub components: Vec<ComponentStatus>,
    pub open_rail_stops: Vec<RailStop>,
    pub latest_reconciliation: Vec<ReconciliationRunSummary>,
    pub open_discrepancies: Vec<DiscrepancyAggregate>,
    pub transfers_by_processing_state: Vec<StateCount>,
    pub payment_intents_by_status: Vec<StateCount>,
    pub outbox_pending: u64,
    pub outbox_dead_lettered: u64,
    pub observation_conflicts_open: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictItem {
    pub observation_id: Uuid,
    pub field_value: Value,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationConflict {
    pub id: Uuid,
    pub chain: String,
    pub network: String,
    pub environment: String,
    pub tx_hash: String,
    pub event_index: i32,
    pub field: String,
    pub created_at: OffsetDateTime,
    pub items: Vec<ConflictItem>,
}
impl Identified for ObservationConflict {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmatchedTransfer {
    pub id: Uuid,
    pub chain: String,
    pub network: String,
    pub environment: String,
    pub tx_hash: String,
    pub event_index: i32,
    pub collector_address: String,
    pub asset_id: Uuid,
    pub amount_raw: RawAmount,
    pub block_time: OffsetDateTime,
    pub finality_state: String,
    pub unmatched_at: OffsetDateTime,
}
impl Identified for UnmatchedTransfer {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementDecisionSummary {
    pub outcome: String,
    pub risk_decision: String,
    pub decided_reason: Option<String>,
    pub decided_at: OffsetDateTime,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldPayment {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub transfer_ids: Vec<Uuid>,
    pub latest_decision: Option<SettlementDecisionSummary>,
    pub created_at: OffsetDateTime,
}
impl Identified for HeldPayment {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDeliverySummary {
    pub attempt: i32,
    pub response_status: Option<i32>,
    pub error: Option<String>,
    pub delivered_at: OffsetDateTime,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetter {
    pub id: Uuid,
    pub merchant_id: Option<Uuid>,
    pub event_type: String,
    pub channel: String,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub dead_lettered_at: OffsetDateTime,
    pub deliveries: Vec<WebhookDeliverySummary>,
}
impl Identified for DeadLetter {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationRun {
    pub id: Uuid,
    pub kind: String,
    pub window_start: OffsetDateTime,
    pub window_end: OffsetDateTime,
    pub transfers_examined: i32,
    pub intents_examined: i32,
    pub discrepancy_count: i32,
    pub money_discrepancy_count: i32,
    pub status: String,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}
impl Identified for ReconciliationRun {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationDiscrepancy {
    pub id: Uuid,
    pub run_id: Uuid,
    pub kind: String,
    pub money_affected: bool,
    pub transfer_id: Option<Uuid>,
    pub payment_intent_id: Option<Uuid>,
    pub asset_id: Option<Uuid>,
    pub detail: Value,
    pub created_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
    pub resolved_by: Option<String>,
    pub resolution: Option<String>,
}
impl Identified for ReconciliationDiscrepancy {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaymentIntentEvidence {
    pub intent: EvidenceIntent,
    pub attempts: Vec<EvidenceAttempt>,
    pub allocations: Vec<EvidenceAllocation>,
    pub settlement_decisions: Vec<EvidenceSettlementDecision>,
    pub fulfillment: Option<EvidenceFulfillment>,
    pub payment_events: Vec<EvidencePaymentEvent>,
    pub transfers: Vec<EvidenceTransfer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinorUnits(pub i64);

impl Serialize for MinorUnits {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceIntent {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub amount_minor: MinorUnits,
    pub currency: String,
    pub status: String,
    pub reference: String,
    pub description: Option<String>,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceAttempt {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub payment_intent_id: Uuid,
    pub quote_id: Uuid,
    pub collector_address_id: Uuid,
    pub expected_amount_raw: RawAmount,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub quote_expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub late_payment_until: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub quotes: Vec<EvidenceQuote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceQuote {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub payment_intent_id: Uuid,
    pub asset_id: Uuid,
    pub collector_address_id: Uuid,
    pub fiat_currency: String,
    pub fiat_amount_minor: MinorUnits,
    pub base_amount_raw: RawAmount,
    pub amount_raw: RawAmount,
    pub rate_numerator: RawAmount,
    pub rate_denominator: RawAmount,
    pub price_sources: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub price_observed_at: OffsetDateTime,
    pub policy_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub rail_health_observed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub late_payment_until: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceAllocation {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub payment_intent_id: Uuid,
    pub merchant_id: Uuid,
    pub transfer_id: Uuid,
    pub allocated_raw: RawAmount,
    pub allocated_by: String,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSettlementDecision {
    pub id: Uuid,
    pub payment_intent_id: Uuid,
    pub attempt_id: Uuid,
    pub transfer_id: Uuid,
    pub merchant_id: Uuid,
    pub fiat_amount_minor: MinorUnits,
    pub required_policy: String,
    pub distinct_groups: i32,
    pub had_own_node: bool,
    pub finality_state: String,
    pub risk_decision: String,
    pub risk_evaluation_id: Option<Uuid>,
    pub attestation_ids: Vec<Uuid>,
    pub match_strategy: String,
    pub allocated_raw: RawAmount,
    pub remainder_raw: RawAmount,
    pub outcome: String,
    pub decided_by: String,
    pub decided_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceFulfillment {
    pub payment_intent_id: Uuid,
    pub merchant_id: Uuid,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub claimed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub fulfilled_at: Option<OffsetDateTime>,
    pub attempts: i32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidencePaymentEvent {
    pub id: Uuid,
    pub merchant_id: Option<Uuid>,
    pub payment_intent_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub transfer_id: Option<Uuid>,
    pub event_type: String,
    pub previous_status: Option<String>,
    pub new_status: Option<String>,
    pub reason_code: Option<String>,
    pub source: String,
    pub actor: Option<String>,
    pub payload: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceTransfer {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub collector_address_id: Uuid,
    pub chain: String,
    pub network: String,
    pub environment: String,
    pub tx_hash: String,
    pub event_index: i32,
    pub block_number: i64,
    pub block_hash: String,
    #[serde(with = "time::serde::rfc3339")]
    pub block_time: OffsetDateTime,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: RawAmount,
    pub decimals: i16,
    pub memo: Option<String>,
    pub canonicalization_policy: String,
    pub verifier_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub canonicalized_at: OffsetDateTime,
    pub current_state: String,
    pub attestation_count: u64,
}

#[async_trait]
pub trait OperatorReadRepository: Send + Sync {
    async fn overview(&self) -> Result<Overview, RepositoryError>;
    async fn conflicts(
        &self,
        page: PageRequest,
    ) -> Result<Vec<ObservationConflict>, RepositoryError>;
    async fn unmatched_transfers(
        &self,
        page: PageRequest,
    ) -> Result<Vec<UnmatchedTransfer>, RepositoryError>;
    async fn held_payments(&self, page: PageRequest) -> Result<Vec<HeldPayment>, RepositoryError>;
    async fn dead_letters(&self, page: PageRequest) -> Result<Vec<DeadLetter>, RepositoryError>;
    async fn reconciliation_runs(
        &self,
        page: PageRequest,
    ) -> Result<Vec<ReconciliationRun>, RepositoryError>;
    async fn open_discrepancies(
        &self,
        page: PageRequest,
    ) -> Result<Vec<ReconciliationDiscrepancy>, RepositoryError>;
    async fn payment_intent_evidence(
        &self,
        intent_id: Uuid,
    ) -> Result<Option<PaymentIntentEvidence>, RepositoryError>;
}

#[derive(Debug)]
pub struct OperatorReadService<R> {
    repository: Arc<R>,
}

impl<R: OperatorReadRepository> OperatorReadService<R> {
    pub const fn new(repository: Arc<R>) -> Self {
        Self { repository }
    }
    fn require(credential: &OperatorCredential) -> Result<(), OperationsError> {
        if credential.allows(OperatorScope::Read) {
            Ok(())
        } else {
            Err(OperationsError::MissingScope(OperatorScope::Read))
        }
    }
    /// Reads the operator overview.
    /// # Errors
    /// Returns an error when scope validation or storage fails.
    pub async fn overview(
        &self,
        credential: &OperatorCredential,
    ) -> Result<Overview, OperationsError> {
        Self::require(credential)?;
        Ok(self.repository.overview().await?)
    }
    /// Reads open observation conflicts.
    /// # Errors
    /// Returns an error when scope, pagination, or storage validation fails.
    pub async fn conflicts(
        &self,
        credential: &OperatorCredential,
        limit: Option<u32>,
        before: Option<Uuid>,
    ) -> Result<Page<ObservationConflict>, OperationsError> {
        Self::require(credential)?;
        let page = PageRequest::new(limit, before)?;
        Ok(Page::from_items(
            self.repository.conflicts(page).await?,
            page.limit,
        ))
    }
    /// Reads unmatched transfers.
    /// # Errors
    /// Returns an error when scope, pagination, or storage validation fails.
    pub async fn unmatched_transfers(
        &self,
        credential: &OperatorCredential,
        limit: Option<u32>,
        before: Option<Uuid>,
    ) -> Result<Page<UnmatchedTransfer>, OperationsError> {
        Self::require(credential)?;
        let page = PageRequest::new(limit, before)?;
        Ok(Page::from_items(
            self.repository.unmatched_transfers(page).await?,
            page.limit,
        ))
    }
    /// Reads held payments.
    /// # Errors
    /// Returns an error when scope, pagination, or storage validation fails.
    pub async fn held_payments(
        &self,
        credential: &OperatorCredential,
        limit: Option<u32>,
        before: Option<Uuid>,
    ) -> Result<Page<HeldPayment>, OperationsError> {
        Self::require(credential)?;
        let page = PageRequest::new(limit, before)?;
        Ok(Page::from_items(
            self.repository.held_payments(page).await?,
            page.limit,
        ))
    }
    /// Reads dead-lettered events.
    /// # Errors
    /// Returns an error when scope, pagination, or storage validation fails.
    pub async fn dead_letters(
        &self,
        credential: &OperatorCredential,
        limit: Option<u32>,
        before: Option<Uuid>,
    ) -> Result<Page<DeadLetter>, OperationsError> {
        Self::require(credential)?;
        let page = PageRequest::new(limit, before)?;
        Ok(Page::from_items(
            self.repository.dead_letters(page).await?,
            page.limit,
        ))
    }
    /// Reads reconciliation runs.
    /// # Errors
    /// Returns an error when scope, pagination, or storage validation fails.
    pub async fn reconciliation_runs(
        &self,
        credential: &OperatorCredential,
        limit: Option<u32>,
        before: Option<Uuid>,
    ) -> Result<Page<ReconciliationRun>, OperationsError> {
        Self::require(credential)?;
        let page = PageRequest::new(limit, before)?;
        Ok(Page::from_items(
            self.repository.reconciliation_runs(page).await?,
            page.limit,
        ))
    }
    /// Reads unresolved reconciliation discrepancies.
    /// # Errors
    /// Returns an error when scope, pagination, or storage validation fails.
    pub async fn open_discrepancies(
        &self,
        credential: &OperatorCredential,
        limit: Option<u32>,
        before: Option<Uuid>,
    ) -> Result<Page<ReconciliationDiscrepancy>, OperationsError> {
        Self::require(credential)?;
        let page = PageRequest::new(limit, before)?;
        Ok(Page::from_items(
            self.repository.open_discrepancies(page).await?,
            page.limit,
        ))
    }
    /// Reads the complete evidence bundle for one intent.
    /// # Errors
    /// Returns an error when scope validation or storage fails.
    pub async fn payment_intent_evidence(
        &self,
        credential: &OperatorCredential,
        intent_id: Uuid,
    ) -> Result<Option<PaymentIntentEvidence>, OperationsError> {
        Self::require(credential)?;
        Ok(self.repository.payment_intent_evidence(intent_id).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo;
    #[async_trait]
    impl OperatorReadRepository for Repo {
        async fn overview(&self) -> Result<Overview, RepositoryError> {
            Ok(Overview {
                components: vec![],
                open_rail_stops: vec![],
                latest_reconciliation: vec![],
                open_discrepancies: vec![],
                transfers_by_processing_state: vec![],
                payment_intents_by_status: vec![],
                outbox_pending: 0,
                outbox_dead_lettered: 0,
                observation_conflicts_open: 0,
            })
        }
        async fn conflicts(
            &self,
            _: PageRequest,
        ) -> Result<Vec<ObservationConflict>, RepositoryError> {
            Ok(vec![])
        }
        async fn unmatched_transfers(
            &self,
            _: PageRequest,
        ) -> Result<Vec<UnmatchedTransfer>, RepositoryError> {
            Ok(vec![])
        }
        async fn held_payments(&self, _: PageRequest) -> Result<Vec<HeldPayment>, RepositoryError> {
            Ok(vec![])
        }
        async fn dead_letters(&self, _: PageRequest) -> Result<Vec<DeadLetter>, RepositoryError> {
            Ok(vec![])
        }
        async fn reconciliation_runs(
            &self,
            _: PageRequest,
        ) -> Result<Vec<ReconciliationRun>, RepositoryError> {
            Ok(vec![])
        }
        async fn open_discrepancies(
            &self,
            _: PageRequest,
        ) -> Result<Vec<ReconciliationDiscrepancy>, RepositoryError> {
            Ok(vec![])
        }
        async fn payment_intent_evidence(
            &self,
            _: Uuid,
        ) -> Result<Option<PaymentIntentEvidence>, RepositoryError> {
            Ok(None)
        }
    }
    fn credential(scopes: Vec<OperatorScope>) -> OperatorCredential {
        OperatorCredential {
            key_id: Uuid::nil(),
            label: "test".into(),
            scopes,
        }
    }

    #[tokio::test]
    async fn every_read_refuses_a_key_without_read_scope() {
        let s = OperatorReadService::new(Arc::new(Repo));
        let c = credential(vec![OperatorScope::Ingest]);
        assert!(matches!(
            s.overview(&c).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.conflicts(&c, None, None).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.unmatched_transfers(&c, None, None).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.held_payments(&c, None, None).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.dead_letters(&c, None, None).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.reconciliation_runs(&c, None, None).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.open_discrepancies(&c, None, None).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
        assert!(matches!(
            s.payment_intent_evidence(&c, Uuid::nil()).await,
            Err(OperationsError::MissingScope(OperatorScope::Read))
        ));
    }

    #[test]
    fn page_limits_default_clamp_and_refuse_zero() {
        assert!(matches!(
            PageRequest::new(None, None),
            Ok(PageRequest { limit: 50, .. })
        ));
        assert!(matches!(
            PageRequest::new(Some(999), None),
            Ok(PageRequest { limit: 200, .. })
        ));
        assert!(matches!(
            PageRequest::new(Some(0), None),
            Err(OperationsError::InvalidPageLimit)
        ));
    }
}
