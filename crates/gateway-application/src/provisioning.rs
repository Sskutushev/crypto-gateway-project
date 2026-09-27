//! Onboarding a merchant without hand-written SQL.
//!
//! Every write here changes who can be paid, where the money goes, or who is
//! told about it, so each one names the person who made it, is recorded in
//! the audit trail in the same transaction, and is idempotent where a retry
//! is plausible. Secrets (API keys, webhook signing secrets) exist in memory
//! only long enough to be shown once; the database keeps a hash or a
//! fingerprint.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{AddressKey, SigningSecret};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Clock, RepositoryError};

/// Whose address receives a merchant's money. See migration 0015.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorPolicy {
    /// Only collectors registered to this merchant.
    Own,
    /// Only operator collectors; the operator owes the merchant the money.
    Shared,
}

impl CollectorPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Shared => "shared",
        }
    }
}

impl std::str::FromStr for CollectorPolicy {
    type Err = ProvisioningError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "own" => Ok(Self::Own),
            "shared" => Ok(Self::Shared),
            _ => Err(ProvisioningError::Invalid(
                "collector policy must be own or shared",
            )),
        }
    }
}

/// Operating-system randomness, injected so the application stays testable.
pub trait RandomBytes: Send + Sync {
    /// Fills `buffer` with cryptographically secure random bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError::Randomness`] when the source is unavailable.
    fn fill(&self, buffer: &mut [u8]) -> Result<(), ProvisioningError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerchantRecord {
    pub id: Uuid,
    pub external_id: String,
    pub display_name: String,
    pub collector_policy: CollectorPolicy,
    /// False when an identical merchant already existed and nothing was written.
    pub created: bool,
}

/// An API key as shown once to the person who issued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedApiKey {
    pub key_id: Uuid,
    pub merchant_id: Uuid,
    pub prefix: String,
    pub secret: String,
}

