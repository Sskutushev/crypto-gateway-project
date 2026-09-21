use std::{error::Error, str::FromStr};

use gateway_application::{
    ChainSource, ComponentLease, CursorKind, CursorPosition, ObservationRepository,
    RepositoryError, ResolvedObservation,
};
use gateway_domain::{
    AddressKey, ExecutionStatus, ObservationKind, ObservedTransfer, RawAmount, SourceFinality,
    TxHash,
};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect, database_url_as},
};

type TestResult = Result<(), Box<dyn Error>>;

const SOURCE_A: Uuid = Uuid::from_u128(9_001);
const SOURCE_B: Uuid = Uuid::from_u128(9_002);
const ASSET_ID: Uuid = Uuid::from_u128(9_101);
const COLLECTOR_ID: Uuid = Uuid::from_u128(9_201);
const OBSERVER_ROLE: &str = "gateway_observer_test";
const OBSERVER_PASSWORD: &str = "gateway_observer_test_password";

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn evidence_is_deduplicated_and_cursors_are_fenced() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = PostgresRepository::new(pool.clone());
    // The lease guard reads the database clock, so the scenario uses real time.
    let now = OffsetDateTime::now_utc();

    let source = repository
        .find_source("provider-a")
        .await?
        .ok_or("the seeded source was not found")?;
    assert_eq!(source.provider_group, "group-a");

    let collectors = repository
        .watched_collectors(&source.chain, &source.network, source.chain_environment)
        .await?;
    assert_eq!(collectors.len(), 1);

    let lease = repository
        .acquire_component_lease("observer:tron:provider-a", "pod-a:boot-1", 30, now)
        .await?
        .ok_or("the first holder could not take a free lease")?;
    assert_eq!(lease.fence_token, 1);

    let first = observation(&source, &lease, SourceFinality::Seen, 0)?;
    let second = observation(&source, &lease, SourceFinality::Seen, 1)?;
    let report = repository
        .record_observations(
            &source,
            &lease,
            &[first.clone(), second],
            Some((
                COLLECTOR_ID,
                ObservationKind::CursorScan,
                CursorPosition::new(CursorKind::Block, "100", Some("block-100".to_owned()), 1)?,
            )),
        )
        .await?;
    assert_eq!(report.recorded, 2);
    assert_eq!(report.duplicates, 0);
    assert!(report.cursor_advanced);

    // The exact same claim again is one row, not a conflict and not a second
    // piece of evidence.
    let repeat = repository
        .record_observations(&source, &lease, std::slice::from_ref(&first), None)
        .await?;
    assert_eq!(repeat.recorded, 0);
    assert_eq!(repeat.duplicates, 1);

    // The same source moving the same transfer to finalized is new evidence.
    let finalized = observation(&source, &lease, SourceFinality::Finalized, 0)?;
    let progressed = repository
        .record_observations(&source, &lease, &[finalized], None)
        .await?;
    assert_eq!(progressed.recorded, 1);
    assert_eq!(count(&pool, "chain_observations").await?, 3);

    // A cursor never moves backwards, and saying so is not an error.
    let backwards = repository
        .record_observations(
            &source,
            &lease,
            &[],
            Some((
                COLLECTOR_ID,
                ObservationKind::CursorScan,
                CursorPosition::new(CursorKind::Block, "90", None, 1)?,
            )),
        )
        .await?;
    assert!(!backwards.cursor_advanced);
    let position = repository
        .find_cursor(source.id, ObservationKind::CursorScan, COLLECTOR_ID)
        .await?
        .ok_or("the cursor row disappeared")?;
    assert_eq!(position.value, "100");
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_frozen_holder_cannot_write_after_a_takeover() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = PostgresRepository::new(pool.clone());
    let now = OffsetDateTime::now_utc();
    let source = repository
        .find_source("provider-a")
        .await?
        .ok_or("the seeded source was not found")?;

    let first = repository
        .acquire_component_lease("observer:tron:provider-a", "pod-a:boot-1", 30, now)
        .await?
        .ok_or("the first holder could not take a free lease")?;

    // A live lease is not stolen.
    let contested = repository
        .acquire_component_lease("observer:tron:provider-a", "pod-b:boot-1", 30, now)
        .await?;
    assert!(contested.is_none());

    // Once it expires the neighbour takes over and the token moves on.
    let after_expiry = now + Duration::seconds(31);
    let second = repository
        .acquire_component_lease("observer:tron:provider-a", "pod-b:boot-1", 30, after_expiry)
        .await?
        .ok_or("the neighbour could not take an expired lease")?;
    assert_eq!(second.fence_token, 2);

    // The thawed predecessor still believes it is the holder.
    let stale = repository
        .record_observations(
            &source,
            &first,
            &[observation(&source, &first, SourceFinality::Seen, 0)?],
            None,
        )
        .await;
    assert!(matches!(stale, Err(RepositoryError::LeaseLost)));
    assert_eq!(count(&pool, "chain_observations").await?, 0);

    // The current holder writes normally.
    let live = repository
        .record_observations(
            &source,
            &second,
            &[observation(&source, &second, SourceFinality::Seen, 0)?],
            None,
        )
        .await?;
    assert_eq!(live.recorded, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn an_observer_cannot_claim_another_principals_identity() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    create_observer_role(&pool).await?;

    let observer_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url_as(OBSERVER_ROLE, OBSERVER_PASSWORD)?)
        .await?;

    let forged = insert_observation_as(&observer_pool, SOURCE_A, Some("someone_else")).await;
    assert!(
        forged.is_err(),
        "row level security allowed a forged source principal"
    );

    let honest = insert_observation_as(&observer_pool, SOURCE_A, None).await?;
    assert_eq!(honest, OBSERVER_ROLE);

    // Claiming another source's id is still possible, and is exactly why the
    // principal is recorded: the mismatch is detectable evidence.
    let borrowed_id = insert_observation_as(&observer_pool, SOURCE_B, None).await?;
    assert_eq!(borrowed_id, OBSERVER_ROLE);
    let mismatches: i64 = sqlx::query_scalar(
        r"
        SELECT count(*)
          FROM chain_observations AS observation
          JOIN chain_sources AS source ON source.id = observation.source_id
         WHERE observation.source_principal <> source.db_principal
        ",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(mismatches, 1);
    Ok(())
}

