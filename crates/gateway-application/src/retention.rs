//! Bounded deletion of operational rows nobody needs any more.
//!
//! Three append-only tables grow with traffic rather than with money:
//! webhook delivery attempts, raw chain observations, and component health
//! transitions. Retention deletes from them only rows that are older than a
//! configured age AND that no audit, reconciliation or evidence read still
//! needs:
//!
//! - a delivery attempt of an event that was delivered, both before the
//!   cutoff, except the newest `keep_delivery_attempts` attempts per event and
//!   endpoint. Attempts of pending or dead-lettered events are all kept: they
//!   are what an operator reads before redelivering.
//! - an observation of a chain event whose canonical transfer is final
//!   (`finalized` or `invalidated`), that no attestation and no conflict item
//!   references, and whose event has no open conflict. The attested readings
//!   behind every canonical transfer, and so every evidence bundle, stay.
//! - a component health transition that is not the newest one of its
//!   component, so the current state keeps the row that explains it.
//!
//! Nothing is configured by default, and nothing is deleted then. An age below
//! [`MIN_RETENTION_DAYS`] is refused.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::{Clock, RepositoryError};

/// The shortest age retention accepts. Anything younger may still be the
/// subject of an open incident.
pub const MIN_RETENTION_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Age after which delivery attempts may go; `None` keeps them all.
    pub webhook_deliveries: Option<Duration>,
    /// Newest attempts kept per event and endpoint, whatever their age.
    pub keep_delivery_attempts: u32,
    /// Age after which final, unreferenced observations may go.
    pub observations: Option<Duration>,
    /// Age after which superseded health transitions may go.
    pub health_events: Option<Duration>,
}

impl RetentionPolicy {
    /// Retention that deletes nothing: the default.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            webhook_deliveries: None,
            keep_delivery_attempts: 3,
            observations: None,
            health_events: None,
        }
    }

    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        self.webhook_deliveries.is_none()
            && self.observations.is_none()
            && self.health_events.is_none()
    }

    /// Builds a policy from whole days, as configured.
    ///
    /// # Errors
    ///
    /// Returns [`RetentionError::TooShort`] for an age under
    /// [`MIN_RETENTION_DAYS`] and [`RetentionError::KeepAtLeastOne`] when no
    /// delivery attempt would be kept.
    pub fn from_days(
        webhook_deliveries: Option<u32>,
        keep_delivery_attempts: u32,
        observations: Option<u32>,
        health_events: Option<u32>,
    ) -> Result<Self, RetentionError> {
        let age = |days: Option<u32>| -> Result<Option<Duration>, RetentionError> {
            match days {
                None => Ok(None),
                Some(days) if i64::from(days) < MIN_RETENTION_DAYS => Err(RetentionError::TooShort),
                Some(days) => Ok(Some(Duration::days(i64::from(days)))),
            }
        };
        if keep_delivery_attempts == 0 {
            return Err(RetentionError::KeepAtLeastOne);
        }
        Ok(Self {
            webhook_deliveries: age(webhook_deliveries)?,
            keep_delivery_attempts,
            observations: age(observations)?,
            health_events: age(health_events)?,
        })
    }
}

/// Rows one batch deleted, per table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionReport {
    pub webhook_deliveries: u64,
    pub observations: u64,
    pub health_events: u64,
    /// Whether every configured table had fewer eligible rows than the limit.
    pub drained: bool,
}

impl RetentionReport {
    #[must_use]
    pub const fn deleted(&self) -> u64 {
        self.webhook_deliveries
            .saturating_add(self.observations)
            .saturating_add(self.health_events)
    }
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("retention needs at least one configured age; with none it deletes nothing")]
    Disabled,
    #[error("a retention age must be at least {MIN_RETENTION_DAYS} days")]
    TooShort,
    #[error("at least one delivery attempt per event and endpoint is kept")]
    KeepAtLeastOne,
    #[error("the retention batch limit must be between 1 and 10000")]
    InvalidBatchLimit,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl RetentionError {
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            _ => false,
        }
    }
}

/// Each method deletes at most `limit` eligible rows, oldest first, records
/// an audit row when it deleted any, and returns how many it deleted.
#[async_trait]
pub trait RetentionRepository: Send + Sync {
    async fn purge_webhook_deliveries(
        &self,
        older_than: OffsetDateTime,
        keep_latest: u32,
        limit: u32,
    ) -> Result<u64, RepositoryError>;

