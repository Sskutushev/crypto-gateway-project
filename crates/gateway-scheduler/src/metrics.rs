use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Sentinel for "no run has ever succeeded". A zero would be a valid Unix
/// timestamp, so success must be distinguishable from absence.
const NEVER: i64 = i64::MIN;

/// Counters describing what one background worker actually did.
///
/// They are observability only: nothing reads them to decide a payment, and a
/// relaxed ordering is therefore sufficient.
#[derive(Debug)]
pub struct RunMetrics {
    runs_started: AtomicU64,
    runs_succeeded: AtomicU64,
    runs_failed: AtomicU64,
    runs_skipped_overlapping: AtomicU64,
    batches_executed: AtomicU64,
    items_processed: AtomicU64,
    transient_retries: AtomicU64,
    consecutive_failures: AtomicU64,
    backlog_left: AtomicU64,
    runs_not_leader: AtomicU64,
    last_success_unix: AtomicI64,
}

impl Default for RunMetrics {
    fn default() -> Self {
        Self {
            runs_started: AtomicU64::new(0),
            runs_succeeded: AtomicU64::new(0),
            runs_failed: AtomicU64::new(0),
            runs_skipped_overlapping: AtomicU64::new(0),
            batches_executed: AtomicU64::new(0),
            items_processed: AtomicU64::new(0),
            transient_retries: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            backlog_left: AtomicU64::new(0),
            runs_not_leader: AtomicU64::new(0),
            last_success_unix: AtomicI64::new(NEVER),
        }
    }
}

impl RunMetrics {
    pub(crate) fn record_run_started(&self) {
        self.runs_started.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_overlap_skipped(&self) {
        self.runs_skipped_overlapping
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_batch(&self, items: u64) {
        self.batches_executed.fetch_add(1, Ordering::Relaxed);
        self.items_processed.fetch_add(items, Ordering::Relaxed);
    }

    pub(crate) fn record_transient_retry(&self) {
        self.transient_retries.fetch_add(1, Ordering::Relaxed);
    }

    /// Another process holds the lease. This is normal, and counted, because
    /// a worker that is never the leader looks exactly like one that is idle.
    pub(crate) fn record_not_leader(&self) {
        self.runs_not_leader.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_backlog_left(&self) {
        self.backlog_left.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_run_succeeded(&self, at_unix: i64) {
        self.runs_succeeded.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.last_success_unix.store(at_unix, Ordering::Relaxed);
    }

    /// Records a failed sweep and returns the new consecutive failure count.
    pub(crate) fn record_run_failed(&self) -> u64 {
        self.runs_failed.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    #[must_use]
    pub fn snapshot(&self) -> RunMetricsSnapshot {
        let last_success_unix = self.last_success_unix.load(Ordering::Relaxed);
        RunMetricsSnapshot {
            runs_started: self.runs_started.load(Ordering::Relaxed),
            runs_succeeded: self.runs_succeeded.load(Ordering::Relaxed),
            runs_failed: self.runs_failed.load(Ordering::Relaxed),
            runs_skipped_overlapping: self.runs_skipped_overlapping.load(Ordering::Relaxed),
            batches_executed: self.batches_executed.load(Ordering::Relaxed),
            items_processed: self.items_processed.load(Ordering::Relaxed),
            transient_retries: self.transient_retries.load(Ordering::Relaxed),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            backlog_left: self.backlog_left.load(Ordering::Relaxed),
            runs_not_leader: self.runs_not_leader.load(Ordering::Relaxed),
            last_success_unix: (last_success_unix != NEVER).then_some(last_success_unix),
        }
    }
}

/// A point-in-time copy of [`RunMetrics`].
///
/// `last_success_unix` is `None` until a sweep has actually succeeded; an
/// absent success is never reported as an old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunMetricsSnapshot {
    pub runs_started: u64,
    pub runs_succeeded: u64,
    pub runs_failed: u64,
    pub runs_skipped_overlapping: u64,
    pub batches_executed: u64,
    pub items_processed: u64,
    pub transient_retries: u64,
    pub consecutive_failures: u64,
    /// Runs that stopped at the per-tick batch ceiling with work still due.
    pub backlog_left: u64,
    /// Ticks where another process held the lease.
    pub runs_not_leader: u64,
    pub last_success_unix: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::RunMetrics;

    #[test]
    fn absent_success_is_not_reported_as_an_old_one() {
        let metrics = RunMetrics::default();
        assert_eq!(metrics.snapshot().last_success_unix, None);

        metrics.record_run_succeeded(0);
        assert_eq!(metrics.snapshot().last_success_unix, Some(0));
    }

    #[test]
    fn success_clears_the_consecutive_failure_streak() {
        let metrics = RunMetrics::default();

        assert_eq!(metrics.record_run_failed(), 1);
        assert_eq!(metrics.record_run_failed(), 2);
        metrics.record_batch(7);
        metrics.record_run_succeeded(1_700_000_000);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.consecutive_failures, 0);
        assert_eq!(snapshot.runs_failed, 2);
        assert_eq!(snapshot.items_processed, 7);
        assert_eq!(snapshot.batches_executed, 1);
        assert_eq!(snapshot.last_success_unix, Some(1_700_000_000));
    }
}
