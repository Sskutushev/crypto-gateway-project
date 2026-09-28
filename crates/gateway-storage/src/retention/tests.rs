use std::{error::Error, sync::Arc};

use gateway_application::{RetentionPolicy, RetentionService, SystemClock};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET: Uuid = Uuid::from_u128(12_001);
const COLLECTOR: Uuid = Uuid::from_u128(12_002);
const SOURCE: Uuid = Uuid::from_u128(12_003);
const MERCHANT: Uuid = Uuid::from_u128(12_004);
const ENDPOINT: Uuid = Uuid::from_u128(12_005);
const FINAL_TRANSFER: Uuid = Uuid::from_u128(12_010);
const PENDING_TRANSFER: Uuid = Uuid::from_u128(12_011);
const ATTESTED: Uuid = Uuid::from_u128(12_020);
const DUPLICATE_OF_FINAL: Uuid = Uuid::from_u128(12_021);
const NEVER_CANONICAL: Uuid = Uuid::from_u128(12_022);
const IN_CONFLICT: Uuid = Uuid::from_u128(12_023);
const OF_UNFINISHED: Uuid = Uuid::from_u128(12_024);
const RECENT_DUPLICATE: Uuid = Uuid::from_u128(12_025);
const DELIVERED_EVENT: Uuid = Uuid::from_u128(12_030);
const DEAD_EVENT: Uuid = Uuid::from_u128(12_031);
const RECENTLY_DELIVERED_EVENT: Uuid = Uuid::from_u128(12_032);

fn days_ago(days: i64) -> OffsetDateTime {
    OffsetDateTime::now_utc() - Duration::days(days)
}

