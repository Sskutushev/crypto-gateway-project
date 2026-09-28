use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gateway_domain::SigningSecret;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    CollectorPolicy, EndpointState, MerchantRecord, NewCollector, ProvisioningError,
    ProvisioningRepository, ProvisioningService, RandomBytes,
};
use crate::Clock;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MERCHANT: Uuid = Uuid::from_u128(1);
const ENDPOINT: Uuid = Uuid::from_u128(2);

fn master_key() -> Vec<u8> {
    vec![9_u8; 32]
}

#[derive(Clone, Copy)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }
}

struct FixedRandom;

impl RandomBytes for FixedRandom {
    fn fill(&self, buffer: &mut [u8]) -> Result<(), ProvisioningError> {
        buffer.fill(0xab);
        Ok(())
    }
}

/// from version, previous fingerprint, new version, new fingerprint, previous valid until
type Rotation = (i32, [u8; 32], i32, [u8; 32], OffsetDateTime);

/// What the repository was asked to store.
#[derive(Default)]
struct Recorded {
    key_hashes: Vec<[u8; 32]>,
    key_prefixes: Vec<String>,
    endpoint_fingerprints: Vec<[u8; 32]>,
    rotations: Vec<Rotation>,
}

struct FakeRepository {
    recorded: Mutex<Recorded>,
    endpoint: Option<EndpointState>,
}

impl FakeRepository {
    fn new(endpoint: Option<EndpointState>) -> Arc<Self> {
        Arc::new(Self {
            recorded: Mutex::new(Recorded::default()),
            endpoint,
        })
    }

