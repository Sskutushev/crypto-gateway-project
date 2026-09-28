use std::{error::Error, str::FromStr};

use gateway_application::{
    ManualResolution, OperationsError, OperationsRepository, OperationsService, OperatorScope,
    QuoteRepository, RepositoryError, RiskSubmission, SystemClock,
};
use gateway_domain::{
    CurrencyCode, ManualResolutionAction, PriceReading, RailHealth, RawAmount,
    RemainderDisposition, RiskDecision,
};
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
const RISK_KEY: Uuid = Uuid::from_u128(9_606);
const INGEST_SECRET: &str = "operator-ingest-secret-000000000000";
const ADMIN_SECRET: &str = "operator-admin-secret-0000000000000";
const RISK_SECRET: &str = "operator-risk-secret-00000000000000";
const MANUAL_MERCHANT: Uuid = Uuid::from_u128(9_610);
const MANUAL_INTENT: Uuid = Uuid::from_u128(9_611);
const MANUAL_QUOTE: Uuid = Uuid::from_u128(9_612);
const MANUAL_ATTEMPT: Uuid = Uuid::from_u128(9_613);
const MANUAL_TRANSFER: Uuid = Uuid::from_u128(9_614);
const MANUAL_PRICE: Uuid = Uuid::from_u128(9_615);
const MANUAL_HEALTH: Uuid = Uuid::from_u128(9_616);

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

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn manual_honor_is_atomic_idempotent_and_cannot_be_retargeted() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let service = OperationsService::new(Arc::clone(&repository), SystemClock);
    let admin = repository
        .authenticate_operator_key(&digest(ADMIN_SECRET))
        .await?
        .ok_or("the seeded admin key did not authenticate")?;
    let command = ManualResolution {
        action: ManualResolutionAction::Honor,
        transfer_id: MANUAL_TRANSFER,
        payment_intent_id: Some(MANUAL_INTENT),
        attempt_id: Some(MANUAL_ATTEMPT),
        allocate_raw: Some(RawAmount::from_str("1000000")?),
        remainder_raw: None,
        disposition: None,
        external_reference: None,
        reason: "operator verified the late payer evidence".to_owned(),
    };

    let (left, right) = tokio::join!(
        service.resolve_manual(&admin, "manual-honor-idem-0001", &command),
        service.resolve_manual(&admin, "manual-honor-idem-0001", &command),
    );
    let (first, concurrent_replay) = match (left?, right?) {
        (created, replayed) if !created.replayed && replayed.replayed => (created, replayed),
        (replayed, created) if replayed.replayed && !created.replayed => (created, replayed),
        (left, right) => {
            return Err(
                format!("expected one create and one replay, got {left:?} and {right:?}").into(),
            );
        }
    };
    assert_eq!(first.allocated_raw, command.allocate_raw);
    assert_eq!(concurrent_replay.id, first.id);
    let replay = service
        .resolve_manual(&admin, "manual-honor-idem-0001", &command)
        .await?;
    assert!(replay.replayed);
    assert_eq!(replay.id, first.id);

    let intent_status: String =
        sqlx::query_scalar("SELECT status FROM payment_intents WHERE id=$1")
            .bind(MANUAL_INTENT)
            .fetch_one(&pool)
            .await?;
    assert_eq!(intent_status, "paid");
    let processing: (String, String) = sqlx::query_as(
        "SELECT processing_state, allocated_raw::text FROM chain_transfer_processing WHERE transfer_id=$1",
    )
    .bind(MANUAL_TRANSFER)
    .fetch_one(&pool)
    .await?;
    assert_eq!(processing, ("settled".to_owned(), "1000000".to_owned()));
    for (table, count) in [
        ("payment_allocations", 1_i64),
        ("payment_fulfillments", 1_i64),
        ("manual_resolution_requests", 1_i64),
    ] {
        let actual: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await?;
        assert_eq!(actual, count, "unexpected row count in {table}");
    }
    let paid_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM domain_events WHERE event_type='payment_intent.paid' AND aggregate_id=$1",
    )
    .bind(MANUAL_INTENT)
    .fetch_one(&pool)
    .await?;
    assert_eq!(paid_events, 1);

    let mut changed = command.clone();
    changed.reason = "a different command under the same key".to_owned();
    assert!(matches!(
        service
            .resolve_manual(&admin, "manual-honor-idem-0001", &changed)
            .await,
        Err(OperationsError::Repository(
            RepositoryError::IdempotencyConflict
        ))
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn manual_reject_closes_only_unallocated_parked_money() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let service = OperationsService::new(Arc::clone(&repository), SystemClock);
    let admin = repository
        .authenticate_operator_key(&digest(ADMIN_SECRET))
        .await?
        .ok_or("the seeded admin key did not authenticate")?;
    let command = ManualResolution {
        action: ManualResolutionAction::Reject,
        transfer_id: MANUAL_TRANSFER,
        payment_intent_id: None,
        attempt_id: None,
        allocate_raw: None,
        remainder_raw: None,
        disposition: None,
        external_reference: None,
        reason: "transfer belongs to no payable obligation".to_owned(),
    };

    let result = service
        .resolve_manual(&admin, "manual-reject-idem-0001", &command)
        .await?;
    assert_eq!(result.action, ManualResolutionAction::Reject);
    let processing: String = sqlx::query_scalar(
        "SELECT processing_state FROM chain_transfer_processing WHERE transfer_id=$1",
    )
    .bind(MANUAL_TRANSFER)
    .fetch_one(&pool)
    .await?;
    assert_eq!(processing, "resolved");
    let allocations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM payment_allocations WHERE transfer_id=$1")
            .bind(MANUAL_TRANSFER)
            .fetch_one(&pool)
            .await?;
    assert_eq!(allocations, 0);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn overpayment_disposition_records_external_action_without_claiming_a_refund() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    sqlx::query("UPDATE chain_transfers SET amount_raw=1200000 WHERE id=$1")
        .bind(MANUAL_TRANSFER)
        .execute(&pool)
        .await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let service = OperationsService::new(Arc::clone(&repository), SystemClock);
    let admin = repository
        .authenticate_operator_key(&digest(ADMIN_SECRET))
        .await?
        .ok_or("the seeded admin key did not authenticate")?;
    let honor = ManualResolution {
        action: ManualResolutionAction::Honor,
        transfer_id: MANUAL_TRANSFER,
        payment_intent_id: Some(MANUAL_INTENT),
        attempt_id: Some(MANUAL_ATTEMPT),
        allocate_raw: Some(RawAmount::from_str("1000000")?),
        remainder_raw: None,
        disposition: None,
        external_reference: None,
        reason: "operator matched the transfer".to_owned(),
    };
    service
        .resolve_manual(&admin, "manual-overpay-honor-0001", &honor)
        .await?;
    let disposition = ManualResolution {
        action: ManualResolutionAction::RecordRemainderDisposition,
        transfer_id: MANUAL_TRANSFER,
        payment_intent_id: Some(MANUAL_INTENT),
        attempt_id: None,
        allocate_raw: None,
        remainder_raw: Some(RawAmount::from_str("200000")?),
        disposition: Some(RemainderDisposition::RefundedExternally),
        external_reference: Some("treasury-refund-17".to_owned()),
        reason: "treasury supplied proof of its external refund".to_owned(),
    };

    service
        .resolve_manual(&admin, "manual-disposition-0001", &disposition)
        .await?;
    let stored: (String, String) = sqlx::query_as(
        "SELECT disposition,external_reference FROM overpayment_remainder_dispositions WHERE transfer_id=$1",
    )
    .bind(MANUAL_TRANSFER)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        stored,
        (
            "refunded_externally".to_owned(),
            "treasury-refund-17".to_owned()
        )
    );
    let processing: String = sqlx::query_scalar(
        "SELECT processing_state FROM chain_transfer_processing WHERE transfer_id=$1",
    )
    .bind(MANUAL_TRANSFER)
    .fetch_one(&pool)
    .await?;
    assert_eq!(processing, "resolved");
    let refund_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM domain_events WHERE event_type LIKE '%refund%'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        refund_events, 0,
        "the gateway did not send the external refund"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn only_the_provider_bound_risk_key_can_submit_current_evidence() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool));
    let service = OperationsService::new(Arc::clone(&repository), SystemClock);
    let price_key = repository
        .authenticate_operator_key(&digest(INGEST_SECRET))
        .await?
        .ok_or("the seeded price key did not authenticate")?;
    let risk_key = repository
        .authenticate_operator_key(&digest(RISK_SECRET))
        .await?
        .ok_or("the seeded risk key did not authenticate")?;
    let mut submission = RiskSubmission {
        transfer_id: MANUAL_TRANSFER,
        provider: "example-kyt".to_owned(),
        decision: RiskDecision::Allow,
        score: Some(1),
        reasons: serde_json::json!({"screened": true}),
        evaluated_at: OffsetDateTime::now_utc(),
    };

    assert!(matches!(
        service
            .submit_risk_evaluation(&price_key, &submission)
            .await,
        Err(OperationsError::MissingScope(OperatorScope::RiskIngest))
    ));
    let id = service
        .submit_risk_evaluation(&risk_key, &submission)
        .await?;
    assert_ne!(id, Uuid::nil());
    submission.provider = "impersonated-provider".to_owned();
    assert!(matches!(
        service.submit_risk_evaluation(&risk_key, &submission).await,
        Err(OperationsError::RiskProviderNotAllowed)
    ));
    submission.provider = "example-kyt".to_owned();
    submission.evaluated_at = OffsetDateTime::now_utc() + Duration::hours(24);
    assert!(matches!(
        service.submit_risk_evaluation(&risk_key, &submission).await,
        Err(OperationsError::InvalidRiskEvaluation)
    ));
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
        (RISK_KEY, RISK_SECRET, "risk-feeder", vec!["risk_ingest"]),
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
    sqlx::query(
        "INSERT INTO operator_risk_provider_bindings(operator_key_id,provider,enabled_at) VALUES($1,'example-kyt',now())",
    )
    .bind(RISK_KEY)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_manual_payment(pool: &PgPool) -> TestResult {
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO merchants(id,external_id,display_name,status) VALUES($1,'manual-merchant','Manual Merchant','active')",
    )
    .bind(MANUAL_MERCHANT)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO price_snapshots(id,asset_id,fiat_currency,rate_numerator,rate_denominator,sources,observed_at) VALUES($1,$2,'AED',1,1,'[{},{}]'::jsonb,$3)",
    )
    .bind(MANUAL_PRICE)
    .bind(ASSET_ID)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO rail_health_snapshots(id,asset_id,health,observed_at) VALUES($1,$2,'healthy',$3)",
    )
    .bind(MANUAL_HEALTH)
    .bind(ASSET_ID)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO payment_intents(id,merchant_id,amount_minor,currency,status,reference,created_at,updated_at) VALUES($1,$2,100,'AED','risk_hold','manual-intent',$3,$3)",
    )
    .bind(MANUAL_INTENT)
    .bind(MANUAL_MERCHANT)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO payment_quotes(
             id,merchant_id,payment_intent_id,asset_id,collector_address_id,fiat_currency,
             fiat_amount_minor,base_amount_raw,amount_raw,rate_numerator,rate_denominator,
             price_sources,price_observed_at,policy_version,rail_health_observed_at,
             created_at,expires_at,late_payment_until,price_snapshot_id,quote_policy_id,
             rail_health_snapshot_id
           ) VALUES($1,$2,$3,$4,$5,'AED',100,1000000,1000000,1,1,'[{}]'::jsonb,$6,'v1',$6,$6,$7,$8,$9,$10,$11)",
    )
    .bind(MANUAL_QUOTE)
    .bind(MANUAL_MERCHANT)
    .bind(MANUAL_INTENT)
    .bind(ASSET_ID)
    .bind(COLLECTOR_ID)
    .bind(now)
    .bind(now + Duration::hours(1))
    .bind(now + Duration::hours(2))
    .bind(MANUAL_PRICE)
    .bind(POLICY_ID)
    .bind(MANUAL_HEALTH)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO payment_attempts(id,merchant_id,payment_intent_id,quote_id,collector_address_id,expected_amount_raw,status,quote_expires_at,late_payment_until,created_at,updated_at) VALUES($1,$2,$3,$4,$5,1000000,'awaiting_payment',$6,$7,$8,$8)",
    )
    .bind(MANUAL_ATTEMPT)
    .bind(MANUAL_MERCHANT)
    .bind(MANUAL_INTENT)
    .bind(MANUAL_QUOTE)
    .bind(COLLECTOR_ID)
    .bind(now + Duration::hours(1))
    .bind(now + Duration::hours(2))
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO chain_transfers(
             id,asset_id,collector_address_id,chain,network,chain_environment,tx_hash,
             event_index,block_number,block_hash,block_time,token_key,from_address_key,
             from_address_text,to_address_key,to_address_text,amount_raw,decimals,
             canonicalization_policy,verifier_version,canonicalized_at
           ) VALUES($1,$2,$3,'tron','nile','testnet','manual-tx',0,1,'manual-block',$4,$5,$6,'TFrom',$7,'TCollector',1000000,6,'test','test',$4)",
    )
    .bind(MANUAL_TRANSFER)
    .bind(ASSET_ID)
    .bind(COLLECTOR_ID)
    .bind(now)
    .bind([11_u8; 20].as_slice())
    .bind([13_u8; 21].as_slice())
    .bind([12_u8; 21].as_slice())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chain_transfer_state_current(transfer_id,state,state_version,updated_at) VALUES($1,'finalized',1,$2)",
    )
    .bind(MANUAL_TRANSFER)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chain_transfer_processing(transfer_id,allocated_raw,processing_state,updated_at) VALUES($1,0,'unmatched',$2)",
    )
    .bind(MANUAL_TRANSFER)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

