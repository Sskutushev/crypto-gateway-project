use std::sync::Arc;

use async_trait::async_trait;
use gateway_application::{
    ChainReader, Clock, ComponentLease, HealthRepository, ObservationRepository,
    OperationsRepository, OutboxError, OutboxRepository, OutboxService, ReconciliationError,
    ReconciliationKind, ReconciliationRepository, ReconciliationService, SettlementRepository,
    SettlementService, SettlementServiceError, VerificationRepository, VerificationService,
    VerificationServiceError, WebhookSender,
};

use crate::worker::{BatchOutcome, LeasedWorker, WorkerError};

/// Turns observations into canonical facts, one bounded batch at a time.
#[derive(Debug)]
pub struct VerificationWorker<R, K, C> {
    service: Arc<VerificationService<R, K, C>>,
    component: String,
    batch_limit: u32,
}

impl<R, K, C> VerificationWorker<R, K, C> {
    pub fn new(
        service: Arc<VerificationService<R, K, C>>,
        component: impl Into<String>,
        batch_limit: u32,
    ) -> Self {
        Self {
            service,
            component: component.into(),
            batch_limit,
        }
    }
}

#[async_trait]
impl<R, K, C> LeasedWorker for VerificationWorker<R, K, C>
where
    R: VerificationRepository + ObservationRepository,
    K: ChainReader,
    C: Clock,
{
    fn component(&self) -> &str {
        &self.component
    }

    fn name(&self) -> &'static str {
        "verifier"
    }

    async fn run_batch(&self, lease: &ComponentLease) -> Result<BatchOutcome, WorkerError> {
        let report = self
            .service
            .verify_pending(lease, self.batch_limit)
            .await
            .map_err(classify_verification)?;
        Ok(BatchOutcome {
            processed: report.examined,
            drained: report.examined < self.batch_limit,
        })
    }
}

#[allow(clippy::needless_pass_by_value)]
fn classify_verification(error: VerificationServiceError) -> WorkerError {
    if matches!(
        error,
        VerificationServiceError::Repository(gateway_application::RepositoryError::LeaseLost)
    ) {
        return WorkerError::LeaseLost;
    }
    if error.is_transient() {
        return WorkerError::Transient(error.to_string());
    }
    WorkerError::Permanent(error.to_string())
}

/// Ties verified money to obligations, one bounded batch at a time.
#[derive(Debug)]
pub struct SettlementWorker<R, C> {
    service: Arc<SettlementService<R, C>>,
    component: String,
    batch_limit: u32,
}

impl<R, C> SettlementWorker<R, C> {
    pub fn new(
        service: Arc<SettlementService<R, C>>,
        component: impl Into<String>,
        batch_limit: u32,
    ) -> Self {
        Self {
            service,
            component: component.into(),
            batch_limit,
        }
    }
}

#[async_trait]
impl<R, C> LeasedWorker for SettlementWorker<R, C>
where
    R: SettlementRepository,
    C: Clock,
{
    fn component(&self) -> &str {
        &self.component
    }

    fn name(&self) -> &'static str {
        "settlement"
    }

    async fn run_batch(&self, lease: &ComponentLease) -> Result<BatchOutcome, WorkerError> {
        let report = self
            .service
            .settle_pending(lease, self.batch_limit)
            .await
            .map_err(classify_settlement)?;
        Ok(BatchOutcome {
            processed: report.examined,
            drained: report.examined < self.batch_limit,
        })
    }
}

#[allow(clippy::needless_pass_by_value)]
fn classify_settlement(error: SettlementServiceError) -> WorkerError {
    if matches!(
        error,
        SettlementServiceError::Repository(gateway_application::RepositoryError::LeaseLost)
    ) {
        return WorkerError::LeaseLost;
    }
    if error.is_transient() {
        return WorkerError::Transient(error.to_string());
    }
    WorkerError::Permanent(error.to_string())
}

/// Delivers what the money path promised, one bounded batch at a time.
#[derive(Debug)]
pub struct OutboxWorker<R, S, C> {
    service: Arc<OutboxService<R, S, C>>,
    component: String,
    batch_limit: u32,
}

impl<R, S, C> OutboxWorker<R, S, C> {
    pub fn new(
        service: Arc<OutboxService<R, S, C>>,
        component: impl Into<String>,
        batch_limit: u32,
    ) -> Self {
        Self {
            service,
            component: component.into(),
            batch_limit,
        }
    }
}

#[async_trait]
impl<R, S, C> LeasedWorker for OutboxWorker<R, S, C>
where
    R: OutboxRepository,
    S: WebhookSender,
    C: Clock,
{
    fn component(&self) -> &str {
        &self.component
    }

    fn name(&self) -> &'static str {
        "outbox"
    }

    async fn run_batch(&self, _lease: &ComponentLease) -> Result<BatchOutcome, WorkerError> {
        let report = self
            .service
            .deliver_due(self.batch_limit)
            .await
            .map_err(classify_outbox)?;
        Ok(BatchOutcome {
            processed: report.claimed,
            drained: report.claimed < self.batch_limit,
        })
    }
}

#[allow(clippy::needless_pass_by_value)]
fn classify_outbox(error: OutboxError) -> WorkerError {
    if error.is_transient() {
        return WorkerError::Transient(error.to_string());
    }
    WorkerError::Permanent(error.to_string())
}

/// Checks that the money still adds up, one bounded pass at a time.
///
/// The pass is the whole batch: reconciliation asks its questions of the whole
/// window or of none of it, so a "drained" report simply means this tick is
/// done rather than that a queue is empty.
#[derive(Debug)]
pub struct ReconciliationWorker<R, C> {
    service: Arc<ReconciliationService<R, C>>,
    component: String,
    kind: ReconciliationKind,
}

impl<R, C> ReconciliationWorker<R, C> {
    pub fn new(
        service: Arc<ReconciliationService<R, C>>,
        component: impl Into<String>,
        kind: ReconciliationKind,
    ) -> Self {
        Self {
            service,
            component: component.into(),
            kind,
        }
    }
}

#[async_trait]
impl<R, C> LeasedWorker for ReconciliationWorker<R, C>
where
    R: ReconciliationRepository + OperationsRepository + HealthRepository,
    C: Clock,
{
    fn component(&self) -> &str {
        &self.component
    }

    fn name(&self) -> &'static str {
        "reconciler"
    }

    async fn run_batch(&self, _lease: &ComponentLease) -> Result<BatchOutcome, WorkerError> {
        let report = self
            .service
            .run_once(self.kind)
            .await
            .map_err(classify_reconciliation)?;
        Ok(BatchOutcome {
            processed: u32::try_from(report.discrepancies.len()).unwrap_or(u32::MAX),
            drained: true,
        })
    }
}

#[allow(clippy::needless_pass_by_value)]
fn classify_reconciliation(error: ReconciliationError) -> WorkerError {
    if error.is_transient() {
        return WorkerError::Transient(error.to_string());
    }
    WorkerError::Permanent(error.to_string())
}
