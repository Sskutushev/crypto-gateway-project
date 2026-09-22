use std::{error::Error, str::FromStr};

use gateway_application::{
    OperationsRepository, OperationsService, OperatorScope, QuoteRepository, SystemClock,
};
use gateway_domain::{CurrencyCode, PriceReading, RailHealth, RawAmount};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET_ID: Uuid = Uuid::from_u128(9_601);
const COLLECTOR_ID: Uuid = Uuid::from_u128(9_602);
const POLICY_ID: Uuid = Uuid::from_u128(9_603);
const INGEST_KEY: Uuid = Uuid::from_u128(9_604);
const ADMIN_KEY: Uuid = Uuid::from_u128(9_605);
const INGEST_SECRET: &str = "operator-ingest-secret-000000000000";
const ADMIN_SECRET: &str = "operator-admin-secret-0000000000000";

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn evidence_is_ingested_with_its_readings_and_a_stop_closes_new_quotes() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let service = OperationsService::new(Arc::clone(&repository), SystemClock);
    let currency = CurrencyCode::new("AED")?;

    let ingester = repository
        .authenticate_operator_key(&digest(INGEST_SECRET))
        .await?
        .ok_or("the seeded ingest key did not authenticate")?;
    assert!(ingester.allows(OperatorScope::Ingest));
    assert!(!ingester.allows(OperatorScope::Admin));

    // Two independent groups that agree: one snapshot, and both readings kept.
    let snapshot = service
        .submit_price(
            &ingester,
            ASSET_ID,
            &currency,
            vec![reading("alpha", "2727", 30)?, reading("beta", "2729", 10)?],
        )
        .await?;
    assert_eq!(snapshot.group_count, 2);
    let readings: i64 =
        sqlx::query_scalar("SELECT count(*) FROM price_readings WHERE snapshot_id = $1")
            .bind(snapshot.snapshot_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(readings, 2);
    // The snapshot is as old as its stalest input, never as young as its
    // newest: staleness is what closes new quotes.
    let observed_at: OffsetDateTime =
        sqlx::query_scalar("SELECT observed_at FROM price_snapshots WHERE id = $1")
            .bind(snapshot.snapshot_id)
            .fetch_one(&pool)
            .await?;
    assert!(OffsetDateTime::now_utc() - observed_at >= Duration::seconds(29));

    // A disagreement writes no snapshot and still records what each source
    // said, with the reason it did not count.
    let refused = service
        .submit_price(
            &ingester,
            ASSET_ID,
            &currency,
            vec![reading("alpha", "2728", 5)?, reading("beta", "2900", 5)?],
        )
        .await;
    assert!(refused.is_err());
    let diverged: i64 =
        sqlx::query_scalar("SELECT count(*) FROM price_readings WHERE discard_reason = 'diverged'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(diverged, 2);
    let snapshots: i64 = sqlx::query_scalar("SELECT count(*) FROM price_snapshots")
        .fetch_one(&pool)
        .await?;
    assert_eq!(snapshots, 1, "a disagreement writes no rate");

    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_closed_rail_stops_new_quotes_and_only_a_person_reopens_it() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let service = OperationsService::new(Arc::clone(&repository), SystemClock);
    let currency = CurrencyCode::new("AED")?;
    let ingester = repository
        .authenticate_operator_key(&digest(INGEST_SECRET))
        .await?
        .ok_or("the seeded ingest key did not authenticate")?;
    service
        .submit_price(
            &ingester,
            ASSET_ID,
            &currency,
            vec![reading("alpha", "2727", 30)?, reading("beta", "2729", 10)?],
        )
        .await?;

    service
        .submit_rail_health(&ingester, ASSET_ID, RailHealth::Healthy, None)
        .await?;

    // With healthy evidence the quote context is complete and the rail is open.
    let context = repository.load_quote_context(ASSET_ID, &currency).await?;
    assert!(context.price.is_some());
    assert!(context.policy.is_some());
    assert!(context.rail_health.is_some());
    assert_eq!(context.rail_stop_reason, None);

    // An ingest key cannot close a rail.
    assert!(
        service
            .open_rail_stop(&ingester, ASSET_ID, "reconciliation_drift", None)
            .await
            .is_err()
    );

    let admin = repository
        .authenticate_operator_key(&digest(ADMIN_SECRET))
        .await?
        .ok_or("the seeded admin key did not authenticate")?;
    let stop = service
        .open_rail_stop(
            &admin,
            ASSET_ID,
            "reconciliation_drift",
            Some("AED 12 short"),
        )
        .await?;
    assert_eq!(stop.reason_code, "reconciliation_drift");

    // The same rail closed twice keeps the first reason rather than replacing
    // it: the second call is idempotent, not a second incident.
    let again = service
        .open_rail_stop(&admin, ASSET_ID, "something_else", None)
        .await?;
    assert_eq!(again.id, stop.id);
    assert_eq!(again.reason_code, "reconciliation_drift");

    let closed = repository.load_quote_context(ASSET_ID, &currency).await?;
    assert_eq!(
        closed.rail_stop_reason.as_deref(),
        Some("reconciliation_drift"),
        "a closed rail is visible to the quote path"
    );

    service
        .clear_rail_stop(&admin, ASSET_ID, "counted twice, corrected")
        .await?;
    let reopened = repository.load_quote_context(ASSET_ID, &currency).await?;
    assert_eq!(reopened.rail_stop_reason, None);
    let cleared_reason: Option<String> =
        sqlx::query_scalar("SELECT cleared_reason FROM rail_stops WHERE id = $1")
            .bind(stop.id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(cleared_reason.as_deref(), Some("counted twice, corrected"));
    Ok(())
}

fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn reading(group: &str, numerator: &str, age: i64) -> Result<PriceReading, Box<dyn Error>> {
    Ok(PriceReading {
        source_key: format!("{group}-key"),
        provider_group: group.to_owned(),
        rate_numerator: RawAmount::from_str(numerator)?,
        rate_denominator: RawAmount::from_str("10000")?,
        observed_at: OffsetDateTime::now_utc() - Duration::seconds(age),
    })
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query(
        "TRUNCATE chain_assets, merchants, operator_api_keys, rail_stops, price_snapshots, \
         price_readings, rail_health_snapshots, quote_policies CASCADE",
    )
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
    .bind([11_u8; 20].as_slice())
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
    .bind([12_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO quote_policies (
            id, asset_id, fiat_currency, version, status, quote_ttl_seconds,
            late_payment_window_seconds, amount_slot_count, max_price_age_seconds,
            max_policy_age_seconds, max_rail_health_age_seconds, observed_at,
            min_price_sources, max_price_deviation_bps
        ) VALUES (
            $1, $2, 'AED', 'v1', 'active', 900, 2592000, 10000, 300,
            31536000, 600, $3, 2, 200
        )
        ",
    )
    .bind(POLICY_ID)
    .bind(ASSET_ID)
    .bind(OffsetDateTime::now_utc())
    .execute(pool)
    .await?;
    for (id, secret, label, scopes) in [
        (INGEST_KEY, INGEST_SECRET, "price-feeder", vec!["ingest"]),
        (ADMIN_KEY, ADMIN_SECRET, "on-call", vec!["read", "admin"]),
    ] {
        sqlx::query(
            r"
            INSERT INTO operator_api_keys (id, key_prefix, secret_hash, label, scopes)
            VALUES ($1, $2, $3, $4, $5)
            ",
        )
        .bind(id)
        .bind(&secret[..12])
        .bind(digest(secret).as_slice())
        .bind(label)
        .bind(&scopes)
        .execute(pool)
        .await?;
    }
    Ok(())
}
