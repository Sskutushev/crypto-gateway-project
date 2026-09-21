use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use gateway_application::{ExpiryResult, ExpirySweeper, QuoteServiceError, RepositoryError};
use tokio::{
    sync::{Mutex, watch},
    time::{sleep, timeout},
};

use super::{ExpiryScheduler, SweepError};
use crate::config::{BatchConfig, RetryPolicy};

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Batch { quotes: u64, leases: u64 },
    Transient,
    Corrupt,
}

impl Outcome {
    fn into_result(self) -> Result<ExpiryResult, QuoteServiceError> {
        match self {
            Self::Batch { quotes, leases } => Ok(ExpiryResult {
                quotes_expired: quotes,
                leases_archived: leases,
            }),
            Self::Transient => Err(QuoteServiceError::Repository(RepositoryError::Unavailable(
                "connection reset by peer".to_owned(),
            ))),
            Self::Corrupt => Err(QuoteServiceError::Repository(RepositoryError::CorruptData(
                "quote expiry did not advance both attempt and payment intent".to_owned(),
            ))),
        }
    }
}

/// Replays a fixed script; an exhausted script means nothing is due.
#[derive(Debug)]
struct ScriptedSweeper {
    script: Mutex<VecDeque<Outcome>>,
    calls: AtomicUsize,
}