const SECOND_ASSET: Uuid = Uuid::from_u128(9_620);
const COLLECTOR_ON_SECOND_ASSET: Uuid = Uuid::from_u128(9_621);
const SECOND_COLLECTOR: Uuid = Uuid::from_u128(9_622);
const FOREIGN_TRANSFER: Uuid = Uuid::from_u128(9_623);
const OTHER_MERCHANT: Uuid = Uuid::from_u128(9_624);
const OTHER_INTENT: Uuid = Uuid::from_u128(9_625);

fn honor(transfer: Uuid, intent: Uuid, attempt: Uuid) -> Result<ManualResolution, Box<dyn Error>> {
    Ok(ManualResolution {
        action: ManualResolutionAction::Honor,
        transfer_id: transfer,
        payment_intent_id: Some(intent),
        attempt_id: Some(attempt),
        allocate_raw: Some(RawAmount::from_str("1000000")?),
        remainder_raw: None,
        disposition: None,
        external_reference: None,
        reason: "operator checked the payer evidence".to_owned(),
    })
}

/// A second asset on the same rail with the same precision, a collector on
/// it, and a second collector on the first asset: the money that must never
/// be honored against the seeded attempt.
async fn seed_neighbours(pool: &PgPool) -> TestResult {
    sqlx::query(
        r"INSERT INTO chain_assets (id, chain, network, chain_environment, contract_address_key,
              display_symbol, decimals, status, pinned_sha256, approved_by)
          VALUES ($1, 'tron', 'nile', 'testnet', $2, 'USDX', 6, 'active', encode(sha256($2), 'hex'), 'test')",
    )
    .bind(SECOND_ASSET)
    .bind([21_u8; 20].as_slice())
    .execute(pool)
    .await?;
    for (id, asset, key) in [
        (COLLECTOR_ON_SECOND_ASSET, SECOND_ASSET, [22_u8; 21]),
        (SECOND_COLLECTOR, ASSET_ID, [23_u8; 21]),
    ] {
        sqlx::query(
            r"INSERT INTO collector_addresses (id, asset_id, address_key, address_text, state, valid_from,
                  pinned_sha256, approved_by)
              VALUES ($1, $2, $3, 'TNeighbour', 'active', $4, encode(sha256($3), 'hex'), 'test')",
        )
        .bind(id)
        .bind(asset)
        .bind(key.as_slice())
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// A finalized, unmatched transfer of exactly the attempt's amount.
async fn seed_transfer(
    pool: &PgPool,
    id: Uuid,
    asset: Uuid,
    collector: Uuid,
    token: [u8; 20],
    to: [u8; 21],
    block_time: OffsetDateTime,
) -> TestResult {
    sqlx::query(
        r"INSERT INTO chain_transfers(
             id,asset_id,collector_address_id,chain,network,chain_environment,tx_hash,
             event_index,block_number,block_hash,block_time,token_key,from_address_key,
             from_address_text,to_address_key,to_address_text,amount_raw,decimals,
             canonicalization_policy,verifier_version,canonicalized_at
           ) VALUES($1,$2,$3,'tron','nile','testnet',$4,0,2,'neighbour-block',$5,$6,$7,'TFrom',$8,'TNeighbour',1000000,6,'test','test',$5)",
    )
    .bind(id)
    .bind(asset)
    .bind(collector)
    .bind(format!("tx-{id}"))
    .bind(block_time)
    .bind(token.as_slice())
    .bind([13_u8; 21].as_slice())
    .bind(to.as_slice())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chain_transfer_state_current(transfer_id,state,state_version,updated_at) VALUES($1,'finalized',1,$2)",
    )
    .bind(id)
    .bind(block_time)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO chain_transfer_processing(transfer_id,allocated_raw,processing_state,updated_at) VALUES($1,0,'unmatched',$2)",
    )
    .bind(id)
    .bind(block_time)
    .execute(pool)
    .await?;
    Ok(())
}

