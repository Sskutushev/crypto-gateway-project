//! What state each component is in, and when it entered it.
//!
//! An automatic failover is allowed; a silent one is not. Every component
//! publishes its state, every change writes an event, and a state that has been
//! held too long is something an operator can see and alert on rather than
//! something that has to be inferred from the absence of payments.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;

use crate::{Clock, RepositoryError};

/// The states a component can be in.
///
/// They are ordered from healthy to worst, so "the worst state anything is in"
/// is a comparison rather than a table of special cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentState {
    /// Working.
    Ok,
    /// Working with less than it should have: one provider of two, a lagging
    /// cursor, a retry that succeeded.
    Degraded,
    /// Not working. Nothing that depends on it may proceed on assumption.
    Unavailable,
    /// Sources contradict each other. This is worse than unavailable: an
    /// unavailable source says nothing, a diverged one says something wrong.
    Diverged,
    /// Deliberately stopped by a person or by reconciliation.
    Stopped,
}

impl ComponentState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Diverged => "diverged",
            Self::Stopped => "stopped",
        }
    }

    /// Parses the stored text.
    ///
    /// # Errors
    ///
    /// Returns [`HealthError::UnknownState`] for text that is not a state.
    pub fn parse(value: &str) -> Result<Self, HealthError> {
        match value {
            "ok" => Ok(Self::Ok),
            "degraded" => Ok(Self::Degraded),
            "unavailable" => Ok(Self::Unavailable),
            "diverged" => Ok(Self::Diverged),
            "stopped" => Ok(Self::Stopped),
            other => Err(HealthError::UnknownState(other.to_owned())),
        }
    }

    /// Whether anything depending on this component may still commit money.
    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Ok)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentStatus {
    pub component: String,
    pub state: ComponentState,
    pub detail: Option<String>,
    pub since: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[async_trait]
pub trait HealthRepository: Send + Sync {
    /// Publishes a component's state.
    ///
    /// Returns whether this call changed the state. An unchanged state
    /// refreshes the timestamp and writes no event, so the event history is a
    /// history of transitions and not of heartbeats.
    async fn publish_component_state(
        &self,
        component: &str,
        state: ComponentState,
        detail: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<bool, RepositoryError>;

    async fn component_statuses(&self) -> Result<Vec<ComponentStatus>, RepositoryError>;
}

#[derive(Debug)]
pub struct HealthService<R, C> {
    repository: Arc<R>,
    clock: C,
}

impl<R, C> HealthService<R, C>
where
    R: HealthRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C) -> Self {
        Self { repository, clock }
    }

    /// Records where a component stands.
    ///
    /// # Errors
    ///
    /// Returns [`HealthError`] when storage fails.
    pub async fn publish(
        &self,
        component: &str,
        state: ComponentState,
        detail: Option<&str>,
    ) -> Result<bool, HealthError> {
        Ok(self
            .repository
            .publish_component_state(component, state, detail, self.clock.now())
            .await?)
    }

    /// Everything the operator page and the metrics endpoint show.
    ///
    /// # Errors
    ///
    /// Returns [`HealthError`] when storage fails.
    pub async fn statuses(&self) -> Result<Vec<ComponentStatus>, HealthError> {
        Ok(self.repository.component_statuses().await?)
    }

    /// The worst state anything is in, with the component that is in it.
    ///
    /// An empty roster is not health: a gateway that has published nothing is
    /// a gateway nobody has heard from.
    ///
    /// # Errors
    ///
    /// Returns [`HealthError`] when storage fails.
    pub async fn worst(&self) -> Result<Option<ComponentStatus>, HealthError> {
        let mut statuses = self.statuses().await?;
        statuses.sort_by(|left, right| {
            right
                .state
                .cmp(&left.state)
                .then_with(|| left.since.cmp(&right.since))
        });
        Ok(statuses.into_iter().next())
    }
}

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("unknown component state: {0}")]
    UnknownState(String),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl HealthError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            Self::UnknownState(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use time::{Duration, OffsetDateTime};

    use super::{ComponentState, ComponentStatus, HealthRepository, HealthService};
    use crate::{Clock, RepositoryError};

    type TestResult = Result<(), Box<dyn Error>>;

    #[derive(Debug, Clone, Copy)]
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
        }
    }

    #[derive(Debug, Default)]
    struct StubRepository {
        statuses: Mutex<Vec<ComponentStatus>>,
    }

    #[async_trait]
    impl HealthRepository for StubRepository {
        async fn publish_component_state(
            &self,
            component: &str,
            state: ComponentState,
            detail: Option<&str>,
            now: OffsetDateTime,
        ) -> Result<bool, RepositoryError> {
            let mut statuses = self
                .statuses
                .lock()
                .map_err(|_| RepositoryError::Unavailable("poisoned".to_owned()))?;
            if let Some(existing) = statuses
                .iter_mut()
                .find(|status| status.component == component)
            {
                let changed = existing.state != state;
                existing.state = state;
                existing.updated_at = now;
                if changed {
                    existing.since = now;
                }
                return Ok(changed);
            }
            statuses.push(ComponentStatus {
                component: component.to_owned(),
                state,
                detail: detail.map(ToOwned::to_owned),
                since: now,
                updated_at: now,
            });
            Ok(true)
        }

        async fn component_statuses(&self) -> Result<Vec<ComponentStatus>, RepositoryError> {
            Ok(self
                .statuses
                .lock()
                .map(|statuses| statuses.clone())
                .unwrap_or_default())
        }
    }

    #[tokio::test]
    async fn the_worst_state_is_the_one_an_operator_is_shown() -> TestResult {
        let service = HealthService::new(Arc::new(StubRepository::default()), FixedClock);

        service
            .publish("observer:tron:a", ComponentState::Ok, None)
            .await?;
        service
            .publish("observer:tron:b", ComponentState::Degraded, Some("lagging"))
            .await?;
        service
            .publish(
                "verifier",
                ComponentState::Diverged,
                Some("sources disagree"),
            )
            .await?;

        let worst = service.worst().await?.ok_or("something was published")?;
        assert_eq!(worst.component, "verifier");
        assert_eq!(worst.state, ComponentState::Diverged);
        Ok(())
    }

    #[tokio::test]
    async fn only_a_change_of_state_is_a_transition() -> TestResult {
        let service = HealthService::new(Arc::new(StubRepository::default()), FixedClock);

        assert!(service.publish("outbox", ComponentState::Ok, None).await?);
        assert!(
            !service.publish("outbox", ComponentState::Ok, None).await?,
            "a heartbeat is not a transition"
        );
        assert!(
            service
                .publish("outbox", ComponentState::Unavailable, Some("no endpoint"))
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_gateway_nobody_has_heard_from_reports_nothing_not_health() -> TestResult {
        let service = HealthService::new(Arc::new(StubRepository::default()), FixedClock);

        assert_eq!(service.worst().await?, None);
        Ok(())
    }
}
