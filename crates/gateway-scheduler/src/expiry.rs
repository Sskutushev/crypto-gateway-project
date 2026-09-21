use std::sync::Arc;

use gateway_application::{ExpiryResult, ExpirySweeper, QuoteServiceError};
use thiserror::Error;
use time::OffsetDateTime;
use tokio::{
    sync::{Mutex, watch},
    time::{MissedTickBehavior, interval, sleep},
};
use tracing::{error, info, warn};

use crate::{
    config::{ExpiryConfig, SchedulerConfigError},
    metrics::ExpiryMetrics,
};

/// Drives bounded quote-expiry and amount-lease archival batches.
///
/// The scheduler owns no expiry logic. Every batch is one transactional
/// application call, two batches never run at once, transient storage outages
/// are retried with backoff, and anything else stops the sweep loudly instead
/// of being retried against stored state that will not change.
#[derive(Debug)]
pub struct ExpiryScheduler<S> {
    sweeper: Arc<S>,
    config: ExpiryConfig,
    metrics: Arc<ExpiryMetrics>,
    running: Mutex<()>,
}

impl<S> ExpiryScheduler<S>
where
    S: ExpirySweeper,
{
    /// Builds a scheduler from validated bounds.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulerConfigError`] when the configuration would allow an
    /// unbounded or non-progressing sweep.
    pub fn new(sweeper: Arc<S>, config: ExpiryConfig) -> Result<Self, SchedulerConfigError> {
        Ok(Self {
            sweeper,
            config: config.validated()?,
            metrics: Arc::new(ExpiryMetrics::default()),
            running: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn metrics(&self) -> Arc<ExpiryMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Runs sweeps until `shutdown` is set or its sender is dropped.
    ///
    /// A sweep that outlives its interval delays the next tick instead of
    /// overlapping with itself, and shutdown is observed between batches so an
    /// in-flight transaction is never abandoned mid-sweep.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let mut ticker = interval(self.config.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        info!(
            interval_seconds = self.config.interval.as_secs(),
            batch_limit = self.config.batch_limit,
            max_batches_per_tick = self.config.max_batches_per_tick,
            "expiry scheduler started"
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
            match self.sweep(Some(&shutdown)).await {
                Ok(report) => {
                    if report.did_work() {
                        info!(
                            batches = report.batches,
                            quotes_expired = report.quotes_expired,
                            leases_archived = report.leases_archived,
                            retries = report.retries,
                            drained = report.drained,
                            "expiry sweep completed"
                        );
                    }
                }
                Err(SweepError::Overlapping) => {
                    warn!("expiry sweep skipped: a previous sweep is still running");
                }
                Err(SweepError::Sweep(sweep_error)) => {
                    let streak = self.metrics.snapshot().consecutive_failures;
                    error!(
                        error = %sweep_error,
                        consecutive_failures = streak,
                        "expiry sweep failed"
                    );
                }
            }
        }
        info!("expiry scheduler stopped");
    }

    /// Runs one bounded sweep: batches until nothing is due or the per-tick
    /// ceiling is reached.
    ///
    /// # Errors
    ///
    /// Returns [`SweepError::Overlapping`] when another sweep holds the
    /// single-flight guard, or [`SweepError::Sweep`] when a batch failed and
    /// the retry policy did not recover it.
    pub async fn run_once(&self) -> Result<SweepReport, SweepError> {
        self.sweep(None).await
    }

    async fn sweep(
        &self,
        shutdown: Option<&watch::Receiver<bool>>,
    ) -> Result<SweepReport, SweepError> {
        let Ok(_guard) = self.running.try_lock() else {
            self.metrics.record_overlap_skipped();
            return Err(SweepError::Overlapping);
        };
        self.metrics.record_sweep_started();

        let mut report = SweepReport::default();
        let mut interrupted = false;
        for _ in 0..self.config.max_batches_per_tick {
            let result = match self.run_batch(&mut report).await {
                Ok(result) => result,
                Err(sweep_error) => {
                    self.metrics.record_sweep_failed();
                    return Err(SweepError::Sweep(sweep_error));
                }
            };
            self.metrics.record_batch(result);
            report.batches = report.batches.saturating_add(1);
            report.quotes_expired = report.quotes_expired.saturating_add(result.quotes_expired);
            report.leases_archived = report
                .leases_archived
                .saturating_add(result.leases_archived);
            if !self.batch_was_full(result) {
                report.drained = true;
                break;
            }
            if shutdown.is_some_and(|shutdown| *shutdown.borrow()) {
                interrupted = true;
                break;
            }
        }
        if interrupted {
            info!(
                batches = report.batches,
                "expiry sweep stopped between batches for shutdown"
            );
        } else if !report.drained {
            self.metrics.record_backlog_left();
            warn!(
                batches = report.batches,
                "expiry sweep reached its batch ceiling with work still due"
            );
        }
        self.metrics
            .record_sweep_succeeded(OffsetDateTime::now_utc().unix_timestamp());
        Ok(report)
    }

    async fn run_batch(&self, report: &mut SweepReport) -> Result<ExpiryResult, QuoteServiceError> {
        let mut attempt: u32 = 1;
        loop {
            let error = match self.sweeper.sweep_expired(self.config.batch_limit).await {
                Ok(result) => return Ok(result),
                Err(error) => error,
            };
            if !error.is_transient() || attempt >= self.config.retry.max_attempts {
                return Err(error);
            }
            let backoff = self.config.retry.backoff_after(attempt);
            warn!(
                attempt,
                backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                error = %error,
                "expiry batch failed on transient storage error; retrying"
            );
            self.metrics.record_transient_retry();
            report.retries = report.retries.saturating_add(1);
            sleep(backoff).await;
            attempt = attempt.saturating_add(1);
        }
    }

    fn batch_was_full(&self, result: ExpiryResult) -> bool {
        let limit = u64::from(self.config.batch_limit);
        result.quotes_expired >= limit || result.leases_archived >= limit
    }
}

/// What one sweep did. `drained` distinguishes "nothing is due" from "the
/// per-tick ceiling stopped us while work remained".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub batches: u32,
    pub quotes_expired: u64,
    pub leases_archived: u64,
    pub retries: u32,
    pub drained: bool,
}

impl SweepReport {
    #[must_use]
    pub const fn did_work(&self) -> bool {
        self.quotes_expired > 0 || self.leases_archived > 0 || self.retries > 0
    }
}

#[derive(Debug, Error)]
pub enum SweepError {
    #[error("another expiry sweep is still running")]
    Overlapping,
    #[error(transparent)]
    Sweep(#[from] QuoteServiceError),
}

#[cfg(test)]
mod tests;
