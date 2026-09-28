use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Memo, RawAmount, TransferState};

/// One payment attempt a transfer could belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptCandidate {
    pub attempt_id: Uuid,
    pub payment_intent_id: Uuid,
    pub merchant_id: Uuid,
    pub collector_address_id: Uuid,
    pub expected_amount_raw: RawAmount,
    pub memo_reference: Option<Memo>,
    /// When the exact-amount slot was reserved for this attempt, and until
    /// when. A slot released after the late-payment window may be reserved
    /// again for someone else, so the window is part of the identity.
    pub leased_from: OffsetDateTime,
    pub leased_until: OffsetDateTime,
    pub status: AttemptStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    AwaitingPayment,
    Expired,
    Cancelled,
    Settled,
}

impl AttemptStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingPayment => "awaiting_payment",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Settled => "settled",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementError::UnknownAttemptStatus`] for unknown text.
    pub fn parse(value: &str) -> Result<Self, SettlementError> {
        match value {
            "awaiting_payment" => Ok(Self::AwaitingPayment),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            "settled" => Ok(Self::Settled),
            other => Err(SettlementError::UnknownAttemptStatus(other.to_owned())),
        }
    }
}

/// What a transfer must look like to be matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferFacts {
    pub transfer_id: Uuid,
    pub collector_address_id: Uuid,
    pub amount_raw: RawAmount,
    pub memo: Option<Memo>,
    /// The block's own time. The observer's clock is irrelevant: it may have
    /// been hours behind when it read this transfer.
    pub block_time: OffsetDateTime,
    pub state: TransferState,
}

/// How a transfer was tied to an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStrategy {
    /// The chain carried the payment reference itself.
    Memo,
    /// The exact amount is reserved for exactly one attempt right now.
    ExactAmount,
    /// The exact amount was reserved for exactly one attempt at the time the
    /// block was produced, even though the slot has since been released.
    HistoricalSlot,
    /// An administrator tied already-finalized money to an obligation after
    /// reviewing the evidence. This never weakens chain finality.
    Manual,
}

impl MatchStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memo => "memo",
            Self::ExactAmount => "exact_amount",
            Self::HistoricalSlot => "historical_slot",
            Self::Manual => "manual",
        }
    }
}

/// The only decisions the manual-resolution boundary accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualResolutionAction {
    Honor,
    Reject,
    RecordRemainderDisposition,
}

impl ManualResolutionAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Honor => "honor",
            Self::Reject => "reject",
            Self::RecordRemainderDisposition => "record_remainder_disposition",
        }
    }
}

/// A remainder was handled outside the gateway. Recording one of these is an
/// audit fact only: it never claims that this service sent an on-chain refund.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemainderDisposition {
    RefundedExternally,
    CreditedExternally,
    DonatedExternally,
    RetainedByAgreement,
}

impl RemainderDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RefundedExternally => "refunded_externally",
            Self::CreditedExternally => "credited_externally",
            Self::DonatedExternally => "donated_externally",
            Self::RetainedByAgreement => "retained_by_agreement",
        }
    }
}

/// The result of matching. There is no "probably this one".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome {
    Matched {
        attempt_id: Uuid,
        payment_intent_id: Uuid,
        strategy: MatchStrategy,
        late: bool,
    },
    /// More than one attempt fits. A human decides; the system must not guess.
    Ambiguous { attempt_ids: Vec<Uuid> },
    /// Money arrived that no open obligation explains. It is recorded and
    /// queued, never discarded.
    Unmatched,
}

/// Ties a transfer to at most one attempt.
///
/// The order is deliberate: an explicit reference beats an amount, a live
/// reservation beats a historical one, and anything ambiguous stops.
#[must_use]
pub fn match_transfer(transfer: &TransferFacts, candidates: &[AttemptCandidate]) -> MatchOutcome {
    let on_address: Vec<&AttemptCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.collector_address_id == transfer.collector_address_id)
        .collect();

    if let Some(memo) = transfer.memo.as_ref() {
        let by_memo: Vec<&AttemptCandidate> = on_address
            .iter()
            .filter(|candidate| candidate.memo_reference.as_ref() == Some(memo))
            .copied()
            .collect();
        match by_memo.as_slice() {
            [single] => return matched(single, MatchStrategy::Memo, transfer),
            [] => {}
            many => return ambiguous(many),
        }
    }

    let live: Vec<&AttemptCandidate> = on_address
        .iter()
        .filter(|candidate| {
            candidate.expected_amount_raw == transfer.amount_raw
                && candidate.status == AttemptStatus::AwaitingPayment
                && transfer.block_time >= candidate.leased_from
                && transfer.block_time < candidate.leased_until
        })
        .copied()
        .collect();
    match live.as_slice() {
        [single] => return matched(single, MatchStrategy::ExactAmount, transfer),
        [] => {}
        many => return ambiguous(many),
    }

    // A slot released after the late-payment window can be handed to another
    // attempt. Matching by the block's time is what keeps a payment that
    // arrived on day 35 from being credited to whoever holds the slot today.
    let historical: Vec<&AttemptCandidate> = on_address
        .iter()
        .filter(|candidate| {
            candidate.expected_amount_raw == transfer.amount_raw
                && transfer.block_time >= candidate.leased_from
                && transfer.block_time < candidate.leased_until
        })
        .copied()
        .collect();
    match historical.as_slice() {
        [single] => matched(single, MatchStrategy::HistoricalSlot, transfer),
        [] => MatchOutcome::Unmatched,
        many => ambiguous(many),
    }
}

fn matched(
    candidate: &AttemptCandidate,
    strategy: MatchStrategy,
    transfer: &TransferFacts,
) -> MatchOutcome {
    MatchOutcome::Matched {
        attempt_id: candidate.attempt_id,
        payment_intent_id: candidate.payment_intent_id,
        strategy,
        late: candidate.status != AttemptStatus::AwaitingPayment
            || transfer.block_time >= candidate.leased_until,
    }
}

fn ambiguous(candidates: &[&AttemptCandidate]) -> MatchOutcome {
    let mut attempt_ids: Vec<Uuid> = candidates
        .iter()
        .map(|candidate| candidate.attempt_id)
        .collect();
    attempt_ids.sort_unstable();
    MatchOutcome::Ambiguous { attempt_ids }
}

/// What a risk screening said about the source of funds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskDecision {
    Allow,
    Review,
    Deny,
    /// No screening was configured. This is not the same as `Allow`.
    Skipped,
}

impl RiskDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Review => "review",
            Self::Deny => "deny",
            Self::Skipped => "skipped",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementError::UnknownRiskDecision`] for unknown text.
    pub fn parse(value: &str) -> Result<Self, SettlementError> {
        match value {
            "allow" => Ok(Self::Allow),
            "review" => Ok(Self::Review),
            "deny" => Ok(Self::Deny),
            "skipped" => Ok(Self::Skipped),
            other => Err(SettlementError::UnknownRiskDecision(other.to_owned())),
        }
    }
}

