//! Bounded background scheduling for the gateway's owned deadlines.
//!
//! A scheduler here never decides money or access. It repeatedly asks the
//! application layer to run one bounded, transactional batch, and it reports
//! every retry, overlap, and failure instead of absorbing them.

mod config;
mod expiry;
mod metrics;
mod observer;
mod pipeline;
mod worker;

pub use config::{BatchConfig, RetryPolicy, SchedulerConfigError};
pub use expiry::{ExpiryScheduler, SweepError, SweepReport};
pub use metrics::{RunMetrics, RunMetricsSnapshot};
pub use observer::ObservationWorker;
pub use pipeline::{OutboxWorker, SettlementWorker, VerificationWorker};
pub use worker::{BatchOutcome, LeasedWorker, RunError, RunReport, WorkerError, WorkerLoop};