async fn insert_observation_as(
    pool: &PgPool,
    source_id: Uuid,
    forged_principal: Option<&str>,
) -> Result<String, sqlx::Error> {
    let statement = if forged_principal.is_some() {
        r"
        INSERT INTO chain_observations (
            id, source_id, source_principal, chain, network, chain_environment,
            observation_kind, tx_hash, event_index, token_key, token_display,
            from_address_key, from_address_text, to_address_key, to_address_text,
            amount_raw, decimals, execution_status, source_finality,
            evidence_sha256, observer_version, parser_version, fence_token,
            semantic_hash, observed_at
        ) VALUES (
            gen_random_uuid(), $1, $2, 'tron', 'nile', 'testnet', 'cursor_scan',
            'tx-rls', 0, '\x07', 'USDT', '\x09', 'TFrom', '\x03', 'TCollector',
            1000, 6, 'success', 'seen', repeat('a', 64), 'test', 'test', 1,
            sha256(gen_random_uuid()::TEXT::BYTEA), now()
        )
        RETURNING source_principal
        "
    } else {
        r"
        INSERT INTO chain_observations (
            id, source_id, chain, network, chain_environment,
            observation_kind, tx_hash, event_index, token_key, token_display,
            from_address_key, from_address_text, to_address_key, to_address_text,
            amount_raw, decimals, execution_status, source_finality,
            evidence_sha256, observer_version, parser_version, fence_token,
            semantic_hash, observed_at
        ) VALUES (
            gen_random_uuid(), $1, 'tron', 'nile', 'testnet', 'cursor_scan',
            'tx-rls', 0, '\x07', 'USDT', '\x09', 'TFrom', '\x03', 'TCollector',
            1000, 6, 'success', 'seen', repeat('a', 64), 'test', 'test', 1,
            sha256(gen_random_uuid()::TEXT::BYTEA), now()
        )
        RETURNING source_principal
        "
    };
    let mut query = sqlx::query_scalar::<_, String>(statement).bind(source_id);
    if let Some(principal) = forged_principal {
        query = query.bind(principal);
    }
    query.fetch_one(pool).await
}

