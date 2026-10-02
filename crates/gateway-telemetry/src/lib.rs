//! Process-level metrics: what this one process did and how long it took.
//!
//! The database-backed series on the API's `/metrics` say whether money is
//! moving; they are the same for every replica. What they cannot say is how
//! long a request, a batch or a provider call took inside a process, and
//! without that a load test only measures from the outside. This crate holds
//! those numbers and, in the binaries, serves them on a listener of their
//! own, inside the cluster, with no operator key: a latency histogram reveals
//! nothing about a merchant, so it is not an operator secret.
//!
//! No dependency carries the registry: a histogram is a handful of atomics
//! and the text format is a few lines, while a metrics framework is another
//! audit surface in a binary that moves money. Durations are kept in whole
//! microseconds and rendered as seconds without floating-point arithmetic,
//! like every other number this gateway publishes.

mod registry;
#[cfg(feature = "server")]
mod server;

pub use registry::{Counter, Gauge, Histogram, Labels, render};
#[cfg(feature = "server")]
pub use server::{ListenerError, MetricsListener};

/// Upper bounds, in microseconds, for a request or a call: 1 ms to 10 s.
pub const LATENCY_BUCKETS_MICROS: [u64; 12] = [
    1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000,
    10_000_000,
];

/// Upper bounds, in microseconds, for a delay between two events: 100 ms to
/// 10 minutes. A webhook's first attempt or a worker's whole run lives here.
pub const DELAY_BUCKETS_MICROS: [u64; 12] = [
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    30_000_000,
    60_000_000,
    120_000_000,
    300_000_000,
    600_000_000,
];
