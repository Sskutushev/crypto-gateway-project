use std::{error::Error, sync::Arc};

use gateway_application::{
    CollectorPolicy, ListRequest, NewCollector, OutboxRepository, PaymentIntentRepository,
    ProvisioningError, ProvisioningRepository, ProvisioningService, RandomBytes, SystemClock,
};
use gateway_domain::AddressKey;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::Duration;
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET: Uuid = Uuid::from_u128(9_801);

struct CountingRandom(std::sync::atomic::AtomicU8);

impl RandomBytes for CountingRandom {
    fn fill(&self, buffer: &mut [u8]) -> Result<(), ProvisioningError> {
        let seed = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        buffer.fill(seed);
        Ok(())
    }
}

fn service(pool: &PgPool) -> ProvisioningService<PostgresRepository, SystemClock, CountingRandom> {
    ProvisioningService::new(
        Arc::new(PostgresRepository::new(pool.clone())),
        SystemClock,
        CountingRandom(std::sync::atomic::AtomicU8::new(1)),
        Some(vec![4_u8; 32]),
    )
}

async fn reset(pool: &PgPool) -> TestResult {
    sqlx::query("TRUNCATE chain_assets, merchants, audit_events, domain_events CASCADE")
        .execute(pool)
        .await?;
    sqlx::query(
        r"INSERT INTO chain_assets (id, chain, network, chain_environment, contract_address_key,
              display_symbol, decimals, status, pinned_sha256, approved_by)
          VALUES ($1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active', encode(sha256($2), 'hex'), 'test')",
    )
    .bind(ASSET)
    .bind([31_u8; 20].as_slice())
    .execute(pool)
    .await?;
    Ok(())
}

async fn audit_actions(pool: &PgPool) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(
        sqlx::query_scalar("SELECT action FROM audit_events ORDER BY created_at, action")
            .fetch_all(pool)
            .await?,
    )
}

