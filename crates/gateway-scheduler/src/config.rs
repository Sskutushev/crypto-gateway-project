use std::time::Duration;

use thiserror::Error;

/// Mirrors the application's own batch ceiling for one expiry transaction.
const MAX_BATCH_LIMIT: u32 = 10_000;
const MAX_BATCHES_PER_TICK: u32 = 1_000;
const MAX_RETRY_ATTEMPTS: u32 = 10;
const MAX_BACKOFF_DOUBLINGS: u32 = 16;

/// Bounds for the quote-expiry and amount-lease archival sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryConfig {
    /// Delay between the end of one sweep and the start of the next.
    pub interval: Duration,
    /// Rows a single database transaction may expire or archive.
    pub batch_limit: u32,
    /// Transactions one sweep may run before yielding until the next tick.
    pub max_batches_per_tick: u32,
    pub retry: RetryPolicy,
}

impl Default for ExpiryConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            batch_limit: 200,
            max_batches_per_tick: 10,
            retry: RetryPolicy::default(),
        }
    }
}

impl ExpiryConfig {
    /// Returns the configuration only when every bound is safe to run.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulerConfigError`] when a bound would allow an unbounded,
    /// hot, or non-progressing sweep.
    pub fn validated(self) -> Result<Self, SchedulerConfigError> {
        if self.interval < Duration::from_secs(1) {
            return Err(SchedulerConfigError::Interval);
        }
        if self.batch_limit == 0 || self.batch_limit > MAX_BATCH_LIMIT {
            return Err(SchedulerConfigError::BatchLimit);
        }
        if self.max_batches_per_tick == 0 || self.max_batches_per_tick > MAX_BATCHES_PER_TICK {
            return Err(SchedulerConfigError::BatchesPerTick);
        }
        self.retry.validate()?;
        Ok(self)
    }
}

/// Retry bounds for one batch inside a sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts for one batch, including the first.
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    fn validate(&self) -> Result<(), SchedulerConfigError> {
        if self.max_attempts == 0 || self.max_attempts > MAX_RETRY_ATTEMPTS {
            return Err(SchedulerConfigError::RetryAttempts);
        }
        if self.initial_backoff.is_zero() || self.initial_backoff > self.max_backoff {
            return Err(SchedulerConfigError::Backoff);
        }
        Ok(())
    }

    /// Returns the delay before the attempt that follows `attempt`.
    pub(crate) fn backoff_after(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(MAX_BACKOFF_DOUBLINGS);
        let scaled = self
            .initial_backoff
            .saturating_mul(2_u32.saturating_pow(doublings));
        scaled.min(self.max_backoff)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchedulerConfigError {
    #[error("sweep interval must be at least one second")]
    Interval,
    #[error("batch limit must be between 1 and {MAX_BATCH_LIMIT}")]
    BatchLimit,
    #[error("batches per tick must be between 1 and {MAX_BATCHES_PER_TICK}")]
    BatchesPerTick,
    #[error("retry attempts must be between 1 and {MAX_RETRY_ATTEMPTS}")]
    RetryAttempts,
    #[error("initial backoff must be non-zero and at most the maximum backoff")]
    Backoff,
}

#[cfg(test)]
mod tests {
    use super::{Duration, ExpiryConfig, RetryPolicy, SchedulerConfigError};

    #[test]
    fn rejects_unbounded_or_hot_configuration() {
        let too_fast = ExpiryConfig {
            interval: Duration::from_millis(100),
            ..ExpiryConfig::default()
        };
        assert_eq!(too_fast.validated(), Err(SchedulerConfigError::Interval));

        let no_progress = ExpiryConfig {
            batch_limit: 0,
            ..ExpiryConfig::default()
        };
        assert_eq!(
            no_progress.validated(),
            Err(SchedulerConfigError::BatchLimit)
        );

        let unbounded_batch = ExpiryConfig {
            batch_limit: 10_001,
            ..ExpiryConfig::default()
        };
        assert_eq!(
            unbounded_batch.validated(),
            Err(SchedulerConfigError::BatchLimit)
        );

        let unbounded_tick = ExpiryConfig {
            max_batches_per_tick: 0,
            ..ExpiryConfig::default()
        };
        assert_eq!(
            unbounded_tick.validated(),
            Err(SchedulerConfigError::BatchesPerTick)
        );

        let busy_retry = ExpiryConfig {
            retry: RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::ZERO,
                max_backoff: Duration::from_secs(1),
            },
            ..ExpiryConfig::default()
        };
        assert_eq!(busy_retry.validated(), Err(SchedulerConfigError::Backoff));

        let inverted_backoff = ExpiryConfig {
            retry: RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_secs(10),
                max_backoff: Duration::from_secs(1),
            },
            ..ExpiryConfig::default()
        };
        assert_eq!(
            inverted_backoff.validated(),
            Err(SchedulerConfigError::Backoff)
        );

        assert!(ExpiryConfig::default().validated().is_ok());
    }

    #[test]
    fn backoff_grows_then_stops_at_the_ceiling() {
        let policy = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(4),
        };

        assert_eq!(policy.backoff_after(1), Duration::from_secs(1));
        assert_eq!(policy.backoff_after(2), Duration::from_secs(2));
        assert_eq!(policy.backoff_after(3), Duration::from_secs(4));
        assert_eq!(policy.backoff_after(9), Duration::from_secs(4));
        assert_eq!(policy.backoff_after(u32::MAX), Duration::from_secs(4));
    }
}
