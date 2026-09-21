use std::sync::Arc;

use async_trait::async_trait;
use gateway_application::{ComponentLease, LeaseRepository};
use thiserror::Error;
use time::OffsetDateTime;
use tokio::{
    sync::{Mutex, watch},
    time::{MissedTickBehavior, interval, sleep},
};
use tracing::{error, info, warn};

use crate::{
    config::{BatchConfig, SchedulerConfigError},
    metrics::RunMetrics,
};

/// What one batch of a worker did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchOutcome {
    /// Rows or events this batch actually handled.
    pub processed: u32,
    /// Whether the queue was emptied. `false` means work remains.
    pub drained: bool,
}

/// A bounded unit of background work that runs under a component lease.
///
/// A worker never loops on its own: the loop, the bounds, the retries and the
/// shutdown belong to the scheduler, so every worker fails and stops the same
/// observable way.
#[async_trait]
pub trait LeasedWorker: Send + Sync {
    /// The lease this worker must hold to do anything.
    fn component(&self) -> &str;

    /// A short name for logs and metrics.
    fn name(&self) -> &str;

    /// Runs one bounded batch.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerError`] when the batch could not complete. Transient
    /// errors are retried by the loop; anything else stops the run loudly.
    async fn run_batch(&self, lease: &ComponentLease) -> Result<BatchOutcome, WorkerError>;
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("the component lease was taken over")]
    LeaseLost,
    #[error("{0}")]
    Transient(String),
    #[error("{0}")]
    Permanent(String),
}

impl WorkerError {
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

/// Runs one worker: takes the lease, runs bounded batches, retries only what
/// a retry can fix, and stops between batches when asked.
#[derive(Debug)]
pub struct WorkerLoop<W, L> {
    worker: Arc<W>,
    leases: Arc<L>,
    holder: String,
    config: BatchConfig,
    metrics: Arc<RunMetrics>,
    running: Mutex<()>,
}

impl<W, L> WorkerLoop<W, L>
where
    W: LeasedWorker,
    L: LeaseRepository,
{
    /// Builds a worker loop from validated bounds.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulerConfigError`] when the configuration would allow an
    /// unbounded or non-progressing run.
    pub fn new(
        worker: Arc<W>,
        leases: Arc<L>,
        holder: impl Into<String>,
        config: BatchConfig,
    ) -> Result<Self, SchedulerConfigError> {
        Ok(Self {
            worker,
            leases,
            holder: holder.into(),
            config: config.validated()?,
            metrics: Arc::new(RunMetrics::default()),
            running: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn metrics(&self) -> Arc<RunMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Runs until `shutdown` is set or its sender is dropped.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let mut ticker = interval(self.config.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        info!(
            worker = self.worker.name(),
            component = self.worker.component(),
            interval_seconds = self.config.interval.as_secs(),
            batch_limit = self.config.batch_limit,
            "worker started"
        );

        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                    continue;
                }
            }
            if *shutdown.borrow() {
                break;
            }
            match self.run_once_with(Some(&shutdown)).await {
                Ok(Some(report)) => {
                    if report.processed > 0 || report.retries > 0 {
                        info!(
                            worker = self.worker.name(),
                            batches = report.batches,
                            processed = report.processed,
                            retries = report.retries,
                            drained = report.drained,
                            "worker run completed"
                        );
                    }
                }
                Ok(None) => {}
                Err(RunError::Overlapping) => {
                    warn!(
                        worker = self.worker.name(),
                        "worker run skipped: the previous run is still going"
                    );
                }
                Err(RunError::Work(error)) => {
                    let streak = self.metrics.snapshot().consecutive_failures;
                    error!(
                        worker = self.worker.name(),
                        error = %error,
                        consecutive_failures = streak,
                        "worker run failed"
                    );
                }
            }
        }
        info!(worker = self.worker.name(), "worker stopped");
    }

    /// Runs one bounded batch series.
    ///
    /// Returns `None` when another process holds the lease: not being the
    /// leader is a normal state, not a failure.
    ///
    /// # Errors
    ///
    /// Returns [`RunError`] when another run of this loop is in flight or the
    /// work failed in a way retrying did not fix.
    pub async fn run_once(&self) -> Result<Option<RunReport>, RunError> {
        self.run_once_with(None).await
    }

    async fn run_once_with(
        &self,
        shutdown: Option<&watch::Receiver<bool>>,
    ) -> Result<Option<RunReport>, RunError> {
        let Ok(_guard) = self.running.try_lock() else {
            self.metrics.record_overlap_skipped();
            return Err(RunError::Overlapping);
        };

        let Some(lease) = self
            .leases
            .acquire_component_lease(
                self.worker.component(),
                &self.holder,
                self.config.lease_seconds,
                OffsetDateTime::now_utc(),
            )
            .await
            .map_err(|error| RunError::Work(WorkerError::Transient(error.to_string())))?
        else {
            self.metrics.record_not_leader();
            return Ok(None);
        };

        self.metrics.record_run_started();
        let mut report = RunReport::default();
        for _ in 0..self.config.max_batches_per_tick {
            let outcome = match self.run_batch(&lease, &mut report).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.metrics.record_run_failed();
                    return Err(RunError::Work(error));
                }
            };
            self.metrics.record_batch(u64::from(outcome.processed));
            report.batches = report.batches.saturating_add(1);
            report.processed = report.processed.saturating_add(outcome.processed);
            if outcome.drained {
                report.drained = true;
                break;
            }
            if shutdown.is_some_and(|shutdown| *shutdown.borrow()) {
                report.interrupted = true;
                break;
            }
        }
        if !report.drained && !report.interrupted {
            self.metrics.record_backlog_left();
            warn!(
                worker = self.worker.name(),
                batches = report.batches,
                "worker reached its batch ceiling with work still due"
            );
        }
        self.metrics
            .record_run_succeeded(OffsetDateTime::now_utc().unix_timestamp());
        Ok(Some(report))
    }

    async fn run_batch(
        &self,
        lease: &ComponentLease,
        report: &mut RunReport,
    ) -> Result<BatchOutcome, WorkerError> {
        let mut attempt: u32 = 1;
        loop {
            let error = match self.worker.run_batch(lease).await {
                Ok(outcome) => return Ok(outcome),
                Err(error) => error,
            };
            if !error.is_transient() || attempt >= self.config.retry.max_attempts {
                return Err(error);
            }
            let backoff = self.config.retry.backoff_after(attempt);
            warn!(
                worker = self.worker.name(),
                attempt,
                backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                error = %error,
                "worker batch failed on a transient error; retrying"
            );
            self.metrics.record_transient_retry();
            report.retries = report.retries.saturating_add(1);
            sleep(backoff).await;
            attempt = attempt.saturating_add(1);
        }
    }
}

/// What one run of a worker did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunReport {
    pub batches: u32,
    pub processed: u32,
    pub retries: u32,
    pub drained: bool,
    /// The run stopped between batches because shutdown was requested.
    pub interrupted: bool,
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("another run of this worker is still going")]
    Overlapping,
    #[error(transparent)]
    Work(#[from] WorkerError),
}

#[cfg(test)]
mod tests;
