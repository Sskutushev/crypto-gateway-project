use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{
    ChainEnvironment, EvidenceReading, FinalityPolicy, ObservationKind, ObservedTransfer, TxHash,
    Verdict, VerificationError, verify,
};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    ChainSource, Clock, CollectorWatch, ComponentLease, ObservationRepository, RepositoryError,
    ResolvedObservation,
};

/// The identity of one chain event: a transfer inside a transaction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChainEventKey {
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub tx_hash: TxHash,
    pub event_index: i32,
}

/// The verifier's own window onto a chain.
///
/// Evidence that decided money always has an observation, a raw-answer hash, a
/// source and a parser version behind it. "The verifier looked and it was fine"
/// is not a form of proof this system accepts, so a re-read returns a reading
/// that is stored like any other.
#[async_trait]
pub trait ChainReader: Send + Sync {
    /// Reads one event directly from the chain.
    ///
    /// # Errors
    ///
    /// Returns [`ChainReaderError`] when the chain or provider could not be
    /// reached, or answered with something this gateway cannot parse.
    async fn lookup(
        &self,
        event: &ChainEventKey,
    ) -> Result<Option<ObservedTransfer>, ChainReaderError>;
}

#[derive(Debug, Error)]
pub enum ChainReaderError {
    #[error("the chain source is unreachable: {0}")]
    Unreachable(String),
    #[error("the chain source answered with something unparseable: {0}")]
    Unparseable(String),
}

impl ChainReaderError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}

/// What committing one verdict changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictOutcome {
    /// A canonical transfer was created by this call.
    TransferCreated(Uuid),
    /// The canonical transfer already existed; its state may have advanced.
    TransferAdvanced(Uuid),
    /// Sources disagreed and the disagreement was recorded.
    ConflictRecorded,
    /// Nothing was created: the evidence does not yet support a fact.
    Pending,
    /// The event will never be settled by this gateway.
    Refused,
}

#[async_trait]
pub trait VerificationRepository: Send + Sync {
    async fn find_finality_policy(
        &self,
        chain: &str,
        network: &str,
        environment: ChainEnvironment,
    ) -> Result<Option<FinalityPolicy>, RepositoryError>;

    /// Events with fresh evidence and no final decision yet.
    async fn events_awaiting_verdict(
        &self,
        limit: u32,
    ) -> Result<Vec<ChainEventKey>, RepositoryError>;

    async fn evidence_for(
        &self,
        event: &ChainEventKey,
    ) -> Result<Vec<EvidenceReading>, RepositoryError>;

    /// Writes the verdict and everything it implies in one transaction.
    async fn commit_verdict(
        &self,
        lease: &ComponentLease,
        event: &ChainEventKey,
        verdict: &Verdict,
        evidence_count: u32,
        verifier_version: &str,
        now: OffsetDateTime,
    ) -> Result<VerdictOutcome, RepositoryError>;
}

/// Counters for one verification pass. Nothing is dropped silently: every
/// event lands in exactly one bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VerificationReport {
    pub examined: u32,
    pub verified: u32,
    pub conflicted: u32,
    pub insufficient: u32,
    pub rejected: u32,
    pub rereads_performed: u32,
    pub rereads_failed: u32,
    pub missing_policy: u32,
}

/// Turns independent observations into canonical facts.
#[derive(Debug)]
pub struct VerificationService<R, K, C> {
    repository: Arc<R>,
    reader: Arc<K>,
    clock: C,
    verifier_source: ChainSource,
    verifier_version: String,
    parser_version: String,
}

