use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{
    AttemptCandidate, AttemptStatus, CurrencyCode, MatchOutcome, MatchStrategy, RawAmount,
    RiskDecision, SettlementError, SettlementEvidence, SettlementOutcome, SettlementPolicy,
    TransferFacts, TransferState, decide_settlement, match_transfer,
};
use thiserror::Error;
use uuid::Uuid;

use crate::{Clock, ComponentLease, RepositoryError};

/// A canonical transfer waiting for the money path, with the evidence that
/// made it canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTransfer {
    pub facts: TransferFacts,
    pub independent_groups: u32,
    pub had_own_node: bool,
    pub attestation_ids: Vec<Uuid>,
    pub allocated_raw: RawAmount,
}

/// What the obligation behind one attempt currently looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptSnapshot {
    pub attempt_id: Uuid,
    pub payment_intent_id: Uuid,
    pub merchant_id: Uuid,
    pub currency: CurrencyCode,
    pub fiat_amount_minor: i64,
    pub expected_amount_raw: RawAmount,
    pub allocated_raw: RawAmount,
    pub status: AttemptStatus,
}

/// Everything one settlement transaction needs. The decision is already made;
/// storage enforces the invariants and records what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementCommand {
    pub transfer_id: Uuid,
    pub attempt_id: Uuid,
    pub payment_intent_id: Uuid,
    pub merchant_id: Uuid,
    pub fiat_amount_minor: i64,
    pub match_strategy: MatchStrategy,
    pub outcome: SettlementOutcome,
    pub policy_version: String,
    pub independent_groups: u32,
    pub had_own_node: bool,
    pub finality_state: TransferState,
    pub risk: RiskDecision,
    pub risk_evaluation_id: Option<Uuid>,
    pub attestation_ids: Vec<Uuid>,
}

/// What the settlement transaction actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementRecord {
    /// The obligation is covered, the claim to fulfil is held, and the outbox
    /// carries the merchant's event.
    Settled,
    /// Money was allocated but the obligation is not covered yet.
    PartiallyAllocated,
    /// Money was allocated and a remainder is waiting for a person.
    Overpaid,
    /// Nothing moved; the reason is recorded.
    Held,
    /// A person must decide, with the evidence recorded.
    ManualRequired,
    /// This transfer already belongs to another payment intent.
    ForeignClaim,
    /// This transfer was already processed; nothing changed.
    AlreadyProcessed,
}

/// Why a transfer could not be tied to an obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedTransfer {
    /// More than one attempt fits. Recorded, alerted, decided by a person.
    Ambiguous { attempt_ids: Vec<Uuid> },
    /// Money arrived that no obligation explains.
    Unmatched,
}

impl UnresolvedTransfer {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Ambiguous { .. } => "ambiguous",
            Self::Unmatched => "unmatched",
        }
    }
}

#[async_trait]
pub trait SettlementRepository: Send + Sync {
    /// Canonical transfers whose money has not been decided yet.
    async fn transfers_awaiting_settlement(
        &self,
        limit: u32,
    ) -> Result<Vec<PendingTransfer>, RepositoryError>;

    /// Attempts whose reserved amount or memo could explain this transfer,
    /// including attempts whose reservation has already been archived.
    async fn match_candidates(
        &self,
        transfer: &TransferFacts,
    ) -> Result<Vec<AttemptCandidate>, RepositoryError>;

    async fn attempt_snapshot(
        &self,
        attempt_id: Uuid,
    ) -> Result<Option<AttemptSnapshot>, RepositoryError>;

    async fn find_settlement_policy(
        &self,
        currency: &CurrencyCode,
    ) -> Result<Option<SettlementPolicy>, RepositoryError>;

    /// The newest screening of this transfer's source of funds.
    async fn latest_risk(
        &self,
        transfer_id: Uuid,
    ) -> Result<(RiskDecision, Option<Uuid>), RepositoryError>;

    /// Claim, allocation, status, fulfilment claim, decision, events and
    /// outbox in one transaction.
    async fn settle(
        &self,
        lease: &ComponentLease,
        command: &SettlementCommand,
    ) -> Result<SettlementRecord, RepositoryError>;

    /// Records a transfer nobody can claim, so it is visible instead of lost.
    async fn record_unresolved(
        &self,
        lease: &ComponentLease,
        transfer: &PendingTransfer,
        unresolved: &UnresolvedTransfer,
    ) -> Result<(), RepositoryError>;
}

/// Counters for one settlement pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettlementReport {
    pub examined: u32,
    pub settled: u32,
    pub partial: u32,
    pub overpaid: u32,
    pub held: u32,
    pub manual_required: u32,
    pub ambiguous: u32,
    pub unmatched: u32,
    pub foreign_claim: u32,
    pub already_processed: u32,
    pub missing_policy: u32,
}

/// Ties verified money to obligations and lets the database enforce that it
/// can only be spent once.
#[derive(Debug)]
pub struct SettlementService<R, C> {
    repository: Arc<R>,
    clock: C,
}

