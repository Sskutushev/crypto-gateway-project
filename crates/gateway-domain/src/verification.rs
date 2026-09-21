use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    AddressKey, ChainEnvironment, ExecutionStatus, Memo, ObservationKind, ObservedTransfer,
    RawAmount, SourceFinality, TransferState, TxHash,
};

/// How much independent evidence a network needs before a reading becomes a
/// fact, and how much confirmation depth makes it final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalityPolicy {
    pub id: Uuid,
    pub version: String,
    pub min_confirmations: i64,
    pub required_source_finality: SourceFinality,
    pub min_independent_groups: u32,
    pub max_evidence_age_seconds: i64,
    pub observed_at: OffsetDateTime,
}

/// One stored observation offered to the verifier as evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceReading {
    pub observation_id: Uuid,
    pub source_id: Uuid,
    pub provider_group: String,
    pub source_kind: String,
    /// The database principal that actually wrote the row.
    pub source_principal: String,
    /// The principal the source is registered under.
    pub declared_principal: String,
    pub kind: ObservationKind,
    pub asset_id: Option<Uuid>,
    pub collector_address_id: Option<Uuid>,
    pub transfer: ObservedTransfer,
    pub observed_at: OffsetDateTime,
}

/// Why one reading was excluded from the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardedReading {
    pub observation_id: Uuid,
    pub reason: DiscardReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    /// The writing principal is not the one the source is registered under.
    /// The reading is evidence of an incident, never of a payment.
    ImpersonatedSource,
}

impl DiscardReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImpersonatedSource => "impersonated_source",
        }
    }
}

/// The role a reading played in the decision. One role per reading, strongest
/// first: an independent re-read outranks a finality claim, which outranks a
/// plain detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationRole {
    Detection,
    Reverify,
    Finality,
    Canonicality,
}

impl AttestationRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detection => "detection",
            Self::Reverify => "reverify",
            Self::Finality => "finality",
            Self::Canonicality => "canonicality",
        }
    }
}

/// The agreed facts about one transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalTransfer {
    pub asset_id: Uuid,
    pub collector_address_id: Uuid,
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub tx_hash: TxHash,
    pub event_index: i32,
    pub block_number: i64,
    pub block_hash: String,
    pub block_time: OffsetDateTime,
    pub token_key: AddressKey,
    pub from_address: AddressKey,
    pub from_address_text: String,
    pub to_address: AddressKey,
    pub to_address_text: String,
    pub amount_raw: RawAmount,
    pub decimals: i16,
    pub memo: Option<Memo>,
}

/// An accepted decision: the fact, who attested to it, and how final it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedTransfer {
    pub transfer: CanonicalTransfer,
    pub attestations: Vec<(Uuid, AttestationRole)>,
    pub independent_groups: u32,
    pub had_own_node: bool,
    pub state: TransferState,
    pub confirmations: i64,
    pub policy_version: String,
    pub discarded: Vec<DiscardedReading>,
}

/// Two sources said different things about the same event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldConflict {
    pub field: ConflictField,
    /// Each conflicting reading with the value it claimed.
    pub items: Vec<(Uuid, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictField {
    AmountRaw,
    Recipient,
    TokenKey,
    BlockHash,
    BlockNumber,
    ExecutionStatus,
    Memo,
    Asset,
}

impl ConflictField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AmountRaw => "amount_raw",
            Self::Recipient => "to_address_key",
            Self::TokenKey => "token_key",
            Self::BlockHash => "block_hash",
            Self::BlockNumber => "block_number",
            Self::ExecutionStatus => "execution_status",
            Self::Memo => "memo",
            Self::Asset => "asset_id",
        }
    }
}

/// What the verifier decided about one group of readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The evidence agrees and suffices: a canonical fact may be created.
    Verified(Box<VerifiedTransfer>),
    /// Sources disagree. No fact is created and the disagreement is recorded.
    Conflicted {
        conflicts: Vec<FieldConflict>,
        discarded: Vec<DiscardedReading>,
    },
    /// The evidence agrees but is not yet enough. Waiting is safe; deciding is
    /// not.
    Insufficient {
        reason: InsufficientReason,
        discarded: Vec<DiscardedReading>,
    },
    /// The readings describe something this gateway must never settle.
    Rejected {
        reason: RejectionReason,
        discarded: Vec<DiscardedReading>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsufficientReason {
    /// No reading at all survived.
    NoEvidence,
    /// The verifier has not re-read the transaction itself yet.
    NoIndependentReread,
    /// Fewer distinct provider groups than the policy requires.
    NotEnoughIndependentGroups { have: u32, need: u32 },
    /// The newest reading is older than the policy allows.
    StaleEvidence,
    /// No reading carried the block identity a canonical fact needs.
    MissingBlockIdentity,
}

impl InsufficientReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoEvidence => "no_evidence",
            Self::NoIndependentReread => "no_independent_reread",
            Self::NotEnoughIndependentGroups { .. } => "not_enough_independent_groups",
            Self::StaleEvidence => "stale_evidence",
            Self::MissingBlockIdentity => "missing_block_identity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    /// The token is not the allowlisted contract, whatever it calls itself.
    UnsupportedAsset,
    /// The transaction did not execute successfully.
    FailedExecution,
    /// The recipient is not a collector address of this gateway.
    ForeignRecipient,
}

