//! How long each worker's runs and batches take in this process.
//!
//! [`crate::RunMetrics`] counts what a worker did; these histograms say how
//! long it took, which is what a capacity statement needs.

use std::time::Duration;

use gateway_telemetry::{Counter, DELAY_BUCKETS_MICROS, Histogram, LATENCY_BUCKETS_MICROS, Labels};

static RUN_DURATION: Histogram = Histogram::new(
    "gateway_worker_run_duration_seconds",
    "How long one leased run of a worker took, from lease to report, by outcome.",
    &DELAY_BUCKETS_MICROS,
);

static BATCH_DURATION: Histogram = Histogram::new(
    "gateway_worker_batch_duration_seconds",
    "How long one bounded batch of a worker took, retries included.",
    &LATENCY_BUCKETS_MICROS,
);

static ITEMS_PROCESSED: Counter = Counter::new(
    "gateway_worker_items_processed_total",
    "Rows or events a worker handled.",
);

pub(crate) fn record_run(worker: &str, outcome: &str, elapsed: Duration) {
    RUN_DURATION.observe(
        &Labels::new(&[("worker", worker), ("outcome", outcome)]),
        elapsed,
    );
}

pub(crate) fn record_batch(worker: &str, processed: u32, elapsed: Duration) {
    let labels = Labels::new(&[("worker", worker)]);
    BATCH_DURATION.observe(&labels, elapsed);
    ITEMS_PROCESSED.increment(&labels, u64::from(processed));
}
