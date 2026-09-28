//! One `PostgreSQL` scenario per reconciliation check.
//!
//! Each scenario drives the real pipeline to the state the check believes is
//! clean, breaks exactly the one thing the check exists to find, and asserts
//! the finding, the run row and the rail stop. Then it corrects the state and
//! proves the check is quiet again — and that the stop it opened is still open,
//! because a run that finds nothing is not a person saying it was explained.
//!
//! The settlement scenarios' fixtures are reused rather than reseeded: the
//! discrepancies are one mutation away from a settled payment, and the SQL
//! that makes a settled payment already lives there.

use std::{
    error::Error,
    sync::{Arc, Mutex},
};

use gateway_application::{
    Clock, ComponentLease, DiscrepancyKind, LeaseRepository, RECONCILER_COMPONENT,
    RECONCILIATION_HARD_STOP_REASON, ReconciliationKind, ReconciliationReport,
    ReconciliationService, ReconciliationWindow, RunStatus, SettlementService, VerificationService,
};
use gateway_domain::ObservedTransfer;
use sqlx::PgPool;
use time::Duration;
use uuid::Uuid;

use crate::{
    PostgresRepository,
    settlement::tests::{
        ASSET_ID, COLLECTOR_ID, ChainDouble, SOURCE_A, TestClock, VERIFIER_SOURCE, record_evidence,
        seed, source, transfer, two_intents,
    },
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

/// What the checks are keyed by once a payment has settled.
struct Settled {
    transfer_id: Uuid,
    intent_id: Uuid,
    /// A second obligation that was quoted and never paid.
    unpaid_intent_id: Uuid,
    lease: ComponentLease,
}

/// Records two independent readings of `paid` without verifying them: the
/// state the observer leaves behind and the verifier has not yet touched.
async fn observe(
    repository: &Arc<PostgresRepository>,
    clock: &TestClock,
    paid: &ObservedTransfer,
) -> Result<ComponentLease, Box<dyn Error>> {
    let lease = repository
        .acquire_component_lease("payments:tron", "pod-payments:boot-1", 300, clock.now())
        .await?
        .ok_or("the payment worker could not take its lease")?;
    record_evidence(repository, &lease, paid, clock.now()).await?;
    Ok(lease)
}

async fn verify(
    repository: &Arc<PostgresRepository>,
    clock: &TestClock,
    lease: &ComponentLease,
    paid: ObservedTransfer,
) -> TestResult {
    let verifier = VerificationService::new(
        Arc::clone(repository),
        Arc::new(ChainDouble(Mutex::new(paid))),
        clock.clone(),
        source(VERIFIER_SOURCE, "verifier"),
        "verifier-test",
        "parser-test",
    );
    clock.advance(Duration::seconds(5));
    let report = verifier.verify_pending(lease, 50).await?;
    assert_eq!(report.verified, 1);
    Ok(())
}

/// Drives one exact payment through evidence, verification and settlement.
async fn settle_one(
    pool: &PgPool,
    repository: &Arc<PostgresRepository>,
    clock: &TestClock,
) -> Result<Settled, Box<dyn Error>> {
    let (first, second) = two_intents(repository, clock).await?;
    let paid = transfer(first.expected.to_string().as_str(), clock.now())?;
    let lease = observe(repository, clock, &paid).await?;
    verify(repository, clock, &lease, paid).await?;
    let report = SettlementService::new(Arc::clone(repository), clock.clone())
        .settle_pending(&lease, 50)
        .await?;
    assert_eq!(report.settled, 1);
    let transfer_id: Uuid = sqlx::query_scalar("SELECT id FROM chain_transfers")
        .fetch_one(pool)
        .await?;
    Ok(Settled {
        transfer_id,
        intent_id: first.intent_id,
        unpaid_intent_id: second.intent_id,
        lease,
    })
}

/// Runs one incremental pass under the scenario's clock.
///
/// The window ends at the run's own "now", exclusively. A decision stamped at
/// the same instant as the run would fall just outside it, so the reconciler
/// always runs a step after the pipeline it is checking.
async fn reconcile(
    repository: &Arc<PostgresRepository>,
    clock: &TestClock,
    window: ReconciliationWindow,
) -> Result<ReconciliationReport, Box<dyn Error>> {
    clock.advance(Duration::minutes(1));
    Ok(
        ReconciliationService::new(Arc::clone(repository), clock.clone(), window)
            .run_once(ReconciliationKind::Incremental)
            .await?,
    )
}

/// Exactly one finding of `kind`, keyed as expected, with the run row and the
/// stored discrepancy agreeing with the kind about money.
async fn assert_finding(
    pool: &PgPool,
    report: &ReconciliationReport,
    kind: DiscrepancyKind,
    transfer_id: Option<Uuid>,
    payment_intent_id: Option<Uuid>,
    asset_id: Option<Uuid>,
) -> TestResult {
    assert_eq!(
        report.discrepancies.len(),
        1,
        "expected one finding, got {:?}",
        report.discrepancies
    );
    let found = report
        .discrepancies
        .first()
        .ok_or("the run reported no finding")?;
    assert_eq!(found.kind, kind);
    assert_eq!(found.transfer_id, transfer_id);
    assert_eq!(found.payment_intent_id, payment_intent_id);
    assert_eq!(found.asset_id, asset_id);

    let expected_status = if kind.affects_money() {
        RunStatus::HardStop
    } else {
        RunStatus::Drift
    };
    assert_eq!(report.status, expected_status);
    assert_eq!(report.money_discrepancies, u32::from(kind.affects_money()));

    let (status, money_count, discrepancy_count): (String, i32, i32) = sqlx::query_as(
        "SELECT status, money_discrepancy_count, discrepancy_count \
           FROM reconciliation_runs WHERE id = $1",
    )
    .bind(report.run_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(status, expected_status.as_str());
    assert_eq!(money_count, i32::from(kind.affects_money()));
    assert_eq!(discrepancy_count, 1);

    let (stored_kind, money_affected): (String, bool) = sqlx::query_as(
        "SELECT kind, money_affected FROM reconciliation_discrepancies WHERE run_id = $1",
    )
    .bind(report.run_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(stored_kind, kind.as_str());
    assert_eq!(money_affected, kind.affects_money());
    Ok(())
}

/// A run that found nothing, recorded as such.
async fn assert_quiet(pool: &PgPool, report: &ReconciliationReport) -> TestResult {
    assert!(
        report.discrepancies.is_empty(),
        "expected a clean run, got {:?}",
        report.discrepancies
    );
    assert_eq!(report.status, RunStatus::Ok);
    assert!(report.stopped_assets.is_empty());
    let (status, discrepancy_count): (String, i32) =
        sqlx::query_as("SELECT status, discrepancy_count FROM reconciliation_runs WHERE id = $1")
            .bind(report.run_id)
            .fetch_one(pool)
            .await?;
    assert_eq!(status, RunStatus::Ok.as_str());
    assert_eq!(discrepancy_count, 0);
    Ok(())
}

/// The rail stops nobody has cleared: asset, reason, detail, who opened it.
async fn open_rail_stops(
    pool: &PgPool,
) -> Result<Vec<(Uuid, String, Option<String>, String)>, Box<dyn Error>> {
    Ok(sqlx::query_as(
        "SELECT asset_id, reason_code, detail, opened_by FROM rail_stops \
          WHERE cleared_at IS NULL ORDER BY opened_at",
    )
    .fetch_all(pool)
    .await?)
}

/// A money finding closed the rail under the reconciler's name, and a later
/// clean run left it closed.
async fn assert_rail_closed_by(pool: &PgPool, kind: DiscrepancyKind) -> TestResult {
    assert_eq!(
        open_rail_stops(pool).await?,
        vec![(
            ASSET_ID,
            RECONCILIATION_HARD_STOP_REASON.to_owned(),
            Some(kind.as_str().to_owned()),
            RECONCILER_COMPONENT.to_owned(),
        )]
    );
    Ok(())
}

async fn reconciler_state(pool: &PgPool) -> Result<String, Box<dyn Error>> {
    Ok(
        sqlx::query_scalar("SELECT state FROM component_health WHERE component = $1")
            .bind(RECONCILER_COMPONENT)
            .fetch_one(pool)
            .await?,
    )
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_settled_payment_reconciles_clean() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    settle_one(&pool, &repository, &clock).await?;

    let report = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;

    assert_quiet(&pool, &report).await?;
    assert_eq!(report.transfers_examined, 1);
    assert_eq!(report.intents_examined, 2);
    assert!(open_rail_stops(&pool).await?.is_empty());
    assert_eq!(reconciler_state(&pool).await?, "ok");
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn readings_that_never_became_a_fact_are_a_finding() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let window = ReconciliationWindow::default();
    let (first, _) = two_intents(&repository, &clock).await?;
    let paid = transfer(first.expected.to_string().as_str(), clock.now())?;
    let lease = observe(&repository, &clock, &paid).await?;

    // Inside the grace period the readings are a payment still in flight.
    let early = reconcile(&repository, &clock, window).await?;
    assert_quiet(&pool, &early).await?;

    clock.advance(window.canonicalization_grace);
    let report = reconcile(&repository, &clock, window).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::ObservedNotCanonical,
        None,
        None,
        Some(ASSET_ID),
    )
    .await?;
    assert!(report.stopped_assets.is_empty());
    assert!(open_rail_stops(&pool).await?.is_empty());
    assert_eq!(reconciler_state(&pool).await?, "degraded");

    // The verifier catches up: the readings now have their fact.
    verify(&repository, &clock, &lease, paid).await?;
    let again = reconcile(&repository, &clock, window).await?;
    assert_quiet(&pool, &again).await?;
    assert_eq!(reconciler_state(&pool).await?, "ok");
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn an_allocation_larger_than_its_transfer_stops_the_rail() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let settled = settle_one(&pool, &repository, &clock).await?;

    // The settlement path refuses this; the check exists for the day
    // something else does not.
    sqlx::query(
        "UPDATE chain_transfer_processing SET allocated_raw = allocated_raw + 1 \
          WHERE transfer_id = $1",
    )
    .bind(settled.transfer_id)
    .execute(&pool)
    .await?;
    let report = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::AllocationExceedsTransfer,
        Some(settled.transfer_id),
        None,
        Some(ASSET_ID),
    )
    .await?;
    assert_eq!(report.stopped_assets, vec![ASSET_ID]);
    assert_rail_closed_by(&pool, DiscrepancyKind::AllocationExceedsTransfer).await?;
    assert_eq!(reconciler_state(&pool).await?, "stopped");

    sqlx::query(
        "UPDATE chain_transfer_processing AS processing \
            SET allocated_raw = transfer.amount_raw \
           FROM chain_transfers AS transfer \
          WHERE transfer.id = processing.transfer_id AND processing.transfer_id = $1",
    )
    .bind(settled.transfer_id)
    .execute(&pool)
    .await?;
    let again = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;
    assert_quiet(&pool, &again).await?;
    assert_rail_closed_by(&pool, DiscrepancyKind::AllocationExceedsTransfer).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_settled_payment_nobody_claimed_stops_the_rail() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let settled = settle_one(&pool, &repository, &clock).await?;

    sqlx::query("UPDATE payment_fulfillments SET status = 'failed' WHERE payment_intent_id = $1")
        .bind(settled.intent_id)
        .execute(&pool)
        .await?;
    let report = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::SettledNotFulfilled,
        Some(settled.transfer_id),
        Some(settled.intent_id),
        Some(ASSET_ID),
    )
    .await?;
    assert_eq!(report.stopped_assets, vec![ASSET_ID]);
    assert_rail_closed_by(&pool, DiscrepancyKind::SettledNotFulfilled).await?;

    sqlx::query("UPDATE payment_fulfillments SET status = 'claimed' WHERE payment_intent_id = $1")
        .bind(settled.intent_id)
        .execute(&pool)
        .await?;
    let again = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;
    assert_quiet(&pool, &again).await?;
    assert_rail_closed_by(&pool, DiscrepancyKind::SettledNotFulfilled).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_fulfilment_with_no_settlement_behind_it_is_a_hard_stop() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let settled = settle_one(&pool, &repository, &clock).await?;

    // The second obligation was quoted and never paid, yet a fulfilment claim
    // appears for it.
    sqlx::query(
        "INSERT INTO payment_fulfillments (payment_intent_id, merchant_id, status, claimed_at) \
         SELECT id, merchant_id, 'claimed', $2 FROM payment_intents WHERE id = $1",
    )
    .bind(settled.unpaid_intent_id)
    .bind(clock.now())
    .execute(&pool)
    .await?;
    let report = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::FulfilledNotSettled,
        None,
        Some(settled.unpaid_intent_id),
        Some(ASSET_ID),
    )
    .await?;
    // A fulfilment names no transfer, but the intent's immutable quote names
    // the asset. Reconciliation recovers it and closes that rail fail-closed.
    assert_eq!(report.stopped_assets, vec![ASSET_ID]);
    assert_rail_closed_by(&pool, DiscrepancyKind::FulfilledNotSettled).await?;
    assert_eq!(reconciler_state(&pool).await?, "stopped");

    sqlx::query("DELETE FROM payment_fulfillments WHERE payment_intent_id = $1")
        .bind(settled.unpaid_intent_id)
        .execute(&pool)
        .await?;
    let again = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;
    assert_quiet(&pool, &again).await?;
    assert_rail_closed_by(&pool, DiscrepancyKind::FulfilledNotSettled).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn an_allocation_on_an_invalidated_transfer_stops_the_rail() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let settled = settle_one(&pool, &repository, &clock).await?;
    let state_before: String =
        sqlx::query_scalar("SELECT state FROM chain_transfer_state_current WHERE transfer_id = $1")
            .bind(settled.transfer_id)
            .fetch_one(&pool)
            .await?;

    // The chain took the block back after the money had been allocated.
    sqlx::query(
        "UPDATE chain_transfer_state_current SET state = 'invalidated' WHERE transfer_id = $1",
    )
    .bind(settled.transfer_id)
    .execute(&pool)
    .await?;
    let report = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::AllocatedOnInvalidatedTransfer,
        Some(settled.transfer_id),
        Some(settled.intent_id),
        Some(ASSET_ID),
    )
    .await?;
    assert_eq!(report.stopped_assets, vec![ASSET_ID]);
    assert_rail_closed_by(&pool, DiscrepancyKind::AllocatedOnInvalidatedTransfer).await?;

    sqlx::query("UPDATE chain_transfer_state_current SET state = $2 WHERE transfer_id = $1")
        .bind(settled.transfer_id)
        .bind(state_before)
        .execute(&pool)
        .await?;
    let again = reconcile(&repository, &clock, ReconciliationWindow::default()).await?;
    assert_quiet(&pool, &again).await?;
    assert_rail_closed_by(&pool, DiscrepancyKind::AllocatedOnInvalidatedTransfer).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn unmatched_money_that_waited_too_long_is_a_finding() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let window = ReconciliationWindow::default();
    let (first, _) = two_intents(&repository, &clock).await?;

    // An amount no reservation explains: recorded, queued for a person.
    let overpaid = first.expected.checked_add_u32(500)?;
    let paid = transfer(overpaid.to_string().as_str(), clock.now())?;
    let lease = observe(&repository, &clock, &paid).await?;
    verify(&repository, &clock, &lease, paid).await?;
    let settlement = SettlementService::new(Arc::clone(&repository), clock.clone())
        .settle_pending(&lease, 50)
        .await?;
    assert_eq!(settlement.unmatched, 1);
    let transfer_id: Uuid = sqlx::query_scalar("SELECT id FROM chain_transfers")
        .fetch_one(&pool)
        .await?;

    // Fresh unmatched money is a queue entry, not a finding.
    let early = reconcile(&repository, &clock, window).await?;
    assert_quiet(&pool, &early).await?;

    sqlx::query("UPDATE chain_transfer_processing SET updated_at = $2 WHERE transfer_id = $1")
        .bind(transfer_id)
        .bind(clock.now() - window.unmatched_grace - Duration::minutes(1))
        .execute(&pool)
        .await?;
    let report = reconcile(&repository, &clock, window).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::UnmatchedInboundAging,
        Some(transfer_id),
        None,
        Some(ASSET_ID),
    )
    .await?;
    assert!(report.stopped_assets.is_empty());
    assert!(open_rail_stops(&pool).await?.is_empty());

    sqlx::query("UPDATE chain_transfer_processing SET updated_at = $2 WHERE transfer_id = $1")
        .bind(transfer_id)
        .bind(clock.now())
        .execute(&pool)
        .await?;
    let again = reconcile(&repository, &clock, window).await?;
    assert_quiet(&pool, &again).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_payment_held_for_too_long_is_a_finding() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let window = ReconciliationWindow::default();
    let settled = settle_one(&pool, &repository, &clock).await?;
    let state_before: String = sqlx::query_scalar(
        "SELECT processing_state FROM chain_transfer_processing WHERE transfer_id = $1",
    )
    .bind(settled.transfer_id)
    .fetch_one(&pool)
    .await?;

    sqlx::query(
        "UPDATE chain_transfer_processing SET processing_state = 'held', updated_at = $2 \
          WHERE transfer_id = $1",
    )
    .bind(settled.transfer_id)
    .bind(clock.now() - window.held_grace - Duration::minutes(1))
    .execute(&pool)
    .await?;
    let report = reconcile(&repository, &clock, window).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::HeldPaymentAging,
        Some(settled.transfer_id),
        None,
        Some(ASSET_ID),
    )
    .await?;
    assert!(report.stopped_assets.is_empty());
    assert!(open_rail_stops(&pool).await?.is_empty());

    sqlx::query(
        "UPDATE chain_transfer_processing SET processing_state = $2, updated_at = $3 \
          WHERE transfer_id = $1",
    )
    .bind(settled.transfer_id)
    .bind(state_before)
    .bind(clock.now())
    .execute(&pool)
    .await?;
    let again = reconcile(&repository, &clock, window).await?;
    assert_quiet(&pool, &again).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_source_far_behind_its_own_head_is_a_finding() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(time::OffsetDateTime::now_utc());
    let settled = settle_one(&pool, &repository, &clock).await?;
    // The lag is measured against the head the source itself reported with
    // its readings, so the threshold is the window's, not a magic number.
    let window = ReconciliationWindow {
        max_cursor_lag_blocks: 100,
        ..ReconciliationWindow::default()
    };
    let source_head: i64 =
        sqlx::query_scalar("SELECT max(source_head) FROM chain_observations WHERE source_id = $1")
            .bind(SOURCE_A)
            .fetch_one(&pool)
            .await?;
    assert!(source_head > window.max_cursor_lag_blocks + 1);

    sqlx::query(
        "INSERT INTO chain_cursors (source_id, observation_kind, collector_address_id, \
                                    cursor_kind, cursor_value, fence_token, updated_at) \
         VALUES ($1, 'cursor_scan', $2, 'block', '1', $3, $4)",
    )
    .bind(SOURCE_A)
    .bind(COLLECTOR_ID)
    .bind(settled.lease.fence_token)
    .bind(clock.now())
    .execute(&pool)
    .await?;
    let report = reconcile(&repository, &clock, window).await?;

    assert_finding(
        &pool,
        &report,
        DiscrepancyKind::ObserverBehind,
        None,
        None,
        None,
    )
    .await?;
    let found = report
        .discrepancies
        .first()
        .ok_or("the run reported no finding")?;
    assert_eq!(found.detail["source_head"], source_head);
    assert_eq!(found.detail["cursor_value"], "1");
    assert!(report.stopped_assets.is_empty());
    assert!(open_rail_stops(&pool).await?.is_empty());

    sqlx::query("UPDATE chain_cursors SET cursor_value = $2 WHERE source_id = $1")
        .bind(SOURCE_A)
        .bind(source_head.to_string())
        .execute(&pool)
        .await?;
    let again = reconcile(&repository, &clock, window).await?;
    assert_quiet(&pool, &again).await?;
    Ok(())
}