/// One band of the settlement policy: how much evidence an amount of this size
/// needs, and whether it may settle without a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementTier {
    /// The largest fiat amount, in minor units, this band covers.
    pub max_fiat_minor: i64,
    pub min_independent_groups: u32,
    pub require_own_node: bool,
    pub require_risk_allow: bool,
    pub auto_settle: bool,
}

/// The active settlement policy for one currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementPolicy {
    pub id: Uuid,
    pub version: String,
    pub tiers: Vec<SettlementTier>,
}

impl SettlementPolicy {
    /// Returns the band that covers this amount, if the policy covers it at
    /// all. An uncovered amount is never treated as the smallest band.
    #[must_use]
    pub fn tier_for(&self, fiat_minor: i64) -> Option<SettlementTier> {
        self.tiers
            .iter()
            .filter(|tier| tier.max_fiat_minor >= fiat_minor)
            .min_by_key(|tier| tier.max_fiat_minor)
            .copied()
    }
}

/// The evidence a settlement decision is made on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementEvidence {
    pub fiat_amount_minor: i64,
    pub expected_amount_raw: RawAmount,
    pub already_allocated_raw: RawAmount,
    pub transfer_amount_raw: RawAmount,
    pub transfer_state: TransferState,
    pub independent_groups: u32,
    pub had_own_node: bool,
    pub risk: RiskDecision,
    pub attempt_status: AttemptStatus,
    pub late: bool,
}

/// What may happen to the money.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementOutcome {
    /// Allocate and fulfil: the obligation is fully covered.
    Settle { allocate_raw: RawAmount },
    /// Allocate, but the obligation is not covered yet.
    Partial { allocate_raw: RawAmount },
    /// Allocate what was owed and leave the rest for a person to decide. An
    /// overpayment is never silently absorbed.
    Overpaid {
        allocate_raw: RawAmount,
        remainder_raw: RawAmount,
    },
    /// Do nothing yet; the reason says what is missing.
    Hold { reason: HoldReason },
    /// A person must decide, with the full evidence in front of them.
    ManualRequired { reason: ManualReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// The chain fact is not final yet.
    NotFinal,
    /// The chain fact was invalidated.
    Invalidated,
    /// Risk screening refused the source of funds.
    RiskDenied,
    /// The obligation was already covered.
    AlreadyCovered,
    /// The attempt was cancelled.
    Cancelled,
}

