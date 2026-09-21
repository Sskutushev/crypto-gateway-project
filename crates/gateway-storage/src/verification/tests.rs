use std::{
    error::Error,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_application::{
    ChainEventKey, ChainReader, ChainReaderError, ChainSource, Clock, LeaseRepository,
    ObservationRepository, ResolvedObservation, VerificationService,
};
use gateway_domain::{
    AddressKey, ChainEnvironment, ExecutionStatus, ObservationKind, ObservedTransfer, RawAmount,
    SourceFinality, TxHash,
};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET_ID: Uuid = Uuid::from_u128(8_101);
const COLLECTOR_ID: Uuid = Uuid::from_u128(8_201);
const SOURCE_A: Uuid = Uuid::from_u128(8_001);
const SOURCE_B: Uuid = Uuid::from_u128(8_002);
const SOURCE_C: Uuid = Uuid::from_u128(8_003);
const VERIFIER_SOURCE: Uuid = Uuid::from_u128(8_004);
const POLICY_ID: Uuid = Uuid::from_u128(8_301);

#[derive(Debug, Clone)]
struct TestClock(Arc<Mutex<OffsetDateTime>>);

impl TestClock {
    fn at(moment: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(moment)))
    }

    fn advance(&self, by: Duration) {
        if let Ok(mut moment) = self.0.lock() {
            *moment += by;
        }
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        self.0
            .lock()
            .map(|moment| *moment)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

#[derive(Debug)]
struct ChainDouble(ObservedTransfer);

#[async_trait]
impl ChainReader for ChainDouble {
    async fn lookup(
        &self,
        _event: &ChainEventKey,
    ) -> Result<Option<ObservedTransfer>, ChainReaderError> {
        Ok(Some(self.0.clone()))
    }
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn independent_evidence_becomes_one_canonical_fact_and_a_later_disagreement_is_recorded()
-> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(OffsetDateTime::now_utc());
    let lease = repository
        .acquire_component_lease("verifier:tron", "pod-verifier:boot-1", 120, clock.now())
        .await?
        .ok_or("the verifier could not take its lease")?;

    // Two independent providers report the same transfer.
    for (source_id, group) in [(SOURCE_A, "group-a"), (SOURCE_B, "group-b")] {
        let source = source(source_id, group);
        repository
            .record_observations(
                &source,
                &lease,
                &[observation(&source, transfer()?, clock.now())],
                None,
            )
            .await?;
    }

    let service = VerificationService::new(
        Arc::clone(&repository),
        Arc::new(ChainDouble(transfer()?)),
        clock.clone(),
        source(VERIFIER_SOURCE, "verifier"),
        "verifier-test",
        "parser-test",
    );

    clock.advance(Duration::seconds(5));
    let report = service.verify_pending(&lease, 50).await?;

    assert_eq!(report.examined, 1);
    assert_eq!(report.rereads_performed, 1);
    assert_eq!(report.verified, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfers").await?,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfer_attestations").await?,
        3
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfer_processing").await?,
        1
    );
    let (state, version): (String, i64) =
        sqlx::query_as("SELECT state, state_version FROM chain_transfer_state_current LIMIT 1")
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, "finalized");
    // canonical, then confirmed, then finalized: every step is its own event.
    assert_eq!(version, 3);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfer_state_events").await?,
        3
    );
    let verdict: String = sqlx::query_scalar("SELECT verdict FROM chain_event_verdicts LIMIT 1")
        .fetch_one(&pool)
        .await?;
    assert_eq!(verdict, "verified");

    // Nothing new arrived, so nothing is decided again.
    clock.advance(Duration::seconds(5));
    let idle = service.verify_pending(&lease, 50).await?;
    assert_eq!(idle.examined, 0);

    // A third source now claims a different amount for the same event.
    clock.advance(Duration::seconds(5));
    let liar = source(SOURCE_C, "group-c");
    let fabricated = ObservedTransfer {
        amount_raw: RawAmount::from_str("50000000000")?,
        ..transfer()?
    };
    repository
        .record_observations(
            &liar,
            &lease,
            &[observation(&liar, fabricated, clock.now())],
            None,
        )
        .await?;

    clock.advance(Duration::seconds(5));
    let contested = service.verify_pending(&lease, 50).await?;

    assert_eq!(contested.conflicted, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_observation_conflicts").await?,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM chain_observation_conflict_items"
        )
        .await?,
        4
    );
    // The canonical fact that was already proven is not destroyed by a later
    // disagreement, and no second fact is created.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfers").await?,
        1
    );
    let verdict: String = sqlx::query_scalar("SELECT verdict FROM chain_event_verdicts LIMIT 1")
        .fetch_one(&pool)
        .await?;
    assert_eq!(verdict, "conflicted");
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn one_provider_reporting_twice_never_becomes_a_fact() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(OffsetDateTime::now_utc());
    let lease = repository
        .acquire_component_lease("verifier:tron", "pod-verifier:boot-1", 120, clock.now())
        .await?
        .ok_or("the verifier could not take its lease")?;

    // The same provider group writes twice under two source rows.
    for source_id in [SOURCE_A, SOURCE_B] {
        let source = source(source_id, "group-a");
        repository
            .record_observations(
                &source,
                &lease,
                &[observation(&source, transfer()?, clock.now())],
                None,
            )
            .await?;
    }
    sqlx::query("UPDATE chain_sources SET provider_group = 'group-a' WHERE id = $1")
        .bind(SOURCE_B)
        .execute(&pool)
        .await?;

    let service = VerificationService::new(
        Arc::clone(&repository),
        Arc::new(ChainDouble(transfer()?)),
        clock.clone(),
        source(VERIFIER_SOURCE, "group-a"),
        "verifier-test",
        "parser-test",
    );
    sqlx::query("UPDATE chain_sources SET provider_group = 'group-a' WHERE id = $1")
        .bind(VERIFIER_SOURCE)
        .execute(&pool)
        .await?;

    clock.advance(Duration::seconds(5));
    let report = service.verify_pending(&lease, 50).await?;

    assert_eq!(report.verified, 0);
    assert_eq!(report.insufficient, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfers").await?,
        0
    );
    let (verdict, reason): (String, Option<String>) =
        sqlx::query_as("SELECT verdict, reason FROM chain_event_verdicts LIMIT 1")
            .fetch_one(&pool)
            .await?;
    assert_eq!(verdict, "insufficient");
    assert_eq!(reason.as_deref(), Some("not_enough_independent_groups"));
    Ok(())
}