impl RejectionReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedAsset => "unsupported_asset",
            Self::FailedExecution => "failed_execution",
            Self::ForeignRecipient => "foreign_recipient",
        }
    }
}

/// Decides what a group of readings about one event proves.
///
/// The readings must all describe the same `(chain, network, environment,
/// tx_hash, event_index)`; the caller groups them.
///
/// # Errors
///
/// Returns [`VerificationError`] when the caller passed readings about
/// different events, which is a programming error rather than a chain event.
pub fn verify(
    readings: &[EvidenceReading],
    policy: &FinalityPolicy,
    now: OffsetDateTime,
) -> Result<Verdict, VerificationError> {
    ensure_one_event(readings)?;

    let mut discarded = Vec::new();
    let accepted: Vec<&EvidenceReading> = readings
        .iter()
        .filter(|reading| {
            if reading.source_principal == reading.declared_principal {
                true
            } else {
                discarded.push(DiscardedReading {
                    observation_id: reading.observation_id,
                    reason: DiscardReason::ImpersonatedSource,
                });
                false
            }
        })
        .collect();

    if accepted.is_empty() {
        return Ok(Verdict::Insufficient {
            reason: InsufficientReason::NoEvidence,
            discarded,
        });
    }

    let conflicts = find_conflicts(&accepted);
    if !conflicts.is_empty() {
        return Ok(Verdict::Conflicted {
            conflicts,
            discarded,
        });
    }

    if let Some(reason) = rejection(&accepted) {
        return Ok(Verdict::Rejected { reason, discarded });
    }

    let independent_groups = independent_groups(&accepted);
    if let Some(reason) = insufficiency(&accepted, independent_groups, policy, now)? {
        return Ok(Verdict::Insufficient { reason, discarded });
    }

    let Some(transfer) = canonical_transfer(&accepted) else {
        return Ok(Verdict::Insufficient {
            reason: InsufficientReason::MissingBlockIdentity,
            discarded,
        });
    };

    let confirmations = confirmations(&accepted, transfer.block_number);
    let best_claim = accepted
        .iter()
        .map(|reading| reading.transfer.source_finality)
        .max()
        .unwrap_or(SourceFinality::Seen);
    let state = if best_claim >= policy.required_source_finality
        && confirmations >= policy.min_confirmations
    {
        TransferState::Finalized
    } else if best_claim >= SourceFinality::Confirmed {
        TransferState::Confirmed
    } else {
        TransferState::Canonical
    };

    Ok(Verdict::Verified(Box::new(VerifiedTransfer {
        transfer,
        attestations: attestation_roles(&accepted),
        independent_groups,
        had_own_node: accepted
            .iter()
            .any(|reading| reading.source_kind == "own_node"),
        state,
        confirmations,
        policy_version: policy.version.clone(),
        discarded,
    })))
}

/// Independence is counted by provider group: two API keys of one provider are
/// one source, however many rows they write.
fn independent_groups(readings: &[&EvidenceReading]) -> u32 {
    let groups: BTreeSet<&str> = readings
        .iter()
        .map(|reading| reading.provider_group.as_str())
        .collect();
    u32::try_from(groups.len()).unwrap_or(u32::MAX)
}

/// Reports why agreeing evidence is still not enough to create a fact.
fn insufficiency(
    readings: &[&EvidenceReading],
    independent_groups: u32,
    policy: &FinalityPolicy,
    now: OffsetDateTime,
) -> Result<Option<InsufficientReason>, VerificationError> {
    if !readings
        .iter()
        .any(|reading| reading.kind == ObservationKind::TargetedLookup)
    {
        return Ok(Some(InsufficientReason::NoIndependentReread));
    }
    if independent_groups < policy.min_independent_groups {
        return Ok(Some(InsufficientReason::NotEnoughIndependentGroups {
            have: independent_groups,
            need: policy.min_independent_groups,
        }));
    }
    let newest = readings
        .iter()
        .map(|reading| reading.observed_at)
        .max()
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let deadline = newest
        .checked_add(Duration::seconds(policy.max_evidence_age_seconds))
        .ok_or(VerificationError::PolicyOutOfRange)?;
    if deadline < now {
        return Ok(Some(InsufficientReason::StaleEvidence));
    }
    Ok(None)
}

fn ensure_one_event(readings: &[EvidenceReading]) -> Result<(), VerificationError> {
    let mut identity = None;
    for reading in readings {
        let current = (
            reading.transfer.chain.as_str(),
            reading.transfer.network.as_str(),
            reading.transfer.chain_environment,
            reading.transfer.tx_hash.as_str(),
            reading.transfer.event_index,
        );
        match identity {
            None => identity = Some(current),
            Some(first) if first == current => {}
            Some(_) => return Err(VerificationError::MixedEvents),
        }
    }
    Ok(())
}