    fn recorded(&self) -> std::sync::MutexGuard<'_, Recorded> {
        self.recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl ProvisioningRepository for FakeRepository {
    async fn create_merchant(
        &self,
        _actor: &str,
        id: Uuid,
        external_id: &str,
        display_name: &str,
        policy: CollectorPolicy,
    ) -> Result<MerchantRecord, ProvisioningError> {
        Ok(MerchantRecord {
            id,
            external_id: external_id.to_owned(),
            display_name: display_name.to_owned(),
            collector_policy: policy,
            created: true,
        })
    }

    async fn insert_api_key(
        &self,
        _actor: &str,
        _merchant_id: Uuid,
        _key_id: Uuid,
        prefix: &str,
        secret_hash: &[u8; 32],
        _label: &str,
    ) -> Result<(), ProvisioningError> {
        let mut recorded = self.recorded();
        recorded.key_hashes.push(*secret_hash);
        recorded.key_prefixes.push(prefix.to_owned());
        Ok(())
    }

    async fn revoke_api_key(&self, _: &str, _: Uuid, _: &str) -> Result<(), ProvisioningError> {
        Ok(())
    }

    async fn insert_webhook_endpoint(
        &self,
        _actor: &str,
        _merchant_id: Uuid,
        _endpoint_id: Uuid,
        _url: &str,
        _description: Option<&str>,
        fingerprint: &[u8; 32],
    ) -> Result<(), ProvisioningError> {
        self.recorded().endpoint_fingerprints.push(*fingerprint);
        Ok(())
    }

    async fn endpoint_state(&self, _: Uuid) -> Result<Option<EndpointState>, ProvisioningError> {
        Ok(self.endpoint.clone())
    }

    async fn rotate_webhook_secret(
        &self,
        _actor: &str,
        _endpoint_id: Uuid,
        from_version: i32,
        previous_fingerprint: &[u8; 32],
        new_version: i32,
        new_fingerprint: &[u8; 32],
        previous_valid_until: OffsetDateTime,
        _reason: &str,
    ) -> Result<(), ProvisioningError> {
        self.recorded().rotations.push((
            from_version,
            *previous_fingerprint,
            new_version,
            *new_fingerprint,
            previous_valid_until,
        ));
        Ok(())
    }

    async fn disable_webhook_endpoint(
        &self,
        _: &str,
        _: Uuid,
        _: &str,
    ) -> Result<(), ProvisioningError> {
        Ok(())
    }

    async fn enqueue_test_event(&self, _: &str, _: Uuid, _: Uuid) -> Result<(), ProvisioningError> {
        Ok(())
    }

    async fn register_collector(&self, _: &str, _: &NewCollector) -> Result<(), ProvisioningError> {
        Ok(())
    }

    async fn stop_quoting_collector(
        &self,
        _: &str,
        _: Uuid,
        _: &str,
    ) -> Result<(), ProvisioningError> {
        Ok(())
    }

    async fn retire_collector(
        &self,
        _: &str,
        _: Uuid,
        _: &str,
        _: bool,
    ) -> Result<(), ProvisioningError> {
        Ok(())
    }
}

fn service(
    repository: &Arc<FakeRepository>,
    master: Option<Vec<u8>>,
) -> ProvisioningService<FakeRepository, FixedClock, FixedRandom> {
    ProvisioningService::new(Arc::clone(repository), FixedClock, FixedRandom, master)
}

#[tokio::test]
async fn an_api_key_is_shown_once_and_only_its_hash_is_stored() -> TestResult {
    let repository = FakeRepository::new(None);

    let issued = service(&repository, None)
        .issue_api_key("alice", MERCHANT, "checkout server")
        .await?;

    assert_eq!(issued.secret, format!("gw_{}", "ab".repeat(32)));
    assert_eq!(issued.prefix, issued.secret[..12]);
    let recorded = repository.recorded();
    let stored: [u8; 32] = Sha256::digest(issued.secret.as_bytes()).into();
    assert_eq!(recorded.key_hashes, vec![stored]);
    assert_eq!(recorded.key_prefixes, vec![issued.prefix.clone()]);
    Ok(())
}

#[tokio::test]
async fn a_webhook_needs_https_and_the_master_key_and_stores_only_a_fingerprint() -> TestResult {
    let repository = FakeRepository::new(None);

    assert!(matches!(
        service(&repository, Some(master_key()))
            .add_webhook_endpoint("alice", MERCHANT, "http://shop.example/hook", None)
            .await,
        Err(ProvisioningError::Invalid(_))
    ));
    assert!(matches!(
        service(&repository, None)
            .add_webhook_endpoint("alice", MERCHANT, "https://shop.example/hook", None)
            .await,
        Err(ProvisioningError::MasterKeyMissing)
    ));

    let registered = service(&repository, Some(master_key()))
        .add_webhook_endpoint(
            "alice",
            MERCHANT,
            " https://shop.example/hook ",
            Some("orders"),
        )
        .await?;

    let expected = SigningSecret::derive(&master_key(), 1, MERCHANT, registered.endpoint_id)?;
    assert_eq!(registered.signing_secret, expected.expose());
    assert_eq!(registered.url, "https://shop.example/hook");
    assert_eq!(
        repository.recorded().endpoint_fingerprints,
        vec![expected.fingerprint()]
    );
    Ok(())
}

#[tokio::test]
async fn a_rotation_moves_one_version_and_keeps_the_previous_secret_for_the_transition()
-> TestResult {
    let repository = FakeRepository::new(Some(EndpointState {
        merchant_id: MERCHANT,
        secret_version: 3,
        active: true,
    }));

    let rotated = service(&repository, Some(master_key()))
        .rotate_webhook_secret("alice", ENDPOINT, Duration::days(7), "leaked in a log")
        .await?;

    let previous = SigningSecret::derive(&master_key(), 3, MERCHANT, ENDPOINT)?;
    let current = SigningSecret::derive(&master_key(), 4, MERCHANT, ENDPOINT)?;
    assert_eq!(rotated.secret_version, 4);
    assert_eq!(rotated.signing_secret, current.expose());
    let until = FixedClock.now() + Duration::days(7);
    assert_eq!(rotated.previous_valid_until, Some(until));
    assert_eq!(
        repository.recorded().rotations,
        vec![(3, previous.fingerprint(), 4, current.fingerprint(), until)]
    );
    Ok(())
}

#[tokio::test]
async fn a_rotation_is_refused_outside_its_bounds_or_on_a_disabled_endpoint() -> TestResult {
    let active = FakeRepository::new(Some(EndpointState {
        merchant_id: MERCHANT,
        secret_version: 1,
        active: true,
    }));
    for transition in [Duration::minutes(30), Duration::days(31)] {
        assert!(matches!(
            service(&active, Some(master_key()))
                .rotate_webhook_secret("alice", ENDPOINT, transition, "routine")
                .await,
            Err(ProvisioningError::Invalid(_))
        ));
    }
    let disabled = FakeRepository::new(Some(EndpointState {
        merchant_id: MERCHANT,
        secret_version: 1,
        active: false,
    }));
    assert!(matches!(
        service(&disabled, Some(master_key()))
            .rotate_webhook_secret("alice", ENDPOINT, Duration::days(1), "routine")
            .await,
        Err(ProvisioningError::EndpointNotFound)
    ));
    Ok(())
}

#[tokio::test]
async fn every_write_names_a_person_and_every_removal_a_reason() {
    let repository = FakeRepository::new(None);
    let provisioning = service(&repository, Some(master_key()));

    assert!(matches!(
        provisioning
            .create_merchant(" ", "shop-1", "Shop", CollectorPolicy::Own)
            .await,
        Err(ProvisioningError::Invalid(_))
    ));
    assert!(matches!(
        provisioning
            .revoke_api_key("alice", Uuid::from_u128(5), "  ")
            .await,
        Err(ProvisioningError::Invalid(_))
    ));
    assert!(matches!(
        provisioning
            .retire_collector("alice", Uuid::from_u128(6), "", false)
            .await,
        Err(ProvisioningError::Invalid(_))
    ));
    assert!(matches!(
        provisioning
            .stop_quoting_collector("alice", Uuid::from_u128(6), " ")
            .await,
        Err(ProvisioningError::Invalid(_))
    ));
}