    async fn purge_observations(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<u64, RepositoryError>;

    async fn purge_health_events(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<u64, RepositoryError>;
}

#[derive(Debug)]
pub struct RetentionService<R, C> {
    repository: Arc<R>,
    clock: C,
    policy: RetentionPolicy,
}

impl<R, C> RetentionService<R, C>
where
    R: RetentionRepository,
    C: Clock,
{
    /// # Errors
    ///
    /// Returns [`RetentionError::Disabled`] for a policy that would delete
    /// nothing, so a retention worker never runs while doing nothing.
    pub fn new(
        repository: Arc<R>,
        clock: C,
        policy: RetentionPolicy,
    ) -> Result<Self, RetentionError> {
        if policy.is_disabled() {
            return Err(RetentionError::Disabled);
        }
        if policy.keep_delivery_attempts == 0 {
            return Err(RetentionError::KeepAtLeastOne);
        }
        Ok(Self {
            repository,
            clock,
            policy,
        })
    }

    #[must_use]
    pub const fn policy(&self) -> RetentionPolicy {
        self.policy
    }

    /// Deletes one bounded batch from every configured table.
    ///
    /// # Errors
    ///
    /// Returns [`RetentionError`] for an invalid limit or a storage failure.
    pub async fn purge_batch(&self, limit: u32) -> Result<RetentionReport, RetentionError> {
        if limit == 0 || limit > 10_000 {
            return Err(RetentionError::InvalidBatchLimit);
        }
        let now = self.clock.now();
        let mut report = RetentionReport {
            drained: true,
            ..RetentionReport::default()
        };
        if let Some(age) = self.policy.webhook_deliveries {
            report.webhook_deliveries = self
                .repository
                .purge_webhook_deliveries(now - age, self.policy.keep_delivery_attempts, limit)
                .await?;
            report.drained &= report.webhook_deliveries < u64::from(limit);
        }
        if let Some(age) = self.policy.observations {
            report.observations = self.repository.purge_observations(now - age, limit).await?;
            report.drained &= report.observations < u64::from(limit);
        }
        if let Some(age) = self.policy.health_events {
            report.health_events = self
                .repository
                .purge_health_events(now - age, limit)
                .await?;
            report.drained &= report.health_events < u64::from(limit);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
        }
    }

    /// Records which cutoffs it was asked for and deletes `per_call` rows.
    #[derive(Debug, Default)]
    struct Recorder {
        per_call: u64,
        calls: Mutex<Vec<(&'static str, OffsetDateTime, u32)>>,
    }

    impl Recorder {
        fn record(&self, table: &'static str, cutoff: OffsetDateTime, extra: u32) -> u64 {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((table, cutoff, extra));
            }
            self.per_call
        }
    }

    #[async_trait]
    impl RetentionRepository for Recorder {
        async fn purge_webhook_deliveries(
            &self,
            older_than: OffsetDateTime,
            keep_latest: u32,
            _limit: u32,
        ) -> Result<u64, RepositoryError> {
            Ok(self.record("deliveries", older_than, keep_latest))
        }
        async fn purge_observations(
            &self,
            older_than: OffsetDateTime,
            _limit: u32,
        ) -> Result<u64, RepositoryError> {
            Ok(self.record("observations", older_than, 0))
        }
        async fn purge_health_events(
            &self,
            older_than: OffsetDateTime,
            _limit: u32,
        ) -> Result<u64, RepositoryError> {
            Ok(self.record("health", older_than, 0))
        }
    }

    #[test]
    fn nothing_configured_is_nothing_deleted_and_short_ages_are_refused() {
        assert!(RetentionPolicy::disabled().is_disabled());
        assert!(matches!(
            RetentionService::new(
                Arc::new(Recorder::default()),
                FixedClock,
                RetentionPolicy::disabled()
            ),
            Err(RetentionError::Disabled)
        ));
        assert!(matches!(
            RetentionPolicy::from_days(Some(29), 3, None, None),
            Err(RetentionError::TooShort)
        ));
        assert!(matches!(
            RetentionPolicy::from_days(Some(30), 0, None, None),
            Err(RetentionError::KeepAtLeastOne)
        ));
    }

    #[tokio::test]
    async fn only_configured_tables_are_touched_at_their_own_cutoff()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = Arc::new(Recorder {
            per_call: 2,
            ..Recorder::default()
        });
        let policy = RetentionPolicy::from_days(Some(90), 5, None, Some(30))?;
        let service = RetentionService::new(Arc::clone(&repository), FixedClock, policy)?;

        let report = service.purge_batch(2).await?;

        let now = FixedClock.now();
        let calls = repository.calls.lock().map_err(|_| "poisoned")?.clone();
        assert_eq!(
            calls,
            vec![
                ("deliveries", now - Duration::days(90), 5),
                ("health", now - Duration::days(30), 0),
            ]
        );
        assert_eq!(report.deleted(), 4);
        assert!(!report.drained, "a full batch means more may remain");
        assert!(matches!(
            service.purge_batch(0).await,
            Err(RetentionError::InvalidBatchLimit)
        ));
        Ok(())
    }
}
