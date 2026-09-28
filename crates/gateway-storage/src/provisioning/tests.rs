use std::{error::Error, sync::Arc};

use gateway_application::{
    CollectorPolicy, NewCollector, OutboxRepository, PaymentIntentRepository, ProvisioningError,
    ProvisioningRepository, ProvisioningService, RandomBytes, SystemClock,
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
