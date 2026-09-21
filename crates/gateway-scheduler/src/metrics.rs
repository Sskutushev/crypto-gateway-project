use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use gateway_application::ExpiryResult;

/// Sentinel for "no sweep has ever succeeded". A zero would be a valid Unix
/// timestamp, so success must be distinguishable from absence.
const NEVER: i64 = i64::MIN;

/// Counters describing what the expiry scheduler actually did.
///
/// They are observability only: nothing reads them to decide a payment, and a
/// relaxed ordering is therefore sufficient.
#[derive(Debug)]
pub struct ExpiryMetrics {
    sweeps_started: AtomicU64,
    sweeps_succeeded: AtomicU64,
    sweeps_failed: AtomicU64,
    sweeps_skipped_overlapping: AtomicU64,
    batches_executed: AtomicU64,
    quotes_expired: AtomicU64,
    leases_archived: AtomicU64,
    transient_retries: AtomicU64,
    consecutive_failures: AtomicU64,
    backlog_left: AtomicU64,
    last_success_unix: AtomicI64,
}

impl Default for ExpiryMetrics {
    fn default() -> Self {
        Self {
            sweeps_started: AtomicU64::new(0),
            sweeps_succeeded: AtomicU64::new(0),
            sweeps_failed: AtomicU64::new(0),
            sweeps_skipped_overlapping: AtomicU64::new(0),
            batches_executed: AtomicU64::new(0),
            quotes_expired: AtomicU64::new(0),
            leases_archived: AtomicU64::new(0),
            transient_retries: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            backlog_left: AtomicU64::new(0),
            last_success_unix: AtomicI64::new(NEVER),
        }
    }
}

impl ExpiryMetrics {
    pub(crate) fn record_sweep_started(&self) {
        self.sweeps_started.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_overlap_skipped(&self) {
        self.sweeps_skipped_overlapping
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_batch(&self, result: ExpiryResult) {
        self.batches_executed.fetch_add(1, Ordering::Relaxed);
        self.quotes_expired
            .fetch_add(result.quotes_expired, Ordering::Relaxed);
        self.leases_archived
            .fetch_add(result.leases_archived, Ordering::Relaxed);
    }

    pub(crate) fn record_transient_retry(&self) {
        self.transient_retries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_backlog_left(&self) {
        self.backlog_left.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_sweep_succeeded(&self, at_unix: i64) {
        self.sweeps_succeeded.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.last_success_unix.store(at_unix, Ordering::Relaxed);
    }

    /// Records a failed sweep and returns the new consecutive failure count.
    pub(crate) fn record_sweep_failed(&self) -> u64 {
        self.sweeps_failed.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    #[must_use]
    pub fn snapshot(&self) -> ExpiryMetricsSnapshot {
        let last_success_unix = self.last_success_unix.load(Ordering::Relaxed);
        ExpiryMetricsSnapshot {
            sweeps_started: self.sweeps_started.load(Ordering::Relaxed),
            sweeps_succeeded: self.sweeps_succeeded.load(Ordering::Relaxed),
            sweeps_failed: self.sweeps_failed.load(Ordering::Relaxed),
            sweeps_skipped_overlapping: self.sweeps_skipped_overlapping.load(Ordering::Relaxed),
            batches_executed: self.batches_executed.load(Ordering::Relaxed),
            quotes_expired: self.quotes_expired.load(Ordering::Relaxed),
            leases_archived: self.leases_archived.load(Ordering::Relaxed),
            transient_retries: self.transient_retries.load(Ordering::Relaxed),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            backlog_left: self.backlog_left.load(Ordering::Relaxed),
            last_success_unix: (last_success_unix != NEVER).then_some(last_success_unix),
        }
    }
}

/// A point-in-time copy of [`ExpiryMetrics`].
///
/// `last_success_unix` is `None` until a sweep has actually succeeded; an
/// absent success is never reported as an old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryMetricsSnapshot {
    pub sweeps_started: u64,
    pub sweeps_succeeded: u64,
    pub sweeps_failed: u64,
    pub sweeps_skipped_overlapping: u64,
    pub batches_executed: u64,
    pub quotes_expired: u64,
    pub leases_archived: u64,
    pub transient_retries: u64,
    pub consecutive_failures: u64,
    /// Sweeps that stopped at the per-tick batch ceiling with work still due.
    pub backlog_left: u64,
    pub last_success_unix: Option<i64>,
}

#[cfg(test)]
mod tests {
    use gateway_application::ExpiryResult;

    use super::ExpiryMetrics;

    #[test]
    fn absent_success_is_not_reported_as_an_old_one() {
        let metrics = ExpiryMetrics::default();
        assert_eq!(metrics.snapshot().last_success_unix, None);

        metrics.record_sweep_succeeded(0);
        assert_eq!(metrics.snapshot().last_success_unix, Some(0));
    }

    #[test]
    fn success_clears_the_consecutive_failure_streak() {
        let metrics = ExpiryMetrics::default();

        assert_eq!(metrics.record_sweep_failed(), 1);
        assert_eq!(metrics.record_sweep_failed(), 2);
        metrics.record_batch(ExpiryResult {
            quotes_expired: 3,
            leases_archived: 4,
        });
        metrics.record_sweep_succeeded(1_700_000_000);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.consecutive_failures, 0);
        assert_eq!(snapshot.sweeps_failed, 2);
        assert_eq!(snapshot.quotes_expired, 3);
        assert_eq!(snapshot.leases_archived, 4);
        assert_eq!(snapshot.batches_executed, 1);
        assert_eq!(snapshot.last_success_unix, Some(1_700_000_000));
    }
}
