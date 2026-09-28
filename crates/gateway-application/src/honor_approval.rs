//! Two operators for one manual honor.
//!
//! Honoring parked money pays an intent and tells the merchant, on one
//! person's word. Above a configured threshold that word is not enough: one
//! operator key proposes, and a different one approves. The approval re-reads
//! and re-checks everything the honor depends on, so the second person
//! approves the facts as they are, not as they were when the first one asked.

use async_trait::async_trait;
use gateway_domain::RawAmount;
use serde_json::Value;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    ManualResolution, ManualResolutionResult, OperationsError, OperatorCredential, PageRequest,
    RepositoryError,
};

/// How long a proposal waits for its second operator. Evidence a day old is
/// looked at again from the start rather than approved on memory.
pub const HONOR_PROPOSAL_TTL: Duration = Duration::hours(24);

/// Which honors need a second operator.
///
/// An honor allocating at least `dual_control_min_raw` raw units needs one.
/// The default is zero: every honor does, so a deployment that configured
/// nothing is the safe one. The threshold is in raw units of the transfer's
/// asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HonorApprovalPolicy {
    pub dual_control_min_raw: RawAmount,
}

impl Default for HonorApprovalPolicy {
    fn default() -> Self {
        Self {
            dual_control_min_raw: RawAmount::ZERO,
        }
    }
}

/// A configured threshold that is not a whole number of raw units.
#[derive(Debug, thiserror::Error)]
#[error("the dual-control threshold must be a whole number of raw units")]
pub struct InvalidHonorThreshold;

impl HonorApprovalPolicy {
    /// Reads the configured threshold. Unset is the safe default (every honor
    /// needs a second operator); set but empty or not a whole number is an
    /// error, never a default.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHonorThreshold`] for a value that is not a whole number.
    pub fn from_setting(value: Option<&str>) -> Result<Self, InvalidHonorThreshold> {
        let Some(text) = value.map(str::trim) else {
            return Ok(Self::default());
        };
        if text.is_empty() {
            return Err(InvalidHonorThreshold);
        }
        if text.bytes().all(|byte| byte == b'0') {
            return Ok(Self::default());
        }
        text.parse::<RawAmount>()
            .map(|dual_control_min_raw| Self {
                dual_control_min_raw,
            })
            .map_err(|_| InvalidHonorThreshold)
    }

    #[must_use]
    pub fn requires_second_operator(&self, allocate_raw: RawAmount) -> bool {
        allocate_raw.as_u256() >= self.dual_control_min_raw.as_u256()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl ProposalStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    /// # Errors
    ///
    /// Returns [`RepositoryError::CorruptData`] for text that is not a status.
    pub fn parse(value: &str) -> Result<Self, RepositoryError> {
        match value {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            other => Err(RepositoryError::CorruptData(format!(
                "unknown honor proposal status {other}"
            ))),
        }
    }
}

/// One proposed honor and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HonorProposal {
    pub id: Uuid,
    pub transfer_id: Uuid,
    pub payment_intent_id: Uuid,
    pub attempt_id: Uuid,
    pub merchant_id: Uuid,
    pub allocate_raw: RawAmount,
    pub threshold_raw: RawAmount,
    pub reason: String,
    /// The locked rows as the proposer saw them.
    pub evidence: Value,
    pub proposed_by_key_id: Uuid,
    pub proposed_by_label: String,
    pub status: ProposalStatus,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub decided_by_key_id: Option<Uuid>,
    pub decided_by_label: Option<String>,
    pub decision_reason: Option<String>,
    pub decided_at: Option<OffsetDateTime>,
    pub resolution_id: Option<Uuid>,
    pub replayed: bool,
}

impl crate::operator_reads::Identified for HonorProposal {
    fn id(&self) -> Uuid {
        self.id
    }
}

#[async_trait]
pub trait HonorProposalRepository: Send + Sync {
    /// Checks the honor against locked rows exactly as an honor would, and
    /// stores it as a pending proposal with no money written.
    #[allow(clippy::too_many_arguments)]
    async fn propose_honor(
        &self,
        proposer: &OperatorCredential,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        resolution: &ManualResolution,
        threshold_raw: RawAmount,
        now: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> Result<HonorProposal, OperationsError>;

    /// Runs the proposed honor under the approver's key, re-checking every
    /// condition, and closes the proposal in the same transaction.
    async fn approve_honor(
        &self,
        approver: &OperatorCredential,
        proposal_id: Uuid,
        idempotency_key: &str,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<ManualResolutionResult, OperationsError>;

    async fn reject_honor(
        &self,
        operator: &OperatorCredential,
        proposal_id: Uuid,
        idempotency_key: &str,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<HonorProposal, OperationsError>;

    /// Pending proposals that have not expired, newest first, one sentinel
    /// row past the page.
    async fn pending_honor_proposals(
        &self,
        page: PageRequest,
        now: OffsetDateTime,
    ) -> Result<Vec<HonorProposal>, RepositoryError>;
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn an_unconfigured_deployment_asks_a_second_operator_for_every_honor()
    -> Result<(), gateway_domain::MoneyError> {
        let policy = HonorApprovalPolicy::default();
        assert!(policy.requires_second_operator(RawAmount::from_str("1")?));
        let policy = HonorApprovalPolicy {
            dual_control_min_raw: RawAmount::from_str("1000000")?,
        };
        assert!(!policy.requires_second_operator(RawAmount::from_str("999999")?));
        assert!(policy.requires_second_operator(RawAmount::from_str("1000000")?));
        Ok(())
    }

    #[test]
    fn a_threshold_is_read_exactly_or_refused() -> Result<(), InvalidHonorThreshold> {
        assert_eq!(
            HonorApprovalPolicy::from_setting(None)?,
            HonorApprovalPolicy::default()
        );
        assert_eq!(
            HonorApprovalPolicy::from_setting(Some("0"))?,
            HonorApprovalPolicy::default()
        );
        assert_eq!(
            HonorApprovalPolicy::from_setting(Some(" 5000000 "))?
                .dual_control_min_raw
                .to_string(),
            "5000000"
        );
        for bad in ["", "  ", "1e6", "-1", "1.5", "five"] {
            assert!(
                HonorApprovalPolicy::from_setting(Some(bad)).is_err(),
                "{bad}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_stored_status_round_trips_and_nothing_else_parses() {
        for status in [
            ProposalStatus::Pending,
            ProposalStatus::Approved,
            ProposalStatus::Rejected,
            ProposalStatus::Expired,
        ] {
            assert!(
                matches!(ProposalStatus::parse(status.as_str()), Ok(parsed) if parsed == status)
            );
        }
        assert!(ProposalStatus::parse("approved-ish").is_err());
    }
}