impl<R, K, C> VerificationService<R, K, C>
where
    R: VerificationRepository + ObservationRepository,
    K: ChainReader,
    C: Clock,
{
    pub fn new(
        repository: Arc<R>,
        reader: Arc<K>,
        clock: C,
        verifier_source: ChainSource,
        verifier_version: impl Into<String>,
        parser_version: impl Into<String>,
    ) -> Self {
        Self {
            repository,
            reader,
            clock,
            verifier_source,
            verifier_version: verifier_version.into(),
            parser_version: parser_version.into(),
        }
    }

    /// Decides a bounded batch of events.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationServiceError`] when storage fails, the lease was
    /// taken over, or readings for one event turned out to describe different
    /// events, which is a defect rather than a chain state.
    pub async fn verify_pending(
        &self,
        lease: &ComponentLease,
        limit: u32,
    ) -> Result<VerificationReport, VerificationServiceError> {
        if limit == 0 || limit > 1_000 {
            return Err(VerificationServiceError::InvalidBatchLimit);
        }
        let events = self.repository.events_awaiting_verdict(limit).await?;
        let mut report = VerificationReport::default();

        for event in events {
            report.examined = report.examined.saturating_add(1);
            let Some(policy) = self
                .repository
                .find_finality_policy(&event.chain, &event.network, event.chain_environment)
                .await?
            else {
                // Unknown policy closes the path instead of opening it.
                report.missing_policy = report.missing_policy.saturating_add(1);
                continue;
            };

            let mut evidence = self.repository.evidence_for(&event).await?;
            if !self.has_own_reread(&evidence) {
                match self.reread(lease, &event).await {
                    Ok(true) => {
                        report.rereads_performed = report.rereads_performed.saturating_add(1);
                        evidence = self.repository.evidence_for(&event).await?;
                    }
                    Ok(false) => {
                        // The chain does not know this event. Another source
                        // claimed something the chain does not show, which is
                        // evidence the verifier must not paper over.
                        report.rereads_failed = report.rereads_failed.saturating_add(1);
                    }
                    Err(error) if error.is_transient() => {
                        report.rereads_failed = report.rereads_failed.saturating_add(1);
                        continue;
                    }
                    Err(_) => {
                        report.rereads_failed = report.rereads_failed.saturating_add(1);
                    }
                }
            }

            let evidence_count = u32::try_from(evidence.len()).unwrap_or(u32::MAX);
            let verdict = verify(&evidence, &policy, self.clock.now())?;
            match verdict {
                Verdict::Verified(_) => report.verified = report.verified.saturating_add(1),
                Verdict::Conflicted { .. } => {
                    report.conflicted = report.conflicted.saturating_add(1);
                }
                Verdict::Insufficient { .. } => {
                    report.insufficient = report.insufficient.saturating_add(1);
                }
                Verdict::Rejected { .. } => report.rejected = report.rejected.saturating_add(1),
            }
            self.repository
                .commit_verdict(
                    lease,
                    &event,
                    &verdict,
                    evidence_count,
                    &self.verifier_version,
                    self.clock.now(),
                )
                .await?;
        }
        Ok(report)
    }

    fn has_own_reread(&self, evidence: &[EvidenceReading]) -> bool {
        evidence.iter().any(|reading| {
            reading.kind == ObservationKind::TargetedLookup
                && reading.source_id == self.verifier_source.id
        })
    }

    /// Reads the event from the chain and stores the answer as evidence.
    ///
    /// Returns whether the chain knew the event at all.
    async fn reread(
        &self,
        lease: &ComponentLease,
        event: &ChainEventKey,
    ) -> Result<bool, ChainReaderError> {
        let Some(transfer) = self.reader.lookup(event).await? else {
            return Ok(false);
        };
        let collectors = self
            .repository
            .watched_collectors(
                &self.verifier_source.chain,
                &self.verifier_source.network,
                self.verifier_source.chain_environment,
            )
            .await
            .map_err(|error| ChainReaderError::Unreachable(error.to_string()))?;
        let Some(resolved) = self.resolve(&transfer, &collectors, lease.fence_token) else {
            // The chain shows a transfer to an address this gateway does not
            // watch. That is not evidence about one of our payments.
            return Ok(false);
        };
        self.repository
            .record_observations(&self.verifier_source, lease, &[resolved], None)
            .await
            .map_err(|error| ChainReaderError::Unreachable(error.to_string()))?;
        Ok(true)
    }

    fn resolve(
        &self,
        transfer: &ObservedTransfer,
        collectors: &[CollectorWatch],
        fence_token: i64,
    ) -> Option<ResolvedObservation> {
        let collector = collectors
            .iter()
            .find(|collector| collector.address_key == transfer.to_address)?;
        let asset_id = collectors
            .iter()
            .find(|candidate| {
                candidate.address_key == transfer.to_address
                    && candidate.token_key == transfer.token_key
            })
            .map(|candidate| candidate.asset_id);
        let semantic_hash =
            transfer.semantic_hash(self.verifier_source.id, ObservationKind::TargetedLookup);
        Some(ResolvedObservation {
            transfer: transfer.clone(),
            kind: ObservationKind::TargetedLookup,
            collector_address_id: collector.collector_address_id,
            asset_id,
            semantic_hash,
            observer_version: self.verifier_version.clone(),
            parser_version: self.parser_version.clone(),
            fence_token,
            observed_at: self.clock.now(),
        })
    }
}

#[derive(Debug, Error)]
pub enum VerificationServiceError {
    #[error("verification batch limit must be between 1 and 1000")]
    InvalidBatchLimit,
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl VerificationServiceError {
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