/// A webhook endpoint and its signing secret, shown once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRegistration {
    pub endpoint_id: Uuid,
    pub merchant_id: Uuid,
    pub url: String,
    pub secret_version: i32,
    pub signing_secret: String,
    /// Until when the previous secret still signs deliveries, after a rotation.
    pub previous_valid_until: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointState {
    pub merchant_id: Uuid,
    pub secret_version: i32,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCollector {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub merchant_id: Option<Uuid>,
    pub address: AddressKey,
    pub address_text: String,
    /// How control of the address was established, kept in the audit trail.
    pub ownership_evidence: String,
}

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("a merchant with this external id exists with different details")]
    MerchantConflict,
    #[error("the merchant does not exist or is not active")]
    MerchantNotFound,
    #[error("the webhook endpoint does not exist or is disabled")]
    EndpointNotFound,
    #[error("the API key does not exist or is already revoked")]
    KeyNotFound,
    #[error("the collector does not exist or is already retired")]
    CollectorNotFound,
    #[error("the address is already registered for this asset")]
    AddressTaken,
    #[error("the merchant's collector policy does not allow this collector")]
    PolicyMismatch,
    #[error("the webhook master key is required for this command")]
    MasterKeyMissing,
    #[error("secure randomness is unavailable")]
    Randomness,
    #[error(transparent)]
    Webhook(#[from] gateway_domain::WebhookError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

#[async_trait]
pub trait ProvisioningRepository: Send + Sync {
    async fn create_merchant(
        &self,
        actor: &str,
        id: Uuid,
        external_id: &str,
        display_name: &str,
        policy: CollectorPolicy,
    ) -> Result<MerchantRecord, ProvisioningError>;

    async fn insert_api_key(
        &self,
        actor: &str,
        merchant_id: Uuid,
        key_id: Uuid,
        prefix: &str,
        secret_hash: &[u8; 32],
        label: &str,
    ) -> Result<(), ProvisioningError>;

    async fn revoke_api_key(
        &self,
        actor: &str,
        key_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError>;

    async fn insert_webhook_endpoint(
        &self,
        actor: &str,
        merchant_id: Uuid,
        endpoint_id: Uuid,
        url: &str,
        description: Option<&str>,
        fingerprint: &[u8; 32],
    ) -> Result<(), ProvisioningError>;

    async fn endpoint_state(
        &self,
        endpoint_id: Uuid,
    ) -> Result<Option<EndpointState>, ProvisioningError>;

    /// Moves the endpoint to `new_version`, keeping `from_version` signing
    /// until `previous_valid_until`. Refuses if the endpoint is no longer at
    /// `from_version`, so two concurrent rotations cannot both succeed.
    #[allow(clippy::too_many_arguments)]
    async fn rotate_webhook_secret(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        from_version: i32,
        previous_fingerprint: &[u8; 32],
        new_version: i32,
        new_fingerprint: &[u8; 32],
        previous_valid_until: OffsetDateTime,
        reason: &str,
    ) -> Result<(), ProvisioningError>;

    async fn disable_webhook_endpoint(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError>;

    /// Queues a `webhook.test` event through the normal outbox.
    async fn enqueue_test_event(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        event_id: Uuid,
    ) -> Result<(), ProvisioningError>;

    async fn register_collector(
        &self,
        actor: &str,
        collector: &NewCollector,
    ) -> Result<(), ProvisioningError>;

    async fn retire_collector(
        &self,
        actor: &str,
        collector_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError>;
}

/// API keys look like `gw_<64 hex>`; the first twelve characters are the
/// prefix shown in lists so a key can be recognised without being revealed.
const API_KEY_PREFIX_CHARS: usize = 12;

pub struct ProvisioningService<R, C, G> {
    repository: Arc<R>,
    clock: C,
    random: G,
    master_key: Option<Vec<u8>>,
}

impl<R, C, G> std::fmt::Debug for ProvisioningService<R, C, G> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The master key never reaches a log line or a panic message.
        formatter
            .debug_struct("ProvisioningService")
            .field("master_key", &self.master_key.as_ref().map(|_| "redacted"))
            .finish_non_exhaustive()
    }
}

impl<R, C, G> ProvisioningService<R, C, G>
where
    R: ProvisioningRepository,
    C: Clock,
    G: RandomBytes,
{
    pub fn new(repository: Arc<R>, clock: C, random: G, master_key: Option<Vec<u8>>) -> Self {
        Self {
            repository,
            clock,
            random,
            master_key,
        }
    }

    /// Creates a merchant, or returns the identical one that already exists.
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for invalid fields or a conflicting merchant.
    pub async fn create_merchant(
        &self,
        actor: &str,
        external_id: &str,
        display_name: &str,
        policy: CollectorPolicy,
    ) -> Result<MerchantRecord, ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let external_id = bounded(external_id, 1, 200, "external id must be 1-200 characters")?;
        let display_name = bounded(
            display_name,
            1,
            200,
            "display name must be 1-200 characters",
        )?;
        self.repository
            .create_merchant(actor, Uuid::now_v7(), external_id, display_name, policy)
            .await
    }

    /// Issues a merchant API key. The secret is returned once and never stored.
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for invalid fields or an unknown merchant.
    pub async fn issue_api_key(
        &self,
        actor: &str,
        merchant_id: Uuid,
        label: &str,
    ) -> Result<IssuedApiKey, ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let label = bounded(label, 1, 100, "label must be 1-100 characters")?;
        let mut raw = [0_u8; 32];
        self.random.fill(&mut raw)?;
        let secret = format!("gw_{}", hex(&raw));
        let prefix = secret[..API_KEY_PREFIX_CHARS].to_owned();
        let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        let key_id = Uuid::now_v7();
        self.repository
            .insert_api_key(actor, merchant_id, key_id, &prefix, &hash, label)
            .await?;
        Ok(IssuedApiKey {
            key_id,
            merchant_id,
            prefix,
            secret,
        })
    }

    /// # Errors
    ///
    /// Returns [`ProvisioningError`] when no reason is given or the key is unknown.
    pub async fn revoke_api_key(
        &self,
        actor: &str,
        key_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let reason = required(reason, "a reason is required")?;
        self.repository.revoke_api_key(actor, key_id, reason).await
    }

    /// Registers a webhook endpoint and returns its signing secret, once.
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an invalid URL, an unknown merchant, or
    /// a missing master key.
    pub async fn add_webhook_endpoint(
        &self,
        actor: &str,
        merchant_id: Uuid,
        url: &str,
        description: Option<&str>,
    ) -> Result<WebhookRegistration, ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let url = url.trim();
        if !url.starts_with("https://") || url.len() > 2_048 {
            return Err(ProvisioningError::Invalid(
                "the webhook URL must be https and at most 2048 characters",
            ));
        }
        let master_key = self.master_key()?;
        let endpoint_id = Uuid::now_v7();
        let secret = SigningSecret::derive(master_key, 1, merchant_id, endpoint_id)?;
        self.repository
            .insert_webhook_endpoint(
                actor,
                merchant_id,
                endpoint_id,
                url,
                description.map(str::trim).filter(|text| !text.is_empty()),
                &secret.fingerprint(),
            )
            .await?;
        Ok(WebhookRegistration {
            endpoint_id,
            merchant_id,
            url: url.to_owned(),
            secret_version: 1,
            signing_secret: secret.expose(),
            previous_valid_until: None,
        })
    }

    /// Rotates one endpoint's signing secret. The previous secret keeps
    /// signing deliveries for `transition`, so the merchant can switch without
    /// refusing a single event.
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an unknown or disabled endpoint, a
    /// transition outside 1 hour..=30 days, or a concurrent rotation.
    pub async fn rotate_webhook_secret(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        transition: Duration,
        reason: &str,
    ) -> Result<WebhookRegistration, ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let reason = required(reason, "a reason is required")?;
        if transition < Duration::hours(1) || transition > Duration::days(30) {
            return Err(ProvisioningError::Invalid(
                "the transition period must be between 1 hour and 30 days",
            ));
        }
        let master_key = self.master_key()?;
        let state = self
            .repository
            .endpoint_state(endpoint_id)
            .await?
            .filter(|state| state.active)
            .ok_or(ProvisioningError::EndpointNotFound)?;
        let next = state
            .secret_version
            .checked_add(1)
            .ok_or(ProvisioningError::Invalid(
                "the secret version cannot grow further",
            ))?;
        let previous = SigningSecret::derive(
            master_key,
            state.secret_version,
            state.merchant_id,
            endpoint_id,
        )?;
        let current = SigningSecret::derive(master_key, next, state.merchant_id, endpoint_id)?;
        let valid_until = self.clock.now() + transition;
        self.repository
            .rotate_webhook_secret(
                actor,
                endpoint_id,
                state.secret_version,
                &previous.fingerprint(),
                next,
                &current.fingerprint(),
                valid_until,
                reason,
            )
            .await?;
        Ok(WebhookRegistration {
            endpoint_id,
            merchant_id: state.merchant_id,
            url: String::new(),
            secret_version: next,
            signing_secret: current.expose(),
            previous_valid_until: Some(valid_until),
        })
    }

    /// # Errors
    ///
    /// Returns [`ProvisioningError`] when no reason is given or the endpoint is unknown.
    pub async fn disable_webhook_endpoint(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let reason = required(reason, "a reason is required")?;
        self.repository
            .disable_webhook_endpoint(actor, endpoint_id, reason)
            .await
    }

    /// Queues a signed `webhook.test` event, delivered like any other.
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] for an unknown or disabled endpoint.
    pub async fn send_test_event(
        &self,
        actor: &str,
        endpoint_id: Uuid,
    ) -> Result<Uuid, ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let event_id = Uuid::now_v7();
        self.repository
            .enqueue_test_event(actor, endpoint_id, event_id)
            .await?;
        Ok(event_id)
    }

    /// Registers a collector address. A merchant-owned collector requires the
    /// caller to have established control of the address; `evidence` records
    /// how (a verified signature, or a named manual check).
    ///
    /// # Errors
    ///
    /// Returns [`ProvisioningError`] when evidence is missing, the address is
    /// taken, or the merchant's policy does not allow the collector.
    pub async fn register_collector(
        &self,
        actor: &str,
        collector: NewCollector,
    ) -> Result<Uuid, ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        required(
            &collector.ownership_evidence,
            "ownership evidence is required",
        )?;
        self.repository
            .register_collector(actor, &collector)
            .await?;
        Ok(collector.id)
    }

    /// # Errors
    ///
    /// Returns [`ProvisioningError`] when no reason is given or the collector is unknown.
    pub async fn retire_collector(
        &self,
        actor: &str,
        collector_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let actor = required(actor, "an actor naming the person is required")?;
        let reason = required(reason, "a reason is required")?;
        self.repository
            .retire_collector(actor, collector_id, reason)
            .await
    }

    fn master_key(&self) -> Result<&[u8], ProvisioningError> {
        self.master_key
            .as_deref()
            .ok_or(ProvisioningError::MasterKeyMissing)
    }
}

fn required<'a>(value: &'a str, message: &'static str) -> Result<&'a str, ProvisioningError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(ProvisioningError::Invalid(message))
    } else {
        Ok(trimmed)
    }
}

fn bounded<'a>(
    value: &'a str,
    min: usize,
    max: usize,
    message: &'static str,
) -> Result<&'a str, ProvisioningError> {
    let trimmed = value.trim();
    let length = trimmed.chars().count();
    if length < min || length > max {
        Err(ProvisioningError::Invalid(message))
    } else {
        Ok(trimmed)
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: [u8; 16] = *b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests;
