//! Checking that the money still adds up.
//!
//! Reconciliation is the part of the system that assumes the rest of it is
//! wrong. It re-reads what was observed, what was made canonical, what was
//! allocated and what was fulfilled, and reports every place those four stop
//! agreeing.
//!
//! A counter that disagrees is something to explain today. Money that
//! disagrees closes the rail on its own, because a system that does not add up
//! must not keep taking payments while somebody looks into it.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    Clock, ComponentState, HealthRepository, OperationsRepository, RepositoryError,
    health::HealthError,
};

/// The component name reconciliation publishes its own state under.
pub const COMPONENT: &str = "reconciler";

/// The reason code a hard stop closes a rail with.
pub const HARD_STOP_REASON: &str = "reconciliation_money_discrepancy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationKind {
    /// The frequent pass over a recent window.
    Incremental,
    /// The daily pass over everything the window policy keeps.
    Daily,
}

impl ReconciliationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incremental => "incremental",
            Self::Daily => "daily",
        }
    }
}

/// What a run found, one finding at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscrepancyKind {
    /// Sources reported a transfer and no canonical fact was ever made of it.
    ObservedNotCanonical,
    /// More was allocated from a transfer than the transfer carried.
    AllocationExceedsTransfer,
    /// A payment was settled and nothing claimed the fulfilment.
    SettledNotFulfilled,
    /// A fulfilment exists with no settlement decision behind it.
    FulfilledNotSettled,
    /// Money arrived that still matches no obligation.
    UnmatchedInboundAging,
    /// A transfer the chain later invalidated still carries an allocation.
    AllocatedOnInvalidatedTransfer,
    /// A source's cursor is far behind the head it reported.
    ObserverBehind,
    /// A payment has been held for longer than anyone should wait.
    HeldPaymentAging,
}

impl DiscrepancyKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObservedNotCanonical => "observed_not_canonical",
            Self::AllocationExceedsTransfer => "allocation_exceeds_transfer",
            Self::SettledNotFulfilled => "settled_not_fulfilled",
            Self::FulfilledNotSettled => "fulfilled_not_settled",
            Self::UnmatchedInboundAging => "unmatched_inbound_aging",
            Self::AllocatedOnInvalidatedTransfer => "allocated_on_invalidated_transfer",
            Self::ObserverBehind => "observer_behind",
            Self::HeldPaymentAging => "held_payment_aging",
        }
    }

    /// Whether this finding is about money rather than about a counter.
    ///
    /// Money closes the rail. Everything else is explained in daylight.
    #[must_use]
    pub const fn affects_money(self) -> bool {
        matches!(
            self,
            Self::AllocationExceedsTransfer
                | Self::SettledNotFulfilled
                | Self::FulfilledNotSettled
                | Self::AllocatedOnInvalidatedTransfer
        )
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`ReconciliationError::UnknownDiscrepancy`] for unknown text.
    pub fn parse(value: &str) -> Result<Self, ReconciliationError> {
        match value {
            "observed_not_canonical" => Ok(Self::ObservedNotCanonical),
            "allocation_exceeds_transfer" => Ok(Self::AllocationExceedsTransfer),
            "settled_not_fulfilled" => Ok(Self::SettledNotFulfilled),
            "fulfilled_not_settled" => Ok(Self::FulfilledNotSettled),
            "unmatched_inbound_aging" => Ok(Self::UnmatchedInboundAging),
            "allocated_on_invalidated_transfer" => Ok(Self::AllocatedOnInvalidatedTransfer),
            "observer_behind" => Ok(Self::ObserverBehind),
            "held_payment_aging" => Ok(Self::HeldPaymentAging),
            other => Err(ReconciliationError::UnknownDiscrepancy(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discrepancy {
    pub kind: DiscrepancyKind,
    pub transfer_id: Option<Uuid>,
    pub payment_intent_id: Option<Uuid>,
    pub asset_id: Option<Uuid>,
    pub detail: Value,
}

/// How far back a run looks, and how long something may sit unresolved before
/// it is a finding rather than a payment still in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconciliationWindow {
    pub lookback: Duration,
    /// How long a canonical fact may stay uncreated before the observations
    /// behind it are a finding.
    pub canonicalization_grace: Duration,
    /// How long unmatched money may sit before somebody must look at it.
    pub unmatched_grace: Duration,
    /// How long a held payment may sit before somebody must look at it.
    pub held_grace: Duration,
    /// How far a cursor may sit behind the head a source reported.
    pub max_cursor_lag_blocks: i64,
}

impl Default for ReconciliationWindow {
    fn default() -> Self {
        Self {
            lookback: Duration::hours(25),
            canonicalization_grace: Duration::minutes(30),
            unmatched_grace: Duration::hours(6),
            held_grace: Duration::hours(24),
            max_cursor_lag_blocks: 1_200,
        }
    }
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub run_id: Uuid,
    pub kind: ReconciliationKind,
    pub transfers_examined: u32,
    pub intents_examined: u32,
    pub discrepancies: Vec<Discrepancy>,
    pub money_discrepancies: u32,
    pub status: RunStatus,
    /// Assets whose rail this run closed, if any.
    pub stopped_assets: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Ok,
    Drift,
    HardStop,
}

impl RunStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Drift => "drift",
            Self::HardStop => "hard_stop",
        }
    }
}