/// A refused decision leaves no money trace at all, and the transfer and the
/// intent exactly where they were.
async fn assert_nothing_moved(pool: &PgPool, transfer: Uuid, intent_status: &str) -> TestResult {
    for table in [
        "payment_allocations",
        "payment_fulfillments",
        "chain_transfer_intent_claims",
        "payment_settlement_decisions",
        "manual_resolution_requests",
        "payment_events",
    ] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(pool)
            .await?;
        assert_eq!(rows, 0, "a refused honor wrote to {table}");
    }
    let webhooks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM domain_events WHERE event_type LIKE 'payment_intent.%'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(webhooks, 0, "a refused honor queued a merchant webhook");
    let processing: (String, String) = sqlx::query_as(
        "SELECT processing_state, allocated_raw::text FROM chain_transfer_processing WHERE transfer_id=$1",
    )
    .bind(transfer)
    .fetch_one(pool)
    .await?;
    assert_eq!(processing, ("unmatched".to_owned(), "0".to_owned()));
    let status: String = sqlx::query_scalar("SELECT status FROM payment_intents WHERE id=$1")
        .bind(MANUAL_INTENT)
        .fetch_one(pool)
        .await?;
    assert_eq!(status, intent_status);
    Ok(())
}

async fn admin_service(
    pool: &PgPool,
) -> Result<
    (
        OperationsService<PostgresRepository, SystemClock>,
        gateway_application::OperatorCredential,
    ),
    Box<dyn Error>,