fn observation(
    source: &ChainSource,
    lease: &ComponentLease,
    finality: SourceFinality,
    event_index: i32,
) -> Result<ResolvedObservation, Box<dyn Error>> {
    let transfer = ObservedTransfer {
        chain: source.chain.clone(),
        network: source.network.clone(),
        chain_environment: source.chain_environment,
        tx_hash: TxHash::new("abc123")?,
        event_index,
        block_number: Some(100),
        block_hash: Some("block-100".to_owned()),
        parent_hash: Some("block-99".to_owned()),
        block_time: Some(OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)),
        token_key: AddressKey::new([7_u8; 20])?,
        token_display: "USDT".to_owned(),
        from_address: AddressKey::new([9_u8; 21])?,
        from_address_text: "TFrom".to_owned(),
        to_address: AddressKey::new([3_u8; 21])?,
        to_address_text: "TCollector".to_owned(),
        amount_raw: RawAmount::from_str("1000")?,
        decimals: 6,
        memo: None,
        execution_status: ExecutionStatus::Success,
        source_finality: finality,
        source_head: Some(120),
        evidence_sha256: "b".repeat(64),
        evidence_uri: None,
    };
    let semantic_hash = transfer.semantic_hash(source.id, ObservationKind::CursorScan);
    Ok(ResolvedObservation {
        transfer,
        kind: ObservationKind::CursorScan,
        collector_address_id: COLLECTOR_ID,
        asset_id: Some(ASSET_ID),
        semantic_hash,
        observer_version: "observer-test".to_owned(),
        parser_version: "parser-test".to_owned(),
        fence_token: lease.fence_token,
        observed_at: OffsetDateTime::UNIX_EPOCH + Duration::days(20_000),
    })
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query("TRUNCATE chain_sources, chain_assets, merchants, component_leases CASCADE")
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
        ) VALUES (
            $1, $2, $3, 'TCollector', 'active', $4, encode(sha256($3), 'hex'), 'test'
        )
        ",
    )
    .bind(COLLECTOR_ID)
    .bind(ASSET_ID)
    .bind([3_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(pool)
    .await?;
    for (id, key, group, principal) in [
        (SOURCE_A, "provider-a", "group-a", "gateway_observer_test"),
        (SOURCE_B, "provider-b", "group-b", "gateway_observer_b"),
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
        .bind(key)
        .bind(group)
        .bind(principal)
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Creates a non-superuser role, because row level security is bypassed for
/// superusers: the policy can only be proven from a restricted principal.
async fn create_observer_role(pool: &PgPool) -> TestResult {
    sqlx::query(&format!(
        r"
        DO $$
        BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{OBSERVER_ROLE}') THEN
                CREATE ROLE {OBSERVER_ROLE} LOGIN PASSWORD '{OBSERVER_PASSWORD}';
            END IF;
        END
        $$;
        "
    ))
    .execute(pool)
    .await?;
    for statement in [
        format!(
            "GRANT CONNECT ON DATABASE {} TO {OBSERVER_ROLE}",
            database_name()
        ),
        format!("GRANT USAGE ON SCHEMA public TO {OBSERVER_ROLE}"),
        format!("GRANT SELECT, INSERT ON chain_observations TO {OBSERVER_ROLE}"),
        format!(
            "GRANT SELECT ON chain_sources, chain_assets, collector_addresses TO {OBSERVER_ROLE}"
        ),
    ] {
        sqlx::query(&statement).execute(pool).await?;
    }
    Ok(())
}

fn database_name() -> String {
    std::env::var("GATEWAY_TEST_DATABASE_NAME").unwrap_or_else(|_| "gateway".to_owned())
}

async fn count(pool: &PgPool, table: &str) -> Result<i64, Box<dyn Error>> {
    let query = match table {
        "chain_observations" => "SELECT count(*) FROM chain_observations",
        "chain_cursors" => "SELECT count(*) FROM chain_cursors",
        _ => return Err("unsupported test table".into()),
    };
    Ok(sqlx::query_scalar(query).fetch_one(pool).await?)
}