/// A field of one reading, as text, or `None` when this reading did not report
/// it. Absence is incompleteness, not disagreement.
type FieldReader = fn(&EvidenceReading) -> Option<String>;

const COMPARED_FIELDS: [(ConflictField, FieldReader); 8] = [
    (ConflictField::AmountRaw, |reading| {
        Some(reading.transfer.amount_raw.to_string())
    }),
    (ConflictField::Recipient, |reading| {
        Some(reading.transfer.to_address.to_hex())
    }),
    (ConflictField::TokenKey, |reading| {
        Some(reading.transfer.token_key.to_hex())
    }),
    (ConflictField::ExecutionStatus, |reading| {
        Some(reading.transfer.execution_status.as_str().to_owned())
    }),
    (ConflictField::Memo, |reading| {
        Some(
            reading
                .transfer
                .memo
                .as_ref()
                .map_or_else(String::new, |memo| memo.as_str().to_owned()),
        )
    }),
    (ConflictField::Asset, |reading| {
        Some(
            reading
                .asset_id
                .map_or_else(|| "unknown".to_owned(), |id| id.to_string()),
        )
    }),
    (ConflictField::BlockHash, |reading| {
        reading.transfer.block_hash.clone()
    }),
    (ConflictField::BlockNumber, |reading| {
        reading
            .transfer
            .block_number
            .map(|number| number.to_string())
    }),
];

fn find_conflicts(readings: &[&EvidenceReading]) -> Vec<FieldConflict> {
    let mut conflicts = Vec::new();
    for (field, read) in COMPARED_FIELDS {
        let items: Vec<(Uuid, String)> = readings
            .iter()
            .filter_map(|reading| read(reading).map(|value| (reading.observation_id, value)))
            .collect();
        let distinct: BTreeSet<&String> = items.iter().map(|(_, value)| value).collect();
        if distinct.len() > 1 {
            conflicts.push(FieldConflict { field, items });
        }
    }
    conflicts
}

fn rejection(readings: &[&EvidenceReading]) -> Option<RejectionReason> {
    if readings
        .iter()
        .any(|reading| reading.transfer.execution_status == ExecutionStatus::Failed)
    {
        return Some(RejectionReason::FailedExecution);
    }
    if readings.iter().any(|reading| reading.asset_id.is_none()) {
        return Some(RejectionReason::UnsupportedAsset);
    }
    if readings
        .iter()
        .any(|reading| reading.collector_address_id.is_none())
    {
        return Some(RejectionReason::ForeignRecipient);
    }
    None
}

fn canonical_transfer(readings: &[&EvidenceReading]) -> Option<CanonicalTransfer> {
    // The canonical values come from a reading that carries full block
    // identity, preferring the verifier's own re-read.
    let complete = readings
        .iter()
        .filter(|reading| {
            reading.transfer.block_number.is_some()
                && reading.transfer.block_hash.is_some()
                && reading.transfer.block_time.is_some()
        })
        .max_by_key(|reading| match reading.kind {
            ObservationKind::TargetedLookup => 2_u8,
            ObservationKind::CursorScan => 1,
            ObservationKind::FastDetect => 0,
        })?;

    let transfer = &complete.transfer;
    Some(CanonicalTransfer {
        asset_id: complete.asset_id?,
        collector_address_id: complete.collector_address_id?,
        chain: transfer.chain.clone(),
        network: transfer.network.clone(),
        chain_environment: transfer.chain_environment,
        tx_hash: transfer.tx_hash.clone(),
        event_index: transfer.event_index,
        block_number: transfer.block_number?,
        block_hash: transfer.block_hash.clone()?,
        block_time: transfer.block_time?,
        token_key: transfer.token_key.clone(),
        from_address: transfer.from_address.clone(),
        from_address_text: transfer.from_address_text.clone(),
        to_address: transfer.to_address.clone(),
        to_address_text: transfer.to_address_text.clone(),
        amount_raw: transfer.amount_raw,
        decimals: transfer.decimals,
        memo: transfer.memo.clone(),
    })
}

fn confirmations(readings: &[&EvidenceReading], block_number: i64) -> i64 {
    readings
        .iter()
        .filter_map(|reading| reading.transfer.source_head)
        .map(|head| head.saturating_sub(block_number))
        .max()
        .unwrap_or(0)
        .max(0)
}

fn attestation_roles(readings: &[&EvidenceReading]) -> Vec<(Uuid, AttestationRole)> {
    let mut roles: BTreeMap<Uuid, AttestationRole> = BTreeMap::new();
    for reading in readings {
        let role = if reading.kind == ObservationKind::TargetedLookup {
            AttestationRole::Reverify
        } else if reading.transfer.source_finality == SourceFinality::Finalized {
            AttestationRole::Finality
        } else {
            AttestationRole::Detection
        };
        roles.insert(reading.observation_id, role);
    }
    roles.into_iter().collect()
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VerificationError {
    #[error("readings from more than one chain event were passed together")]
    MixedEvents,
    #[error("the finality policy contains a duration no timestamp can represent")]
    PolicyOutOfRange,
}

#[cfg(test)]
mod tests;