/// What a scan counted and found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanFindings {
    pub transfers_examined: u32,
    pub intents_examined: u32,
    pub discrepancies: Vec<Discrepancy>,
}

/// One completed run, as it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord<'a> {
    pub kind: ReconciliationKind,
    pub window_start: OffsetDateTime,
    pub window_end: OffsetDateTime,
    pub findings: &'a ScanFindings,
    pub status: RunStatus,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

#[async_trait]
pub trait ReconciliationRepository: Send + Sync {
    /// Runs every check over one window.
    async fn scan(
        &self,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        window: ReconciliationWindow,
    ) -> Result<ScanFindings, RepositoryError>;

    /// Stores the run and its findings in one transaction.
    async fn record_run(&self, record: RunRecord<'_>) -> Result<Uuid, RepositoryError>;
}

#[derive(Debug)]
pub struct ReconciliationService<R, C> {
    repository: Arc<R>,
    clock: C,
    window: ReconciliationWindow,
}

impl<R, C> ReconciliationService<R, C>
where
    R: ReconciliationRepository + OperationsRepository + HealthRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C, window: ReconciliationWindow) -> Self {
        Self {
            repository,
            clock,
            window,
        }
    }

    /// Runs one pass and acts on what it finds.
    ///
    /// # Errors
    ///
    /// Returns [`ReconciliationError`] when storage fails. A failed run
    /// publishes its own state as unavailable rather than leaving the last
    /// successful one in place.
    pub async fn run_once(
        &self,
        kind: ReconciliationKind,
    ) -> Result<ReconciliationReport, ReconciliationError> {
        let started_at = self.clock.now();
        let window_end = started_at;
        let window_start = window_end - self.window.lookback;

        let findings = match self
            .repository
            .scan(window_start, window_end, self.window)
            .await
        {
            Ok(findings) => findings,
            Err(error) => {
                self.publish(ComponentState::Unavailable, Some("scan failed"))
                    .await?;
                return Err(error.into());
            }
        };

        let money_discrepancies = findings
            .discrepancies
            .iter()
            .filter(|discrepancy| discrepancy.kind.affects_money())
            .count();
        let money_discrepancies = u32::try_from(money_discrepancies).unwrap_or(u32::MAX);
        let status = if money_discrepancies > 0 {
            RunStatus::HardStop
        } else if findings.discrepancies.is_empty() {
            RunStatus::Ok
        } else {
            RunStatus::Drift
        };

        let finished_at = self.clock.now();
        let run_id = self
            .repository
            .record_run(RunRecord {
                kind,
                window_start,
                window_end,
                findings: &findings,
                status,
                started_at,
                finished_at,
            })
            .await?;

        // A money discrepancy closes every rail it can name. An anonymous one
        // closes nothing by itself, which is why the run's status still says
        // hard stop and the operator page still shows it.
        let mut stopped_assets = Vec::new();
        if status == RunStatus::HardStop {
            for discrepancy in &findings.discrepancies {
                let Some(asset_id) = discrepancy.asset_id else {
                    continue;
                };
                if !discrepancy.kind.affects_money() || stopped_assets.contains(&asset_id) {
                    continue;
                }
                self.repository
                    .open_rail_stop(
                        asset_id,
                        HARD_STOP_REASON,
                        Some(discrepancy.kind.as_str()),
                        COMPONENT,
                        finished_at,
                    )
                    .await?;
                stopped_assets.push(asset_id);
            }
        }

        self.publish(
            match status {
                RunStatus::Ok => ComponentState::Ok,
                RunStatus::Drift => ComponentState::Degraded,
                RunStatus::HardStop => ComponentState::Stopped,
            },
            (status != RunStatus::Ok).then_some("see reconciliation discrepancies"),
        )
        .await?;

        Ok(ReconciliationReport {
            run_id,
            kind,
            transfers_examined: findings.transfers_examined,
            intents_examined: findings.intents_examined,
            money_discrepancies,
            status,
            discrepancies: findings.discrepancies,
            stopped_assets,
        })
    }

    async fn publish(
        &self,
        state: ComponentState,
        detail: Option<&str>,
    ) -> Result<(), ReconciliationError> {
        self.repository
            .publish_component_state(COMPONENT, state, detail, self.clock.now())
            .await?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ReconciliationError {
    #[error("unknown discrepancy kind: {0}")]
    UnknownDiscrepancy(String),
    #[error(transparent)]
    Health(#[from] HealthError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl ReconciliationError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            Self::Health(error) => error.is_transient(),
            Self::UnknownDiscrepancy(_) => false,
        }
    }
}

#[cfg(test)]
mod tests;