impl<R, C> SettlementService<R, C>
where
    R: SettlementRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C) -> Self {
        Self { repository, clock }
    }

    /// Processes a bounded batch of verified transfers.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementServiceError`] when the batch limit is invalid,
    /// storage fails, the lease was taken over, or stored amounts cannot be
    /// combined.
    pub async fn settle_pending(
        &self,
        lease: &ComponentLease,
        limit: u32,
    ) -> Result<SettlementReport, SettlementServiceError> {
        if limit == 0 || limit > 1_000 {
            return Err(SettlementServiceError::InvalidBatchLimit);
        }
        let transfers = self.repository.transfers_awaiting_settlement(limit).await?;
        let mut report = SettlementReport::default();

        for transfer in transfers {
            report.examined = report.examined.saturating_add(1);
            let candidates = self.repository.match_candidates(&transfer.facts).await?;
            match match_transfer(&transfer.facts, &candidates) {
                MatchOutcome::Matched {
                    attempt_id,
                    strategy,
                    late,
                    ..
                } => {
                    self.settle_match(lease, &transfer, attempt_id, strategy, late, &mut report)
                        .await?;
                }
                MatchOutcome::Ambiguous { attempt_ids } => {
                    report.ambiguous = report.ambiguous.saturating_add(1);
                    self.repository
                        .record_unresolved(
                            lease,
                            &transfer,
                            &UnresolvedTransfer::Ambiguous { attempt_ids },
                        )
                        .await?;
                }
                MatchOutcome::Unmatched => {
                    report.unmatched = report.unmatched.saturating_add(1);
                    self.repository
                        .record_unresolved(lease, &transfer, &UnresolvedTransfer::Unmatched)
                        .await?;
                }
            }
        }
        Ok(report)
    }

    async fn settle_match(
        &self,
        lease: &ComponentLease,
        transfer: &PendingTransfer,
        attempt_id: Uuid,
        strategy: MatchStrategy,
        late: bool,
        report: &mut SettlementReport,
    ) -> Result<(), SettlementServiceError> {
        let Some(attempt) = self.repository.attempt_snapshot(attempt_id).await? else {
            return Err(SettlementServiceError::Repository(
                RepositoryError::CorruptData(
                    "a matched attempt disappeared between matching and settling".to_owned(),
                ),
            ));
        };
        let Some(policy) = self
            .repository
            .find_settlement_policy(&attempt.currency)
            .await?
        else {
            // No policy means nobody decided what this size of money requires.
            report.missing_policy = report.missing_policy.saturating_add(1);
            return Ok(());
        };
        let (risk, risk_evaluation_id) = self
            .repository
            .latest_risk(transfer.facts.transfer_id)
            .await?;

        let evidence = SettlementEvidence {
            fiat_amount_minor: attempt.fiat_amount_minor,
            expected_amount_raw: attempt.expected_amount_raw,
            already_allocated_raw: attempt.allocated_raw,
            transfer_amount_raw: transfer.facts.amount_raw,
            transfer_state: transfer.facts.state,
            independent_groups: transfer.independent_groups,
            had_own_node: transfer.had_own_node,
            risk,
            attempt_status: attempt.status,
            late,
        };
        let outcome = decide_settlement(&evidence, &policy)?;
        let command = SettlementCommand {
            transfer_id: transfer.facts.transfer_id,
            attempt_id: attempt.attempt_id,
            payment_intent_id: attempt.payment_intent_id,
            merchant_id: attempt.merchant_id,
            fiat_amount_minor: attempt.fiat_amount_minor,
            match_strategy: strategy,
            outcome,
            policy_version: policy.version.clone(),
            independent_groups: transfer.independent_groups,
            had_own_node: transfer.had_own_node,
            finality_state: transfer.facts.state,
            risk,
            risk_evaluation_id,
            attestation_ids: transfer.attestation_ids.clone(),
        };

        match self.repository.settle(lease, &command).await? {
            SettlementRecord::Settled => report.settled = report.settled.saturating_add(1),
            SettlementRecord::PartiallyAllocated => {
                report.partial = report.partial.saturating_add(1);
            }
            SettlementRecord::Overpaid => report.overpaid = report.overpaid.saturating_add(1),
            SettlementRecord::Held => report.held = report.held.saturating_add(1),
            SettlementRecord::ManualRequired => {
                report.manual_required = report.manual_required.saturating_add(1);
            }
            SettlementRecord::ForeignClaim => {
                report.foreign_claim = report.foreign_claim.saturating_add(1);
            }
            SettlementRecord::AlreadyProcessed => {
                report.already_processed = report.already_processed.saturating_add(1);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn clock(&self) -> &C {
        &self.clock
    }
}

#[derive(Debug, Error)]
pub enum SettlementServiceError {
    #[error("settlement batch limit must be between 1 and 1000")]
    InvalidBatchLimit,
    #[error(transparent)]
    Settlement(#[from] SettlementError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl SettlementServiceError {
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
