//! The operator's side of the gateway.
//!
//! Merchants buy; operators feed the gateway the evidence it prices with and
//! close a rail when something stops adding up. The two never share a
//! credential, and an operator key carries only the scopes its holder needs.
//!
//! Everything here is append-only. A price snapshot is a decision about
//! several readings, and the readings are kept whether or not they counted, so
//! a disputed conversion is answerable a year later and a source that drifts
//! is visible before it is trusted again.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{
    AggregatedPrice, CurrencyCode, ManualResolutionAction, PriceAggregationError,
    PriceAggregationPolicy, PriceDiscardReason, PriceReading, RailHealth, RawAmount,
    RemainderDisposition,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Clock, RepositoryError};

/// What an operator key is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperatorScope {
    /// Feed the gateway evidence: prices, rail health, risk decisions.
    Ingest,
    /// Submit KYT evidence for a provider explicitly bound to this key.
    RiskIngest,
    /// See the operator views: conflicts, unmatched money, held payments.
    Read,
    /// Close and reopen a rail.
    Admin,
}

impl OperatorScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::RiskIngest => "risk_ingest",
            Self::Read => "read",
            Self::Admin => "admin",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError::UnknownScope`] for text that is not a scope.
    pub fn parse(value: &str) -> Result<Self, OperationsError> {
        match value {
            "ingest" => Ok(Self::Ingest),
            "risk_ingest" => Ok(Self::RiskIngest),
            "read" => Ok(Self::Read),
            "admin" => Ok(Self::Admin),
            other => Err(OperationsError::UnknownScope(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorCredential {
    pub key_id: Uuid,
    pub label: String,
    pub scopes: Vec<OperatorScope>,
}

impl OperatorCredential {
    /// Reports whether this key carries a scope.
    ///
    /// Absence closes the door: a key with no scopes can do nothing, and an
    /// unknown scope never becomes a permissive one.
    #[must_use]
    pub fn allows(&self, scope: OperatorScope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// What one price submission did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPrice {
    pub snapshot_id: Uuid,
    pub rate_numerator: String,
    pub rate_denominator: String,
    pub group_count: u32,
    pub deviation_bps: u32,
    pub observed_at: OffsetDateTime,
}

/// The readings behind one submission, and what became of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceIngestion {
    pub asset_id: Uuid,
    pub currency: CurrencyCode,
    pub readings: Vec<PriceReading>,
    pub outcome: PriceOutcome,
    pub ingested_by: Uuid,
    pub received_at: OffsetDateTime,
}

/// Either a rate, or the recorded reason there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceOutcome {
    Aggregated(AggregatedPrice),
    /// Nothing was priced. Every reading is stored with this reason, so the
    /// disagreement is a record rather than a gap in the trail.
    Refused(String),
}

/// A rail closed by a person or by reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailStop {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub reason_code: String,
    pub detail: Option<String>,
    pub opened_by: String,
    pub opened_at: OffsetDateTime,
}

/// What a screening provider said about the source of one transfer's funds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskSubmission {
    pub transfer_id: Uuid,
    pub provider: String,
    pub decision: gateway_domain::RiskDecision,
    pub score: Option<i32>,
    pub reasons: serde_json::Value,
    pub evaluated_at: OffsetDateTime,
}

/// One explicit operator decision about money the automatic path parked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualResolution {
    pub action: ManualResolutionAction,
    pub transfer_id: Uuid,
    pub payment_intent_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub allocate_raw: Option<RawAmount>,
    pub remainder_raw: Option<RawAmount>,
    pub disposition: Option<RemainderDisposition>,
    pub external_reference: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualResolutionResult {
    pub id: Uuid,
    pub action: ManualResolutionAction,
    pub transfer_id: Uuid,
    pub payment_intent_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub merchant_id: Option<Uuid>,
    pub allocated_raw: Option<RawAmount>,
    pub remainder_raw: Option<RawAmount>,
    pub replayed: bool,
}

#[async_trait]
pub trait OperationsRepository: Send + Sync {
    async fn authenticate_operator_key(
        &self,
        secret_hash: &[u8; 32],
    ) -> Result<Option<OperatorCredential>, RepositoryError>;

    /// The active quote policy, which is also the policy a price must satisfy.
    async fn price_policy(
        &self,
        asset_id: Uuid,
        currency: &CurrencyCode,
    ) -> Result<Option<PriceAggregationPolicy>, RepositoryError>;

    /// Stores one submission: the snapshot when there is one, and every
    /// reading with its outcome, in a single transaction.
    async fn record_price_ingestion(
        &self,
        ingestion: &PriceIngestion,
    ) -> Result<Option<RecordedPrice>, RepositoryError>;

    async fn record_rail_health(
        &self,
        asset_id: Uuid,
        health: RailHealth,
        detail: Option<String>,
        ingested_by: Uuid,
        observed_at: OffsetDateTime,
    ) -> Result<Uuid, RepositoryError>;

    async fn open_rail_stop(
        &self,
        asset_id: Uuid,
        reason_code: &str,
        detail: Option<&str>,
        opened_by: &str,
        opened_at: OffsetDateTime,
    ) -> Result<RailStop, RepositoryError>;

    async fn clear_rail_stop(
        &self,
        asset_id: Uuid,
        cleared_by: &str,
        reason: &str,
        cleared_at: OffsetDateTime,
    ) -> Result<bool, RepositoryError>;

    async fn find_open_rail_stop(
        &self,
        asset_id: Uuid,
    ) -> Result<Option<RailStop>, RepositoryError>;

    async fn record_risk_evaluation(
        &self,
        submission: &RiskSubmission,
        submitted_by: Uuid,
    ) -> Result<Uuid, RepositoryError>;

    async fn risk_provider_allowed(
        &self,
        operator_key_id: Uuid,
        provider: &str,
    ) -> Result<bool, RepositoryError>;

    /// Applies one admin decision and its audit/outbox effects atomically.
    async fn resolve_manual(
        &self,
        credential: &OperatorCredential,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        resolution: &ManualResolution,
        decided_at: OffsetDateTime,
    ) -> Result<ManualResolutionResult, OperationsError>;
}

/// The operator-facing application service.
#[derive(Debug)]
pub struct OperationsService<R, C> {
    repository: Arc<R>,
    clock: C,
}

impl<R, C> OperationsService<R, C>
where
    R: OperationsRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C) -> Self {
        Self { repository, clock }
    }

    /// Aggregates submitted readings into one immutable price snapshot.
    ///
    /// Refusing is a recorded outcome, not an empty response: when the sources
    /// disagree or too few of them are independent, every reading is stored
    /// with the reason, the previous snapshot is left alone, and new quotes
    /// close as it ages out.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the key lacks the scope, no policy
    /// governs the pair, the evidence does not satisfy the policy, or storage
    /// fails.
    pub async fn submit_price(
        &self,
        credential: &OperatorCredential,
        asset_id: Uuid,
        currency: &CurrencyCode,
        readings: Vec<PriceReading>,
    ) -> Result<RecordedPrice, OperationsError> {
        Self::require(credential, OperatorScope::Ingest)?;
        if readings.is_empty() {
            return Err(OperationsError::NoReadings);
        }
        let policy = self
            .repository
            .price_policy(asset_id, currency)
            .await?
            .ok_or(OperationsError::NoPricePolicy)?;
        let now = self.clock.now();

        match gateway_domain::aggregate(&readings, policy, now) {
            Ok(aggregated) => {
                let ingestion = PriceIngestion {
                    asset_id,
                    currency: currency.clone(),
                    readings,
                    outcome: PriceOutcome::Aggregated(aggregated),
                    ingested_by: credential.key_id,
                    received_at: now,
                };
                self.repository
                    .record_price_ingestion(&ingestion)
                    .await?
                    .ok_or(OperationsError::SnapshotNotRecorded)
            }
            Err(error) => {
                let ingestion = PriceIngestion {
                    asset_id,
                    currency: currency.clone(),
                    readings,
                    outcome: PriceOutcome::Refused(refusal_code(error).to_owned()),
                    ingested_by: credential.key_id,
                    received_at: now,
                };
                self.repository.record_price_ingestion(&ingestion).await?;
                Err(OperationsError::Price(error))
            }
        }
    }

    /// Records what a rail's health is right now.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the key lacks the scope or storage
    /// fails.
    pub async fn submit_rail_health(
        &self,
        credential: &OperatorCredential,
        asset_id: Uuid,
        health: RailHealth,
        detail: Option<String>,
    ) -> Result<Uuid, OperationsError> {
        Self::require(credential, OperatorScope::Ingest)?;
        Ok(self
            .repository
            .record_rail_health(
                asset_id,
                health,
                detail,
                credential.key_id,
                self.clock.now(),
            )
            .await?)
    }

    /// Closes a rail. New quotes stop; quotes already issued stay payable,
    /// because a stop is a decision about future obligations, not a way to
    /// forget the ones already made.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the key lacks the scope or storage
    /// fails.
    pub async fn open_rail_stop(
        &self,
        credential: &OperatorCredential,
        asset_id: Uuid,
        reason_code: &str,
        detail: Option<&str>,
    ) -> Result<RailStop, OperationsError> {
        Self::require(credential, OperatorScope::Admin)?;
        if reason_code.trim().is_empty() {
            return Err(OperationsError::ReasonRequired);
        }
        Ok(self
            .repository
            .open_rail_stop(
                asset_id,
                reason_code,
                detail,
                &credential.label,
                self.clock.now(),
            )
            .await?)
    }

    /// Reopens a rail. The reason is required: the thing that noticed a
    /// discrepancy is never the thing that can say it was explained.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the key lacks the scope, no reason was
    /// given, no stop is open, or storage fails.
    pub async fn clear_rail_stop(
        &self,
        credential: &OperatorCredential,
        asset_id: Uuid,
        reason: &str,
    ) -> Result<(), OperationsError> {
        Self::require(credential, OperatorScope::Admin)?;
        if reason.trim().is_empty() {
            return Err(OperationsError::ReasonRequired);
        }
        let cleared = self
            .repository
            .clear_rail_stop(asset_id, &credential.label, reason, self.clock.now())
            .await?;
        if cleared {
            Ok(())
        } else {
            Err(OperationsError::NoOpenRailStop)
        }
    }

    /// Records a screening decision about one transfer.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the key lacks the scope or storage
    /// fails.
    pub async fn submit_risk_evaluation(
        &self,
        credential: &OperatorCredential,
        submission: &RiskSubmission,
    ) -> Result<Uuid, OperationsError> {
        Self::require(credential, OperatorScope::RiskIngest)?;
        let provider = submission.provider.trim();
        if provider.is_empty() || provider.len() > 100 || provider != submission.provider {
            return Err(OperationsError::InvalidRiskEvaluation);
        }
        let now = self.clock.now();
        let earliest = now
            .checked_sub(time::Duration::hours(1))
            .ok_or(OperationsError::InvalidRiskEvaluation)?;
        let latest = now
            .checked_add(time::Duration::seconds(30))
            .ok_or(OperationsError::InvalidRiskEvaluation)?;
        if submission.evaluated_at < earliest || submission.evaluated_at > latest {
            return Err(OperationsError::InvalidRiskEvaluation);
        }
        if !self
            .repository
            .risk_provider_allowed(credential.key_id, provider)
            .await?
        {
            return Err(OperationsError::RiskProviderNotAllowed);
        }
        Ok(self
            .repository
            .record_risk_evaluation(submission, credential.key_id)
            .await?)
    }

    /// Resolves parked money without bypassing settlement allocation or
    /// fulfillment primitives.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError`] when the credential lacks `admin`, the
    /// command or idempotency key is malformed, the target is not eligible for
    /// the requested transition, or the atomic repository transaction fails.
    pub async fn resolve_manual(
        &self,
        credential: &OperatorCredential,
        idempotency_key: &str,
        resolution: &ManualResolution,
    ) -> Result<ManualResolutionResult, OperationsError> {
        Self::require(credential, OperatorScope::Admin)?;
        let valid_key = (16..=128).contains(&idempotency_key.len())
            && idempotency_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !valid_key {
            return Err(OperationsError::InvalidIdempotencyKey);
        }
        if !(1..=1000).contains(&resolution.reason.trim().len()) {
            return Err(OperationsError::ReasonRequired);
        }
        validate_manual_resolution(resolution)?;
        let request_hash = manual_resolution_hash(resolution);
        self.repository
            .resolve_manual(
                credential,
                idempotency_key,
                &request_hash,
                resolution,
                self.clock.now(),
            )
            .await
    }

    /// Checks one scope. Absence closes the door; nothing here widens a key.
    fn require(
        credential: &OperatorCredential,
        scope: OperatorScope,
    ) -> Result<(), OperationsError> {
        if credential.allows(scope) {
            Ok(())
        } else {
            Err(OperationsError::MissingScope(scope))
        }
    }
}