impl ScriptedSweeper {
    fn new(outcomes: impl IntoIterator<Item = Outcome>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(outcomes.into_iter().collect()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ExpirySweeper for ScriptedSweeper {
    async fn sweep_expired(&self, _limit: u32) -> Result<ExpiryResult, QuoteServiceError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let next = self.script.lock().await.pop_front();
        next.unwrap_or(Outcome::Batch {
            quotes: 0,
            leases: 0,
        })
        .into_result()
    }
}

/// Always fails the same way, so retry ceilings are observable.
#[derive(Debug)]
struct FailingSweeper {
    outcome: Outcome,
    calls: AtomicUsize,
}

impl FailingSweeper {
    fn new(outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ExpirySweeper for FailingSweeper {
    async fn sweep_expired(&self, _limit: u32) -> Result<ExpiryResult, QuoteServiceError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.outcome.into_result()
    }
}

/// Holds a batch open long enough for a second sweep to attempt to start.
#[derive(Debug)]
struct SlowSweeper {
    started: watch::Sender<bool>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl SlowSweeper {
    fn new() -> (Arc<Self>, watch::Receiver<bool>) {
        let (started, observer) = watch::channel(false);
        (
            Arc::new(Self {
                started,
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
            }),
            observer,
        )
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ExpirySweeper for SlowSweeper {
    async fn sweep_expired(&self, _limit: u32) -> Result<ExpiryResult, QuoteServiceError> {
        let in_flight = self
            .in_flight
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);
        let _started = self.started.send(true);
        sleep(Duration::from_secs(5)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(ExpiryResult {
            quotes_expired: 0,
            leases_archived: 0,
        })
    }
}

fn config(batch_limit: u32, max_batches_per_tick: u32) -> BatchConfig {
    BatchConfig {
        interval: Duration::from_secs(1),
        batch_limit,
        max_batches_per_tick,
        lease_seconds: 30,
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(4),
        },
    }
}

#[tokio::test]
async fn drains_batches_until_one_is_not_full() -> Result<(), Box<dyn Error>> {
    let sweeper = ScriptedSweeper::new([
        Outcome::Batch {
            quotes: 2,
            leases: 0,
        },
        Outcome::Batch {
            quotes: 0,
            leases: 2,
        },
        Outcome::Batch {
            quotes: 1,
            leases: 1,
        },
    ]);
    let scheduler = ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?;

    let report = scheduler.run_once().await?;

    assert_eq!(report.batches, 3);
    assert_eq!(report.quotes_expired, 3);
    assert_eq!(report.leases_archived, 3);
    assert!(report.drained);
    assert_eq!(sweeper.calls(), 3);
    let metrics = scheduler.metrics().snapshot();
    assert_eq!(metrics.batches_executed, 3);
    assert_eq!(metrics.runs_succeeded, 1);
    assert_eq!(metrics.backlog_left, 0);
    assert!(metrics.last_success_unix.is_some());
    Ok(())
}

#[tokio::test]
async fn stops_at_the_per_tick_ceiling_and_reports_the_backlog() -> Result<(), Box<dyn Error>> {
    let sweeper = ScriptedSweeper::new(std::iter::repeat_n(
        Outcome::Batch {
            quotes: 2,
            leases: 0,
        },
        5,
    ));
    let scheduler = ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 2))?;

    let report = scheduler.run_once().await?;

    assert_eq!(report.batches, 2);
    assert_eq!(report.quotes_expired, 4);
    assert!(!report.drained);
    assert_eq!(sweeper.calls(), 2);
    assert_eq!(scheduler.metrics().snapshot().backlog_left, 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retries_transient_storage_failures_and_counts_them() -> Result<(), Box<dyn Error>> {
    let sweeper = ScriptedSweeper::new([
        Outcome::Transient,
        Outcome::Transient,
        Outcome::Batch {
            quotes: 1,
            leases: 0,
        },
    ]);
    let scheduler = ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?;

    let report = scheduler.run_once().await?;

    assert_eq!(report.retries, 2);
    assert_eq!(report.quotes_expired, 1);
    assert!(report.drained);
    assert_eq!(sweeper.calls(), 3);
    let metrics = scheduler.metrics().snapshot();
    assert_eq!(metrics.transient_retries, 2);
    assert_eq!(metrics.runs_succeeded, 1);
    assert_eq!(metrics.runs_failed, 0);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn gives_up_after_the_retry_ceiling() -> Result<(), Box<dyn Error>> {
    let sweeper = FailingSweeper::new(Outcome::Transient);
    let scheduler = ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?;

    let outcome = scheduler.run_once().await;

    assert!(matches!(outcome, Err(SweepError::Sweep(_))));
    assert_eq!(sweeper.calls(), 3);
    let metrics = scheduler.metrics().snapshot();
    assert_eq!(metrics.transient_retries, 2);
    assert_eq!(metrics.runs_failed, 1);
    assert_eq!(metrics.consecutive_failures, 1);
    assert_eq!(metrics.last_success_unix, None);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn never_retries_an_invariant_violation() -> Result<(), Box<dyn Error>> {
    let sweeper = FailingSweeper::new(Outcome::Corrupt);
    let scheduler = ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?;

    let outcome = scheduler.run_once().await;

    assert!(matches!(outcome, Err(SweepError::Sweep(_))));
    assert_eq!(sweeper.calls(), 1);
    let metrics = scheduler.metrics().snapshot();
    assert_eq!(metrics.transient_retries, 0);
    assert_eq!(metrics.runs_failed, 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn refuses_to_run_two_sweeps_at_once() -> Result<(), Box<dyn Error>> {
    let (sweeper, mut started) = SlowSweeper::new();
    let scheduler = Arc::new(ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?);
    let first = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        async move { scheduler.run_once().await.map(|report| report.batches) }
    });
    started.changed().await?;

    let overlapping = scheduler.run_once().await;

    assert!(matches!(overlapping, Err(SweepError::Overlapping)));
    assert_eq!(first.await??, 1);
    assert_eq!(sweeper.max_in_flight(), 1);
    let metrics = scheduler.metrics().snapshot();
    assert_eq!(metrics.runs_skipped_overlapping, 1);
    assert_eq!(metrics.runs_started, 1);
    assert_eq!(metrics.runs_succeeded, 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn sweeps_on_every_interval_until_shutdown() -> Result<(), Box<dyn Error>> {
    let sweeper = ScriptedSweeper::new([]);
    let scheduler = Arc::new(ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?);
    let (shutdown, receiver) = watch::channel(false);
    let loop_task = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        async move { scheduler.run(receiver).await }
    });

    sleep(Duration::from_millis(2_500)).await;
    let runs_before_shutdown = sweeper.calls();
    shutdown.send(true)?;
    timeout(Duration::from_secs(1), loop_task).await??;

    assert_eq!(runs_before_shutdown, 3);
    assert_eq!(sweeper.calls(), runs_before_shutdown);
    assert_eq!(
        scheduler.metrics().snapshot().runs_succeeded,
        u64::try_from(runs_before_shutdown)?
    );
    Ok(())
}

#[tokio::test]
async fn stops_the_loop_when_the_shutdown_sender_is_dropped() -> Result<(), Box<dyn Error>> {
    let sweeper = ScriptedSweeper::new([]);
    let scheduler = ExpiryScheduler::new(Arc::clone(&sweeper), config(2, 10))?;
    let (shutdown, receiver) = watch::channel(false);
    drop(shutdown);

    timeout(Duration::from_secs(5), scheduler.run(receiver)).await?;

    // The first tick and the closed shutdown channel are ready together, so
    // the loop may or may not have swept once before stopping.
    assert!(sweeper.calls() <= 1);
    Ok(())
}