> {
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let admin = repository
        .authenticate_operator_key(&digest(ADMIN_SECRET))
        .await?
        .ok_or("the seeded admin key did not authenticate")?;
    Ok((OperationsService::new(repository, SystemClock), admin))
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn honor_refuses_money_that_arrived_in_another_asset() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    seed_neighbours(&pool).await?;
    seed_transfer(
        &pool,
        FOREIGN_TRANSFER,
        SECOND_ASSET,
        COLLECTOR_ON_SECOND_ASSET,
        [21_u8; 20],
        [22_u8; 21],
        OffsetDateTime::now_utc(),
    )
    .await?;
    let (service, admin) = admin_service(&pool).await?;

    let result = service
        .resolve_manual(
            &admin,
            "honor-foreign-asset-0001",
            &honor(FOREIGN_TRANSFER, MANUAL_INTENT, MANUAL_ATTEMPT)?,
        )
        .await;

    assert!(
        matches!(result, Err(OperationsError::ManualResolutionConflict)),
        "{result:?}"
    );
    assert_nothing_moved(&pool, FOREIGN_TRANSFER, "risk_hold").await
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn honor_refuses_money_that_arrived_at_another_collector() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    seed_neighbours(&pool).await?;
    seed_transfer(
        &pool,
        FOREIGN_TRANSFER,
        ASSET_ID,
        SECOND_COLLECTOR,
        [11_u8; 20],
        [23_u8; 21],
        OffsetDateTime::now_utc(),
    )
    .await?;
    let (service, admin) = admin_service(&pool).await?;

    let result = service
        .resolve_manual(
            &admin,
            "honor-foreign-collector-01",
            &honor(FOREIGN_TRANSFER, MANUAL_INTENT, MANUAL_ATTEMPT)?,
        )
        .await;

    assert!(
        matches!(result, Err(OperationsError::ManualResolutionConflict)),
        "{result:?}"
    );
    assert_nothing_moved(&pool, FOREIGN_TRANSFER, "risk_hold").await
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn honor_refuses_an_attempt_paired_with_another_merchants_intent() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    let now = OffsetDateTime::now_utc();
    sqlx::query("INSERT INTO merchants(id,external_id,display_name,status) VALUES($1,'other-merchant','Other Merchant','active')")
        .bind(OTHER_MERCHANT)
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO payment_intents(id,merchant_id,amount_minor,currency,status,reference,created_at,updated_at) VALUES($1,$2,100,'AED','awaiting_payment','other-intent',$3,$3)",
    )
    .bind(OTHER_INTENT)
    .bind(OTHER_MERCHANT)
    .bind(now)
    .execute(&pool)
    .await?;
    let (service, admin) = admin_service(&pool).await?;

    let result = service
        .resolve_manual(
            &admin,
            "honor-foreign-merchant-001",
            &honor(MANUAL_TRANSFER, OTHER_INTENT, MANUAL_ATTEMPT)?,
        )
        .await;

    assert!(
        matches!(result, Err(OperationsError::ManualResolutionNotFound)),
        "{result:?}"
    );
    assert_nothing_moved(&pool, MANUAL_TRANSFER, "risk_hold").await
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn honor_never_reopens_a_cancelled_or_paid_intent() -> TestResult {
    for closed in ["cancelled", "paid"] {
        let _fixture = DATABASE.lock().await;
        let pool = connect().await?;
        seed(&pool).await?;
        seed_manual_payment(&pool).await?;
        sqlx::query("UPDATE payment_intents SET status=$2 WHERE id=$1")
            .bind(MANUAL_INTENT)
            .bind(closed)
            .execute(&pool)
            .await?;
        let (service, admin) = admin_service(&pool).await?;

        let result = service
            .resolve_manual(
                &admin,
                &format!("honor-closed-{closed}-0001"),
                &honor(MANUAL_TRANSFER, MANUAL_INTENT, MANUAL_ATTEMPT)?,
            )
            .await;

        assert!(
            matches!(result, Err(OperationsError::ManualResolutionConflict)),
            "{closed}: {result:?}"
        );
        assert_nothing_moved(&pool, MANUAL_TRANSFER, closed).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn honor_takes_a_late_payment_only_inside_its_window() -> TestResult {
    // Outside: the money was sent after the attempt's late-payment window closed.
    {
        let _fixture = DATABASE.lock().await;
        let pool = connect().await?;
        seed(&pool).await?;
        seed_manual_payment(&pool).await?;
        sqlx::query("UPDATE payment_attempts SET status='expired' WHERE id=$1")
            .bind(MANUAL_ATTEMPT)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE payment_intents SET status='expired' WHERE id=$1")
            .bind(MANUAL_INTENT)
            .execute(&pool)
            .await?;
        sqlx::query(
            "UPDATE chain_transfers SET block_time = now() + interval '3 hours' WHERE id=$1",
        )
        .bind(MANUAL_TRANSFER)
        .execute(&pool)
        .await?;
        let (service, admin) = admin_service(&pool).await?;
        let result = service
            .resolve_manual(
                &admin,
                "honor-late-outside-00001",
                &honor(MANUAL_TRANSFER, MANUAL_INTENT, MANUAL_ATTEMPT)?,
            )
            .await;
        assert!(
            matches!(result, Err(OperationsError::ManualResolutionConflict)),
            "{result:?}"
        );
        assert_nothing_moved(&pool, MANUAL_TRANSFER, "expired").await?;
    }
    // Inside: the expired attempt and intent both end settled and paid, once.
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    sqlx::query("UPDATE payment_attempts SET status='expired' WHERE id=$1")
        .bind(MANUAL_ATTEMPT)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE payment_intents SET status='expired' WHERE id=$1")
        .bind(MANUAL_INTENT)
        .execute(&pool)
        .await?;
    let (service, admin) = admin_service(&pool).await?;
    service
        .resolve_manual(
            &admin,
            "honor-late-inside-000001",
            &honor(MANUAL_TRANSFER, MANUAL_INTENT, MANUAL_ATTEMPT)?,
        )
        .await?;
    let states: (String, String) = sqlx::query_as(
        "SELECT i.status, a.status FROM payment_intents i JOIN payment_attempts a ON a.payment_intent_id=i.id WHERE i.id=$1",
    )
    .bind(MANUAL_INTENT)
    .fetch_one(&pool)
    .await?;
    assert_eq!(states, ("paid".to_owned(), "settled".to_owned()));
    let paid: i64 = sqlx::query_scalar("SELECT count(*) FROM domain_events WHERE event_type='payment_intent.paid' AND aggregate_id=$1")
        .bind(MANUAL_INTENT)
        .fetch_one(&pool)
        .await?;
    assert_eq!(paid, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_second_honor_under_a_new_key_is_refused_and_pays_nothing_twice() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    let (service, admin) = admin_service(&pool).await?;
    let command = honor(MANUAL_TRANSFER, MANUAL_INTENT, MANUAL_ATTEMPT)?;
    service
        .resolve_manual(&admin, "honor-first-key-00000001", &command)
        .await?;

    let again = service
        .resolve_manual(&admin, "honor-second-key-0000001", &command)
        .await;

    assert!(
        matches!(again, Err(OperationsError::ManualResolutionConflict)),
        "{again:?}"
    );
    for (table, expected) in [
        ("payment_allocations", 1_i64),
        ("payment_fulfillments", 1_i64),
        ("manual_resolution_requests", 1_i64),
    ] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await?;
        assert_eq!(rows, expected, "{table}");
    }
    let paid: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM domain_events WHERE event_type='payment_intent.paid'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(paid, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn the_database_itself_refuses_an_allocation_across_collectors() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    seed_manual_payment(&pool).await?;
    seed_neighbours(&pool).await?;
    seed_transfer(
        &pool,
        FOREIGN_TRANSFER,
        ASSET_ID,
        SECOND_COLLECTOR,
        [11_u8; 20],
        [23_u8; 21],
        OffsetDateTime::now_utc(),
    )
    .await?;

    // A hand-written statement that names the attempt's collector for a
    // transfer that arrived elsewhere: no application check stands in the way.
    let written = sqlx::query(
        r"INSERT INTO payment_allocations (id, attempt_id, payment_intent_id, merchant_id, transfer_id,
              allocated_raw, allocated_by, reason, created_at, collector_address_id)
          VALUES ($1, $2, $3, $4, $5, 1000000, 'test', 'manual', now(), $6)",
    )
    .bind(Uuid::now_v7())
    .bind(MANUAL_ATTEMPT)
    .bind(MANUAL_INTENT)
    .bind(MANUAL_MERCHANT)
    .bind(FOREIGN_TRANSFER)
    .bind(COLLECTOR_ID)
    .execute(&pool)
    .await;

    let refused = matches!(&written, Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503"));
    assert!(refused, "expected a foreign-key refusal, got {written:?}");
    Ok(())
}