impl HoldReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFinal => "not_final",
            Self::Invalidated => "invalidated",
            Self::RiskDenied => "risk_denied",
            Self::AlreadyCovered => "already_covered",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualReason {
    /// This amount is above every automatic band of the policy.
    AboveAutomaticBand,
    /// Fewer independent groups confirmed the fact than this amount requires.
    NotEnoughIndependentGroups,
    /// This amount requires an own-node attestation.
    OwnNodeRequired,
    /// Risk screening asked for a human, or did not run where it is required.
    RiskReview,
    /// The payment arrived after its window.
    LatePayment,
    /// The policy does not cover an amount of this size at all.
    UncoveredAmount,
}

impl ManualReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AboveAutomaticBand => "above_automatic_band",
            Self::NotEnoughIndependentGroups => "not_enough_independent_groups",
            Self::OwnNodeRequired => "own_node_required",
            Self::RiskReview => "risk_review",
            Self::LatePayment => "late_payment",
            Self::UncoveredAmount => "uncovered_amount",
        }
    }
}

/// Decides what may be done with one verified transfer against one attempt.
///
/// # Errors
///
/// Returns [`SettlementError`] when the stored amounts cannot be combined
/// without overflowing 256 bits, which is a data defect rather than a
/// business case.
pub fn decide_settlement(
    evidence: &SettlementEvidence,
    policy: &SettlementPolicy,
) -> Result<SettlementOutcome, SettlementError> {
    if evidence.transfer_state == TransferState::Invalidated {
        return Ok(SettlementOutcome::Hold {
            reason: HoldReason::Invalidated,
        });
    }
    if evidence.transfer_state != TransferState::Finalized {
        return Ok(SettlementOutcome::Hold {
            reason: HoldReason::NotFinal,
        });
    }
    if evidence.attempt_status == AttemptStatus::Cancelled {
        return Ok(SettlementOutcome::Hold {
            reason: HoldReason::Cancelled,
        });
    }
    if evidence.risk == RiskDecision::Deny {
        return Ok(SettlementOutcome::Hold {
            reason: HoldReason::RiskDenied,
        });
    }

    let Some(tier) = policy.tier_for(evidence.fiat_amount_minor) else {
        return Ok(SettlementOutcome::ManualRequired {
            reason: ManualReason::UncoveredAmount,
        });
    };
    if !tier.auto_settle {
        return Ok(SettlementOutcome::ManualRequired {
            reason: ManualReason::AboveAutomaticBand,
        });
    }
    if evidence.independent_groups < tier.min_independent_groups {
        return Ok(SettlementOutcome::ManualRequired {
            reason: ManualReason::NotEnoughIndependentGroups,
        });
    }
    if tier.require_own_node && !evidence.had_own_node {
        return Ok(SettlementOutcome::ManualRequired {
            reason: ManualReason::OwnNodeRequired,
        });
    }
    if tier.require_risk_allow && evidence.risk != RiskDecision::Allow {
        return Ok(SettlementOutcome::ManualRequired {
            reason: ManualReason::RiskReview,
        });
    }
    if evidence.late {
        return Ok(SettlementOutcome::ManualRequired {
            reason: ManualReason::LatePayment,
        });
    }

    let expected = evidence.expected_amount_raw.as_u256();
    let allocated = evidence.already_allocated_raw.as_u256();
    let arriving = evidence.transfer_amount_raw.as_u256();
    let Some(outstanding) = expected.checked_sub(allocated) else {
        return Ok(SettlementOutcome::Hold {
            reason: HoldReason::AlreadyCovered,
        });
    };
    if outstanding.is_zero() {
        return Ok(SettlementOutcome::Hold {
            reason: HoldReason::AlreadyCovered,
        });
    }

    if arriving > outstanding {
        let remainder = arriving
            .checked_sub(outstanding)
            .ok_or(SettlementError::AmountOverflow)?;
        return Ok(SettlementOutcome::Overpaid {
            allocate_raw: RawAmount::positive(outstanding)
                .map_err(|_| SettlementError::AmountOverflow)?,
            remainder_raw: RawAmount::positive(remainder)
                .map_err(|_| SettlementError::AmountOverflow)?,
        });
    }
    if arriving == outstanding {
        return Ok(SettlementOutcome::Settle {
            allocate_raw: evidence.transfer_amount_raw,
        });
    }
    Ok(SettlementOutcome::Partial {
        allocate_raw: evidence.transfer_amount_raw,
    })
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SettlementError {
    #[error("unknown attempt status: {0}")]
    UnknownAttemptStatus(String),
    #[error("unknown risk decision: {0}")]
    UnknownRiskDecision(String),
    #[error("stored amounts cannot be combined without overflowing 256 bits")]
    AmountOverflow,
}

#[cfg(test)]
mod tests;