async fn observation(
    pool: &PgPool,
    id: Uuid,
    tx_hash: &str,
    observed_at: OffsetDateTime,
) -> TestResult {
    sqlx::query(
        r"INSERT INTO chain_observations (
              id, source_id, chain, network, chain_environment, observation_kind, tx_hash,
              event_index, token_key, token_display, from_address_key, from_address_text,
              to_address_key, to_address_text, amount_raw, decimals, execution_status,
              source_finality, evidence_sha256, observer_version, parser_version, fence_token,
              semantic_hash, observed_at)
          VALUES ($1, $2, 'tron', 'nile', 'testnet', 'cursor_scan', $3, 0, '\x07', 'USDT', '\x09',
                  'TFrom', '\x03', 'TCollector', 1000, 6, 'success', 'finalized', repeat('a', 64),
                  'test', 'test', 1, sha256($1::text::bytea), $4)",
    )
    .bind(id)
    .bind(SOURCE)
    .bind(tx_hash)
    .bind(observed_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn transfer(pool: &PgPool, id: Uuid, tx_hash: &str, state: &str) -> TestResult {
    sqlx::query(
        r"INSERT INTO chain_transfers (id, asset_id, collector_address_id, chain, network,
              chain_environment, tx_hash, event_index, block_number, block_hash, block_time,
              token_key, from_address_key, from_address_text, to_address_key, to_address_text,
              amount_raw, decimals, canonicalization_policy, verifier_version, canonicalized_at)
          VALUES ($1, $2, $3, 'tron', 'nile', 'testnet', $4, 0, 1, 'block', now(), '\x07', '\x09',
                  'TFrom', '\x03', 'TCollector', 1000, 6, 'test', 'test', now())",
    )
    .bind(id)
    .bind(ASSET)
    .bind(COLLECTOR)
    .bind(tx_hash)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chain_transfer_state_current(transfer_id,state,state_version,updated_at) VALUES($1,$2,1,now())",
    )
    .bind(id)
    .bind(state)
    .execute(pool)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query(
        "TRUNCATE chain_sources, chain_assets, merchants, component_health_events, audit_events, chain_observation_conflicts CASCADE",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO chain_assets (id, chain, network, chain_environment, contract_address_key,
              display_symbol, decimals, status, pinned_sha256, approved_by)
          VALUES ($1, 'tron', 'nile', 'testnet', '\x07', 'USDT', 6, 'active',
                  encode(sha256('\x07'::bytea), 'hex'), 'test')",
    )
    .bind(ASSET)
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO collector_addresses (id, asset_id, address_key, address_text, state,
              valid_from, pinned_sha256, approved_by)
          VALUES ($1, $2, '\x03', 'TCollector', 'active', now() - interval '1 year',
                  encode(sha256('\x03'::bytea), 'hex'), 'test')",
    )
    .bind(COLLECTOR)
    .bind(ASSET)
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO chain_sources (id, chain, network, chain_environment, source_key,
              provider_group, kind, db_principal, requires_dedicated_principal, state, valid_from)
          VALUES ($1, 'tron', 'nile', 'testnet', 'retention-source', 'group-a', 'indexed_api',
                  current_user, FALSE, 'active', now() - interval '1 year')",
    )
    .bind(SOURCE)
    .execute(pool)
    .await?;

    // One finalized transfer with its attested reading; a transfer that is
    // not final yet; and readings around both.
    transfer(pool, FINAL_TRANSFER, "tx-final", "finalized").await?;
    transfer(pool, PENDING_TRANSFER, "tx-pending", "confirmed").await?;
    for (id, tx_hash, age) in [
        (ATTESTED, "tx-final", 60),
        (DUPLICATE_OF_FINAL, "tx-final", 60),
        (NEVER_CANONICAL, "tx-orphan", 60),
        (IN_CONFLICT, "tx-final", 61),
        (OF_UNFINISHED, "tx-pending", 60),
        (RECENT_DUPLICATE, "tx-final", 1),
    ] {
        observation(pool, id, tx_hash, days_ago(age)).await?;
    }
    sqlx::query(
        r"INSERT INTO chain_transfer_attestations (id, transfer_id, observation_id, source_id,
              provider_group, source_kind, attestation_role, verifier_version, created_at)
          VALUES ($1, $2, $3, $4, 'group-a', 'indexed_api', 'detection', 'test', now())",
    )
    .bind(Uuid::now_v7())
    .bind(FINAL_TRANSFER)
    .bind(ATTESTED)
    .bind(SOURCE)
    .execute(pool)
    .await?;
    let conflict = Uuid::now_v7();
    sqlx::query(
        r"INSERT INTO chain_observation_conflicts (id, chain, network, chain_environment, tx_hash,
              event_index, field, resolution, resolved_by, resolved_at, created_at)
          VALUES ($1, 'tron', 'nile', 'testnet', 'tx-final', 0, 'amount_raw', 'explained',
                  'test', now(), now())",
    )
    .bind(conflict)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chain_observation_conflict_items(conflict_id,observation_id,field_value) VALUES($1,$2,'1000')",
    )
    .bind(conflict)
    .bind(IN_CONFLICT)
    .execute(pool)
    .await?;

    // Webhook deliveries: five old attempts of a delivered event, five of a
    // dead-lettered one, and old attempts of an event delivered yesterday.
    sqlx::query(
        "INSERT INTO merchants(id,external_id,display_name,status,collector_policy) VALUES($1,'retention','Retention','active','shared')",
    )
    .bind(MERCHANT)
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO webhook_endpoints(id,merchant_id,url,secret_version,secret_fingerprint,status,created_at)
          VALUES($1,$2,'https://retention.example/hook',1,$3,'active',now())",
    )
    .bind(ENDPOINT)
    .bind(MERCHANT)
    .bind([1_u8; 32].as_slice())
    .execute(pool)
    .await?;
    for (event, delivered, dead) in [
        (DELIVERED_EVENT, Some(days_ago(50)), None),
        (DEAD_EVENT, None, Some(days_ago(50))),
        (RECENTLY_DELIVERED_EVENT, Some(days_ago(1)), None),
    ] {
        sqlx::query(
            r"INSERT INTO domain_events(id,merchant_id,channel,event_type,aggregate_type,aggregate_id,
                  payload,available_at,attempts,delivered_at,dead_lettered_at,created_at)
              VALUES($1,$2,'webhook','payment_intent.paid','payment_intent',$1,'{}',$3,5,$4,$5,$3)",
        )
        .bind(event)
        .bind(MERCHANT)
        .bind(days_ago(60))
        .bind(delivered)
        .bind(dead)
        .execute(pool)
        .await?;
        for attempt in 1..=5 {
            sqlx::query(
                "INSERT INTO webhook_deliveries(id,event_id,endpoint_id,attempt,response_status,delivered_at) VALUES($1,$2,$3,$4,503,$5)",
            )
            .bind(Uuid::now_v7())
            .bind(event)
            .bind(ENDPOINT)
            .bind(attempt)
            .bind(days_ago(60 - i64::from(attempt)))
            .execute(pool)
            .await?;
        }
    }

    // Health: three old transitions of one component, one of another.
    for (component, age, state) in [
        ("verifier", 90, "degraded"),
        ("verifier", 80, "ok"),
        ("verifier", 70, "degraded"),
        ("reconciler", 90, "ok"),
    ] {
        sqlx::query(
            "INSERT INTO component_health_events(id,component,new_state,created_at) VALUES($1,$2,$3,$4)",
        )
        .bind(Uuid::now_v7())
        .bind(component)
        .bind(state)
        .bind(days_ago(age))
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn remaining_observations(pool: &PgPool) -> Result<Vec<Uuid>, Box<dyn Error>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM chain_observations ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn retention_deletes_only_what_no_evidence_still_needs() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));

    // Only delivery attempts configured: the other tables are not touched.
    let deliveries_only = RetentionService::new(
        Arc::clone(&repository),
        SystemClock,
        RetentionPolicy::from_days(Some(30), 2, None, None)?,
    )?;
    let first = deliveries_only.purge_batch(100).await?;
    assert_eq!(first.webhook_deliveries, 3);
    assert_eq!(remaining_observations(&pool).await?.len(), 6);
    let attempts: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT event_id, attempt FROM webhook_deliveries ORDER BY event_id, attempt",
    )
    .fetch_all(&pool)
    .await?;
    let kept = |event: Uuid| -> Vec<i32> {
        attempts
            .iter()
            .filter(|(id, _)| *id == event)
            .map(|(_, attempt)| *attempt)
            .collect()
    };
    assert_eq!(kept(DELIVERED_EVENT), vec![4, 5], "the newest two stay");
    assert_eq!(
        kept(DEAD_EVENT),
        vec![1, 2, 3, 4, 5],
        "a dead letter keeps all"
    );
    assert_eq!(kept(RECENTLY_DELIVERED_EVENT), vec![1, 2, 3, 4, 5]);

    // Everything configured, in batches of one: the loop drains, and only the
    // unreferenced reading of a final transfer and the superseded health
    // transitions go.
    let everything = RetentionService::new(
        Arc::clone(&repository),
        SystemClock,
        RetentionPolicy::from_days(Some(30), 2, Some(30), Some(30))?,
    )?;
    let mut observations = 0;
    let mut health = 0;
    for _ in 0..10 {
        let report = everything.purge_batch(1).await?;
        observations += report.observations;
        health += report.health_events;
        if report.drained {
            break;
        }
    }
    assert_eq!((observations, health), (1, 2));
    assert_eq!(
        remaining_observations(&pool).await?,
        vec![
            ATTESTED,
            NEVER_CANONICAL,
            IN_CONFLICT,
            OF_UNFINISHED,
            RECENT_DUPLICATE
        ]
    );
    let components: Vec<(String, String)> = sqlx::query_as(
        "SELECT component, new_state FROM component_health_events ORDER BY component",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        components,
        vec![
            ("reconciler".to_owned(), "ok".to_owned()),
            ("verifier".to_owned(), "degraded".to_owned())
        ]
    );
    // Every purge that deleted something left an audit row.
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT resource_type FROM audit_events WHERE action='retention.purge' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        audited,
        vec![
            "chain_observations",
            "component_health_events",
            "webhook_deliveries"
        ]
    );
    // The evidence behind the canonical transfer is intact.
    let attested: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chain_transfer_attestations a JOIN chain_observations o ON o.id=a.observation_id WHERE a.transfer_id=$1",
    )
    .bind(FINAL_TRANSFER)
    .fetch_one(&pool)
    .await?;
    assert_eq!(attested, 1);
    Ok(())
}
