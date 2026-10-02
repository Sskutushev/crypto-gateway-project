//! Delivery timing for this process: how long an event waited for its first
//! attempt, and how long a merchant endpoint took to answer.

use std::time::Duration;

use gateway_telemetry::{DELAY_BUCKETS_MICROS, Histogram, LATENCY_BUCKETS_MICROS, Labels};

static FIRST_ATTEMPT_DELAY: Histogram = Histogram::new(
    "gateway_outbox_first_attempt_delay_seconds",
    "Time from an event being written to the outbox until its first delivery attempt.",
    &DELAY_BUCKETS_MICROS,
);

static DELIVERY_DURATION: Histogram = Histogram::new(
    "gateway_webhook_delivery_duration_seconds",
    "How long one delivery attempt to a merchant endpoint took, by outcome.",
    &LATENCY_BUCKETS_MICROS,
);

pub(crate) fn record_first_attempt_delay(delay: Duration) {
    FIRST_ATTEMPT_DELAY.observe(&Labels::none(), delay);
}

pub(crate) fn record_delivery(outcome: &str, duration: Duration) {
    DELIVERY_DURATION.observe(&Labels::new(&[("outcome", outcome)]), duration);
}