/// The short, stable name a refusal is stored under.
const fn refusal_code(error: PriceAggregationError) -> &'static str {
    match error {
        PriceAggregationError::PolicyDemandsTooFewSources => "policy_demands_too_few_sources",
        PriceAggregationError::NoReadings => "no_readings",
        PriceAggregationError::NotEnoughSources { .. } => "not_enough_sources",
        PriceAggregationError::Diverged { .. } => "diverged",
        PriceAggregationError::Overflow => "overflow",
    }
}

/// The reason one reading did not count, as stored.
#[must_use]
pub const fn discard_code(reason: PriceDiscardReason) -> &'static str {
    reason.as_str()
}

#[derive(Debug, Error)]
pub enum OperationsError {
    #[error("this operator key does not carry the {} scope", .0.as_str())]
    MissingScope(OperatorScope),
    #[error("page limit must be greater than zero")]
    InvalidPageLimit,
    #[error("unknown operator scope: {0}")]
    UnknownScope(String),
    #[error("a price submission carries at least one reading")]
    NoReadings,
    #[error("no active quote policy governs this asset and currency")]
    NoPricePolicy,
    #[error("the snapshot was not written")]
    SnapshotNotRecorded,
    #[error("no rail stop is open for this asset")]
    NoOpenRailStop,
    #[error("a reason is required and recorded")]
    ReasonRequired,
    #[error("the idempotency key must contain between 16 and 128 characters")]
    InvalidIdempotencyKey,
    #[error("the manual-resolution fields do not match its action")]
    InvalidManualResolution,
    #[error("the transfer, intent or attempt is not eligible for this manual decision")]
    ManualResolutionConflict,
    #[error("the transfer, intent or attempt was not found")]
    ManualResolutionNotFound,
    #[error("the risk evaluation is stale, future-dated or malformed")]
    InvalidRiskEvaluation,
    #[error("this operator key is not bound to the named risk provider")]
    RiskProviderNotAllowed,
    #[error(transparent)]
    Price(#[from] PriceAggregationError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

fn validate_manual_resolution(resolution: &ManualResolution) -> Result<(), OperationsError> {
    let honor = resolution.payment_intent_id.is_some()
        && resolution.attempt_id.is_some()
        && resolution
            .allocate_raw
            .is_some_and(|amount| !amount.is_zero())
        && resolution.remainder_raw.is_none()
        && resolution.disposition.is_none()
        && resolution.external_reference.is_none();
    let reject = resolution.payment_intent_id.is_none()
        && resolution.attempt_id.is_none()
        && resolution.allocate_raw.is_none()
        && resolution.remainder_raw.is_none()
        && resolution.disposition.is_none()
        && resolution.external_reference.is_none();
    let disposition = resolution.payment_intent_id.is_some()
        && resolution.attempt_id.is_none()
        && resolution.allocate_raw.is_none()
        && resolution
            .remainder_raw
            .is_some_and(|amount| !amount.is_zero())
        && resolution.disposition.is_some()
        && resolution
            .external_reference
            .as_deref()
            .is_some_and(|reference| (1..=200).contains(&reference.trim().len()));
    let valid = match resolution.action {
        ManualResolutionAction::Honor => honor,
        ManualResolutionAction::Reject => reject,
        ManualResolutionAction::RecordRemainderDisposition => disposition,
    };
    if valid {
        Ok(())
    } else {
        Err(OperationsError::InvalidManualResolution)
    }
}

fn manual_resolution_hash(resolution: &ManualResolution) -> [u8; 32] {
    let mut digest = Sha256::new();
    for value in [
        resolution.action.as_str().to_owned(),
        resolution.transfer_id.to_string(),
        resolution
            .payment_intent_id
            .map(|v| v.to_string())
            .unwrap_or_default(),
        resolution
            .attempt_id
            .map(|v| v.to_string())
            .unwrap_or_default(),
        resolution
            .allocate_raw
            .map(|v| v.to_string())
            .unwrap_or_default(),
        resolution
            .remainder_raw
            .map(|v| v.to_string())
            .unwrap_or_default(),
        resolution
            .disposition
            .map(|v| v.as_str().to_owned())
            .unwrap_or_default(),
        resolution.external_reference.clone().unwrap_or_default(),
        resolution.reason.clone(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.finalize().into()
}

impl OperationsError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
