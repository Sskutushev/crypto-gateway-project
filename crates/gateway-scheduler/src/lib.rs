//! Bounded background scheduling for the gateway's owned deadlines.
//!
//! A scheduler here never decides money or access. It repeatedly asks the
//! application layer to run one bounded, transactional batch, and it reports
//! every retry, overlap, and failure instead of absorbing them.

mod config;
mod expiry;
mod metrics;

pub use config::{ExpiryConfig, RetryPolicy, SchedulerConfigError};
pub use expiry::{ExpiryScheduler, SweepError, SweepReport};
pub use metrics::{ExpiryMetrics, ExpiryMetricsSnapshot};