fn address(byte: u8) -> Result<AddressKey, Box<dyn Error>> {
    let mut bytes = vec![0x41_u8];
    bytes.extend_from_slice(&[byte; 20]);
    Ok(AddressKey::new(bytes)?)
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn a_merchant_is_onboarded_end_to_end_and_every_step_is_audited() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    reset(&pool).await?;
    let provisioning = service(&pool);
    let repository = PostgresRepository::new(pool.clone());

    // Creating the same merchant twice answers with the first; a different
    // request under the same external id is refused.
    let merchant = provisioning
        .create_merchant("alice", "shop-1", "Shop One", CollectorPolicy::Own)
        .await?;
    assert!(merchant.created);
    let again = provisioning
        .create_merchant("alice", "shop-1", "Shop One", CollectorPolicy::Own)
        .await?;
    assert_eq!((again.id, again.created), (merchant.id, false));
    assert!(matches!(
        provisioning
            .create_merchant("alice", "shop-1", "Shop One", CollectorPolicy::Shared)
            .await,
        Err(ProvisioningError::MerchantConflict)
    ));

    // An issued key authenticates; a revoked one no longer does.
    let key = provisioning
        .issue_api_key("alice", merchant.id, "checkout server")
        .await?;
    let hash: [u8; 32] = Sha256::digest(key.secret.as_bytes()).into();
    assert!(repository.authenticate_api_key(&hash).await?.is_some());
    provisioning
        .revoke_api_key("bob", key.key_id, "server rebuilt")
        .await?;
    assert!(repository.authenticate_api_key(&hash).await?.is_none());
    assert!(matches!(
        provisioning
            .revoke_api_key("bob", key.key_id, "again")
            .await,
        Err(ProvisioningError::KeyNotFound)
    ));

    // A webhook endpoint is live at once; a test event goes through the outbox.
    let endpoint = provisioning
        .add_webhook_endpoint("alice", merchant.id, "https://shop.example/hooks", None)
        .await?;
    let live = repository.active_endpoints(merchant.id).await?;
    assert_eq!(live.len(), 1);
    assert!(live[0].previous_secret.is_none());
    let event = provisioning
        .send_test_event("alice", endpoint.endpoint_id)
        .await?;
    let queued: String = sqlx::query_scalar("SELECT event_type FROM domain_events WHERE id = $1")
        .bind(event)
        .fetch_one(&pool)
        .await?;
    assert_eq!(queued, "webhook.test");

    // The merchant's own address, then a second registration of it is refused.
    let collector = provisioning
        .register_collector(
            "alice",
            NewCollector {
                id: Uuid::now_v7(),
                asset_id: ASSET,
                merchant_id: Some(merchant.id),
                address: address(51)?,
                address_text: "TMerchantOwn".to_owned(),
                ownership_evidence: "signed statement verified".to_owned(),
            },
        )
        .await?;
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT merchant_id FROM collector_addresses WHERE id = $1")
            .bind(collector)
            .fetch_one(&pool)
            .await?;
    assert_eq!(owner, Some(merchant.id));
    assert!(matches!(
        provisioning
            .register_collector(
                "alice",
                NewCollector {
                    id: Uuid::now_v7(),
                    asset_id: ASSET,
                    merchant_id: None,
                    address: address(51)?,
                    address_text: "TMerchantOwn".to_owned(),
                    ownership_evidence: "operator key".to_owned(),
                },
            )
            .await,
        Err(ProvisioningError::AddressTaken)
    ));
    provisioning
        .stop_quoting_collector("bob", collector, "merchant moves wallets")
        .await?;
    let state: String = sqlx::query_scalar("SELECT state FROM collector_addresses WHERE id = $1")
        .bind(collector)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "receiving_only");
    assert!(matches!(
        provisioning
            .stop_quoting_collector("bob", collector, "twice")
            .await,
        Err(ProvisioningError::CollectorNotFound)
    ));
    provisioning
        .retire_collector("bob", collector, "merchant moved wallets", false)
        .await?;

    let mut actions = audit_actions(&pool).await?;
    actions.sort();
    assert_eq!(
        actions,
        vec![
            "api_key.issue",
            "api_key.revoke",
            "collector.register",
            "collector.retire",
            "collector.stop_quoting",
            "merchant.create",
            "webhook_endpoint.create",
            "webhook_endpoint.test",
        ]
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_shared_merchant_is_never_given_an_address_of_its_own() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    reset(&pool).await?;
    let provisioning = service(&pool);
    let merchant = provisioning
        .create_merchant(
            "alice",
            "shop-shared",
            "Shared Shop",
            CollectorPolicy::Shared,
        )
        .await?;

    let refused = provisioning
        .register_collector(
            "alice",
            NewCollector {
                id: Uuid::now_v7(),
                asset_id: ASSET,
                merchant_id: Some(merchant.id),
                address: address(61)?,
                address_text: "TShared".to_owned(),
                ownership_evidence: "manual check".to_owned(),
            },
        )
        .await;

    assert!(
        matches!(refused, Err(ProvisioningError::PolicyMismatch)),
        "{refused:?}"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM collector_addresses")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rows, 0);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_rotated_secret_keeps_signing_until_its_transition_ends_and_a_stale_rotation_is_refused()
-> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    reset(&pool).await?;
    let provisioning = service(&pool);
    let repository = PostgresRepository::new(pool.clone());
    let merchant = provisioning
        .create_merchant(
            "alice",
            "shop-rotate",
            "Rotating Shop",
            CollectorPolicy::Own,
        )
        .await?;
    let endpoint = provisioning
        .add_webhook_endpoint("alice", merchant.id, "https://rotate.example/hooks", None)
        .await?;

    provisioning
        .rotate_webhook_secret("alice", endpoint.endpoint_id, Duration::days(2), "routine")
        .await?;
    let live = repository.active_endpoints(merchant.id).await?;
    assert_eq!(live[0].secret_version, 2);
    assert_eq!(
        live[0].previous_secret.map(|previous| previous.version),
        Some(1)
    );
    // A second rotation computed from the same starting version (a concurrent
    // one) finds the version moved and is refused instead of overwriting it.
    let stale = repository
        .rotate_webhook_secret(
            "bob",
            endpoint.endpoint_id,
            1,
            &live[0].secret_fingerprint,
            2,
            &[7_u8; 32],
            time::OffsetDateTime::now_utc() + Duration::days(1),
            "concurrent",
        )
        .await;
    assert!(
        matches!(stale, Err(ProvisioningError::EndpointNotFound)),
        "{stale:?}"
    );
    let unchanged = repository.active_endpoints(merchant.id).await?;
    assert_eq!(unchanged[0].secret_fingerprint, live[0].secret_fingerprint);

    // Once the transition has passed, only the new secret signs.
    sqlx::query("UPDATE webhook_endpoints SET previous_valid_until = now() - interval '1 second'")
        .execute(&pool)
        .await?;
    let live = repository.active_endpoints(merchant.id).await?;
    assert!(live[0].previous_secret.is_none());

    provisioning
        .disable_webhook_endpoint("bob", endpoint.endpoint_id, "shop closed")
        .await?;
    assert!(repository.active_endpoints(merchant.id).await?.is_empty());
    assert!(matches!(
        provisioning
            .rotate_webhook_secret("alice", endpoint.endpoint_id, Duration::days(1), "late")
            .await,
        Err(ProvisioningError::EndpointNotFound)
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn the_lists_page_through_everything_and_show_no_secret() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    reset(&pool).await?;
    let provisioning = service(&pool);
    let mut merchants = Vec::new();
    for index in 0..3 {
        merchants.push(
            provisioning
                .create_merchant(
                    "alice",
                    &format!("list-{index}"),
                    "Listed",
                    CollectorPolicy::Own,
                )
                .await?
                .id,
        );
    }
    let first = merchants[0];
    let mut secrets = Vec::new();
    for label in ["one", "two", "three"] {
        secrets.push(provisioning.issue_api_key("alice", first, label).await?);
    }

    let page = ListRequest::new(Some(2), None)?;
    let listed = provisioning.list_merchants(page).await?;
    assert_eq!(listed.items.len(), 2);
    let cursor = listed.next_cursor.ok_or("a full page names the next one")?;
    let rest = provisioning
        .list_merchants(ListRequest::new(Some(2), Some(cursor))?)
        .await?;
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.next_cursor, None);
    let mut seen: Vec<Uuid> = listed
        .items
        .iter()
        .chain(rest.items.iter())
        .map(|merchant| merchant.id)
        .collect();
    seen.sort();
    merchants.sort();
    assert_eq!(seen, merchants);

    let keys = provisioning
        .list_api_keys(first, ListRequest::new(Some(10), None)?)
        .await?;
    assert_eq!(keys.items.len(), 3);
    for key in &keys.items {
        let issued = secrets
            .iter()
            .find(|issued| issued.key_id == key.id)
            .ok_or("a listed key was never issued")?;
        assert_eq!(key.prefix, issued.prefix);
        assert!(!format!("{key:?}").contains(&issued.secret));
    }
    assert!(
        provisioning
            .list_api_keys(merchants[1], ListRequest::new(Some(10), None)?)
            .await?
            .items
            .is_empty()
    );

    provisioning
        .add_webhook_endpoint("alice", first, "https://list.example/hooks", None)
        .await?;
    let endpoints = provisioning
        .list_webhook_endpoints(first, ListRequest::new(None, None)?)
        .await?;
    assert_eq!(endpoints.items.len(), 1);
    assert_eq!(endpoints.items[0].status, "active");

    // A collector that still holds one reservation says so.
    let collector = provisioning
        .register_collector(
            "alice",
            NewCollector {
                id: Uuid::now_v7(),
                asset_id: ASSET,
                merchant_id: Some(first),
                address: address(61)?,
                address_text: "TListed".to_owned(),
                ownership_evidence: "manual check".to_owned(),
            },
        )
        .await?;
    reserve(&pool, first, collector).await?;
    let collectors = provisioning
        .list_collectors(Some(first), ListRequest::new(None, None)?)
        .await?;
    assert_eq!(collectors.items.len(), 1);
    assert_eq!(collectors.items[0].open_reservations, 1);
    assert!(
        provisioning
            .list_collectors(Some(merchants[1]), ListRequest::new(None, None)?)
            .await?
            .items
            .is_empty()
    );
    Ok(())
}

/// One quote and one amount reservation on `collector`.
async fn reserve(pool: &PgPool, merchant: Uuid, collector: Uuid) -> TestResult {
    let (price, policy, health, intent, quote, attempt) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query("INSERT INTO price_snapshots(id,asset_id,fiat_currency,rate_numerator,rate_denominator,sources,observed_at) VALUES($1,$2,'USD',1,1,'[{},{}]'::jsonb,now())")
        .bind(price).bind(ASSET).execute(pool).await?;
    sqlx::query("INSERT INTO quote_policies(id,asset_id,fiat_currency,version,status,quote_ttl_seconds,late_payment_window_seconds,amount_slot_count,max_price_age_seconds,max_policy_age_seconds,max_rail_health_age_seconds,observed_at) VALUES($1,$2,'USD','list-v1','active',900,3600,10000,300,300,300,now())")
        .bind(policy).bind(ASSET).execute(pool).await?;
    sqlx::query("INSERT INTO rail_health_snapshots(id,asset_id,health,observed_at) VALUES($1,$2,'healthy',now())")
        .bind(health).bind(ASSET).execute(pool).await?;
    sqlx::query("INSERT INTO payment_intents(id,merchant_id,amount_minor,currency,status,reference,created_at,updated_at) VALUES($1,$2,100,'USD','awaiting_payment','listed',now(),now())")
        .bind(intent).bind(merchant).execute(pool).await?;
    sqlx::query("INSERT INTO payment_quotes(id,merchant_id,payment_intent_id,asset_id,collector_address_id,fiat_currency,fiat_amount_minor,base_amount_raw,amount_raw,rate_numerator,rate_denominator,price_sources,price_observed_at,policy_version,rail_health_observed_at,created_at,expires_at,late_payment_until,price_snapshot_id,quote_policy_id,rail_health_snapshot_id) VALUES($1,$2,$3,$4,$5,'USD',100,100,100,1,1,'[{}]'::jsonb,now(),'list-v1',now(),now(),now()+interval '1 hour',now()+interval '2 hours',$6,$7,$8)")
        .bind(quote).bind(merchant).bind(intent).bind(ASSET).bind(collector).bind(price).bind(policy).bind(health).execute(pool).await?;
    sqlx::query("INSERT INTO payment_attempts(id,merchant_id,payment_intent_id,quote_id,collector_address_id,expected_amount_raw,status,quote_expires_at,late_payment_until,created_at,updated_at) VALUES($1,$2,$3,$4,$5,100,'awaiting_payment',now()+interval '1 hour',now()+interval '2 hours',now(),now())")
        .bind(attempt).bind(merchant).bind(intent).bind(quote).bind(collector).execute(pool).await?;
    sqlx::query("INSERT INTO amount_leases(id,collector_address_id,amount_raw,attempt_id,leased_from,lease_until) VALUES($1,$2,100,$3,now(),now()+interval '2 hours')")
        .bind(Uuid::now_v7()).bind(collector).bind(attempt).execute(pool).await?;
    Ok(())
}