fn source(id: Uuid, group: &str) -> ChainSource {
    ChainSource {
        id,
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        source_key: format!("source-{id}"),
        provider_group: group.to_owned(),
        kind: gateway_application::SourceKind::IndexedApi,
        db_principal: "gateway".to_owned(),
        state: gateway_application::SourceState::Active,
    }
}

fn transfer() -> Result<ObservedTransfer, Box<dyn Error>> {
    Ok(ObservedTransfer {
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        tx_hash: TxHash::new("abc123")?,
        event_index: 0,
        block_number: Some(100),
        block_hash: Some("block-100".to_owned()),
        parent_hash: Some("block-99".to_owned()),
        block_time: Some(OffsetDateTime::now_utc() - Duration::minutes(5)),
        token_key: AddressKey::new([7_u8; 20])?,
        token_display: "USDT".to_owned(),
        from_address: AddressKey::new([9_u8; 21])?,
        from_address_text: "TFrom".to_owned(),
        to_address: AddressKey::new([3_u8; 21])?,
        to_address_text: "TCollector".to_owned(),
        amount_raw: RawAmount::from_str("273001427")?,
        decimals: 6,
        memo: None,
        execution_status: ExecutionStatus::Success,
        source_finality: SourceFinality::Finalized,
        source_head: Some(130),
        evidence_sha256: "a".repeat(64),
        evidence_uri: None,
    })
}

fn observation(
    source: &ChainSource,
    transfer: ObservedTransfer,
    observed_at: OffsetDateTime,
) -> ResolvedObservation {
    let semantic_hash = transfer.semantic_hash(source.id, ObservationKind::CursorScan);
    ResolvedObservation {
        transfer,
        kind: ObservationKind::CursorScan,
        collector_address_id: COLLECTOR_ID,
        asset_id: Some(ASSET_ID),
        semantic_hash,
        observer_version: "observer-test".to_owned(),
        parser_version: "parser-test".to_owned(),
        fence_token: 1,
        observed_at,
    }
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query("TRUNCATE chain_sources, chain_assets, merchants, component_leases, chain_finality_policies CASCADE")
        .execute(pool)
        .await?;
    sqlx::query(
        r"
        INSERT INTO chain_assets (
            id, chain, network, chain_environment, contract_address_key,
            display_symbol, decimals, status, pinned_sha256, approved_by
        ) VALUES (
            $1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active',
            encode(sha256($2), 'hex'), 'test'
        )
        ",
    )
    .bind(ASSET_ID)
    .bind([7_u8; 20].as_slice())
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO collector_addresses (
            id, asset_id, address_key, address_text, state, valid_from,
            pinned_sha256, approved_by
        ) VALUES ($1, $2, $3, 'TCollector', 'active', $4, encode(sha256($3), 'hex'), 'test')
        ",
    )
    .bind(COLLECTOR_ID)
    .bind(ASSET_ID)
    .bind([3_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(pool)
    .await?;
    for (id, group) in [
        (SOURCE_A, "group-a"),
        (SOURCE_B, "group-b"),
        (SOURCE_C, "group-c"),
        (VERIFIER_SOURCE, "verifier"),
    ] {
        sqlx::query(
            r"
            INSERT INTO chain_sources (
                id, chain, network, chain_environment, source_key, provider_group,
                kind, db_principal, state, valid_from
            ) VALUES ($1, 'tron', 'nile', 'testnet', $2, $3, 'indexed_api', $4, 'active', $5)
            ",
        )
        .bind(id)
        .bind(format!("source-{id}"))
        .bind(group)
        .bind(format!("gateway_principal_{id}"))
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
    }
    // The fixture writes every source from one connection, so the sources
    // declare that shared principal explicitly. Production sources keep the
    // dedicated-principal requirement and cannot share a role.
    sqlx::query(
        "UPDATE chain_sources SET db_principal = session_user, requires_dedicated_principal = FALSE",
    )
        .execute(pool)
        .await?;
    sqlx::query(
        r"
        INSERT INTO chain_finality_policies (
            id, chain, network, chain_environment, version, status, min_confirmations,
            required_source_finality, min_independent_groups, max_evidence_age_seconds,
            observed_at
        ) VALUES ($1, 'tron', 'nile', 'testnet', 'finality-v1', 'active', 19,
                  'finalized', 2, 3600, $2)
        ",
    )
    .bind(POLICY_ID)
    .bind(OffsetDateTime::now_utc())
    .execute(pool)
    .await?;
    Ok(())
}

async fn count(pool: &PgPool, query: &str) -> Result<i64, Box<dyn Error>> {
    // The callers pass fixed literals from this module, never external input.
    Ok(sqlx::query_scalar(query).fetch_one(pool).await?)
}
