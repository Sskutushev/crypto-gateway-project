use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use gateway_application::{ComponentLease, LeaseRepository, RepositoryError};
use time::OffsetDateTime;
use tokio::sync::watch;

use super::{BatchOutcome, LeasedWorker, RunError, WorkerError, WorkerLoop};
use crate::config::{BatchConfig, RetryPolicy};

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Debug)]
struct FakeLeases {
    granted: bool,
    calls: AtomicU32,
}

impl FakeLeases {
    fn granting() -> Arc<Self> {
        Arc::new(Self {
            granted: true,
            calls: AtomicU32::new(0),
        })
    }

    fn held_elsewhere() -> Arc<Self> {
        Arc::new(Self {
            granted: false,
            calls: AtomicU32::new(0),
        })
    }
}

#[async_trait]
impl LeaseRepository for FakeLeases {
    async fn acquire_component_lease(
        &self,
        component: &str,
        holder: &str,
        ttl_seconds: i64,
        now: OffsetDateTime,
    ) -> Result<Option<ComponentLease>, RepositoryError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if !self.granted {
            return Ok(None);
        }
        Ok(Some(ComponentLease {
            component: component.to_owned(),
            holder: holder.to_owned(),
            fence_token: 3,
            lease_until: now + time::Duration::seconds(ttl_seconds),
        }))
    }
}

#[derive(Debug)]
struct ScriptedWorker {
    outcomes: Vec<Result<BatchOutcome, &'static str>>,
    transient: bool,
    calls: AtomicU32,
}

impl ScriptedWorker {
    fn new(outcomes: Vec<Result<BatchOutcome, &'static str>>, transient: bool) -> Arc<Self> {
        Arc::new(Self {
            outcomes,
            transient,
            calls: AtomicU32::new(0),
        })
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl LeasedWorker for ScriptedWorker {
    fn component(&self) -> &'static str {
        "test:component"
    }

    fn name(&self) -> &'static str {
        "test-worker"
    }

    async fn run_batch(&self, _lease: &ComponentLease) -> Result<BatchOutcome, WorkerError> {
        let index = self.calls.fetch_add(1, Ordering::Relaxed) as usize;
        match self.outcomes.get(index) {
            Some(Ok(outcome)) => Ok(*outcome),
            Some(Err(message)) => Err(if self.transient {
                WorkerError::Transient((*message).to_owned())
            } else {
                WorkerError::Permanent((*message).to_owned())
            }),
            None => Ok(BatchOutcome {
                processed: 0,
                drained: true,
            }),
        }
    }
}

fn config() -> BatchConfig {
    BatchConfig {
        interval: Duration::from_secs(1),
        batch_limit: 10,
        max_batches_per_tick: 3,
        lease_seconds: 30,
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(4),
        },
    }
}

#[tokio::test]
async fn a_worker_runs_batches_until_the_queue_is_empty() -> TestResult {
    let worker = ScriptedWorker::new(
        vec![
            Ok(BatchOutcome {
                processed: 10,
                drained: false,
            }),
            Ok(BatchOutcome {
                processed: 4,
                drained: true,
            }),
        ],
        false,
    );
    let leases = FakeLeases::granting();
    let loop_ = WorkerLoop::new(Arc::clone(&worker), leases, "pod:boot", config())?;

    let report = loop_.run_once().await?.ok_or("the lease was not granted")?;

    assert_eq!(report.batches, 2);
    assert_eq!(report.processed, 14);
    assert!(report.drained);
    assert_eq!(worker.calls(), 2);
    let metrics = loop_.metrics().snapshot();
    assert_eq!(metrics.runs_succeeded, 1);
    assert_eq!(metrics.items_processed, 14);
    assert_eq!(metrics.backlog_left, 0);
    Ok(())
}

#[tokio::test]
async fn a_worker_without_the_lease_does_nothing_and_says_so() -> TestResult {
    let worker = ScriptedWorker::new(Vec::new(), false);
    let loop_ = WorkerLoop::new(
        Arc::clone(&worker),
        FakeLeases::held_elsewhere(),
        "pod:boot",
        config(),
    )?;

    let report = loop_.run_once().await?;

    assert!(report.is_none());
    assert_eq!(worker.calls(), 0);
    let metrics = loop_.metrics().snapshot();
    assert_eq!(metrics.runs_not_leader, 1);
    assert_eq!(metrics.runs_started, 0);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn transient_failures_are_retried_and_permanent_ones_are_not() -> TestResult {
    let flaky = ScriptedWorker::new(
        vec![
            Err("connection reset"),
            Ok(BatchOutcome {
                processed: 2,
                drained: true,
            }),
        ],
        true,
    );
    let retried = WorkerLoop::new(
        Arc::clone(&flaky),
        FakeLeases::granting(),
        "pod:boot",
        config(),
    )?;
    let report = retried
        .run_once()
        .await?
        .ok_or("the lease was not granted")?;
    assert_eq!(report.retries, 1);
    assert_eq!(report.processed, 2);
    assert_eq!(retried.metrics().snapshot().transient_retries, 1);

    let broken = ScriptedWorker::new(vec![Err("invariant violated")], false);
    let stopped = WorkerLoop::new(
        Arc::clone(&broken),
        FakeLeases::granting(),
        "pod:boot",
        config(),
    )?;
    let outcome = stopped.run_once().await;
    assert!(matches!(outcome, Err(RunError::Work(_))));
    assert_eq!(broken.calls(), 1);
    let metrics = stopped.metrics().snapshot();
    assert_eq!(metrics.runs_failed, 1);
    assert_eq!(metrics.consecutive_failures, 1);
    Ok(())
}

#[tokio::test]
async fn a_backlog_is_reported_instead_of_being_hidden() -> TestResult {
    let worker = ScriptedWorker::new(
        vec![
            Ok(BatchOutcome {
                processed: 10,
                drained: false,
            }),
            Ok(BatchOutcome {
                processed: 10,
                drained: false,
            }),
            Ok(BatchOutcome {
                processed: 10,
                drained: false,
            }),
        ],
        false,
    );
    let loop_ = WorkerLoop::new(
        Arc::clone(&worker),
        FakeLeases::granting(),
        "pod:boot",
        config(),
    )?;

    let report = loop_.run_once().await?.ok_or("the lease was not granted")?;

    assert_eq!(report.batches, 3);
    assert!(!report.drained);
    assert_eq!(loop_.metrics().snapshot().backlog_left, 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn the_loop_ticks_and_stops_on_shutdown() -> TestResult {
    let worker = ScriptedWorker::new(Vec::new(), false);
    let loop_ = Arc::new(WorkerLoop::new(
        Arc::clone(&worker),
        FakeLeases::granting(),
        "pod:boot",
        config(),
    )?);
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::spawn({
        let loop_ = Arc::clone(&loop_);
        async move { loop_.run(receiver).await }
    });

    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let runs = loop_.metrics().snapshot().runs_succeeded;
    shutdown.send(true)?;
    tokio::time::timeout(Duration::from_secs(1), task).await??;

    assert_eq!(runs, 3);
    Ok(())
}

#[test]
fn a_lease_that_could_expire_mid_run_is_refused() {
    let too_short = BatchConfig {
        lease_seconds: 1,
        ..config()
    };
    assert!(too_short.validated().is_err());
}
