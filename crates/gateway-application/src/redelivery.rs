//! Sending a webhook event again, as the same event.
//!
//! A merchant whose endpoint was down, or who lost an event on their side,
//! asks for it again. The event is re-queued in place: the same id and the
//! same payload, so a merchant that already processed it deduplicates it, and
//! nothing about the payment it describes changes. Both the operator API and
//! the admin command line reach this through [`validate_redelivery`] and one
//! repository call, so the two paths cannot drift apart.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::RepositoryError;

/// Who asked for a redelivery. It is also the idempotency scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeliveryActor {
    /// An operator API key with the `admin` scope.
    OperatorKey { key_id: Uuid, label: String },
    /// A named person at the admin command line.
    Admin { name: String },
}

impl RedeliveryActor {
    /// The stored idempotency scope.
    #[must_use]
    pub fn principal(&self) -> String {
        match self {
            Self::OperatorKey { key_id, .. } => format!("operator_key:{key_id}"),
            Self::Admin { name } => format!("admin:{name}"),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::OperatorKey { label, .. } => label,
            Self::Admin { name } => name,
        }
    }

    #[must_use]
    pub const fn operator_key_id(&self) -> Option<Uuid> {
        match self {
            Self::OperatorKey { key_id, .. } => Some(*key_id),
            Self::Admin { .. } => None,
        }
    }
}

/// One redelivery request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRedelivery {
    pub event_id: Uuid,
    /// One endpoint of the event's merchant, or `None` for every active one.
    pub endpoint_id: Option<Uuid>,
    pub reason: String,
}

/// What a redelivery did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeliveryResult {
    pub id: Uuid,
    pub event_id: Uuid,
    pub merchant_id: Uuid,
    pub endpoint_id: Option<Uuid>,
    /// `delivered` or `dead_lettered`: what the event was before.
    pub previous_state: String,
    pub previous_attempts: i32,
    pub requested_at: OffsetDateTime,
    pub replayed: bool,
}

#[derive(Debug, Error)]
pub enum RedeliveryError {
    #[error("the idempotency key must contain between 16 and 128 characters")]
    InvalidIdempotencyKey,
    #[error("a reason of 1-1000 characters is required and recorded")]
    ReasonRequired,
    #[error("the webhook event does not exist")]
    EventNotFound,
    #[error("the endpoint is not an active endpoint of the event's merchant")]
    EndpointNotFound,
    #[error("the event is still queued for delivery; it is not redelivered twice")]
    StillQueued,
    #[error("the event is not a merchant webhook")]
    NotAWebhook,
    #[error("the idempotency key was already used for a different redelivery")]
    IdempotencyConflict,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

#[async_trait]
pub trait RedeliveryRepository: Send + Sync {
    /// Re-queues one delivered or dead-lettered webhook event, records the
    /// request and its audit row in the same transaction, and answers a
    /// replay of the same request with the first result.
    async fn redeliver_webhook_event(
        &self,
        actor: &RedeliveryActor,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        request: &WebhookRedelivery,
        now: OffsetDateTime,
    ) -> Result<RedeliveryResult, RedeliveryError>;
}

/// Checks the request and returns the hash its idempotency key is bound to.
///
/// # Errors
///
/// Returns [`RedeliveryError`] for a malformed key or a missing reason.
pub fn validate_redelivery(
    idempotency_key: &str,
    request: &WebhookRedelivery,
) -> Result<[u8; 32], RedeliveryError> {
    let valid_key = (16..=128).contains(&idempotency_key.len())
        && idempotency_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !valid_key {
        return Err(RedeliveryError::InvalidIdempotencyKey);
    }
    if !(1..=1000).contains(&request.reason.trim().chars().count()) {
        return Err(RedeliveryError::ReasonRequired);
    }
    let mut digest = Sha256::new();
    for value in [
        request.event_id.to_string(),
        request
            .endpoint_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        request.reason.clone(),
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> WebhookRedelivery {
        WebhookRedelivery {
            event_id: Uuid::from_u128(1),
            endpoint_id: None,
            reason: "merchant lost the event during their outage".to_owned(),
        }
    }

    #[test]
    fn a_redelivery_needs_a_well_formed_key_and_a_reason() {
        assert!(matches!(
            validate_redelivery("short", &request()),
            Err(RedeliveryError::InvalidIdempotencyKey)
        ));
        assert!(matches!(
            validate_redelivery("redeliver key with spaces", &request()),
            Err(RedeliveryError::InvalidIdempotencyKey)
        ));
        let mut silent = request();
        silent.reason = "   ".to_owned();
        assert!(matches!(
            validate_redelivery("redeliver-0000000001", &silent),
            Err(RedeliveryError::ReasonRequired)
        ));
    }

    #[test]
    fn the_hash_binds_the_event_the_endpoint_and_the_reason() -> Result<(), RedeliveryError> {
        let base = validate_redelivery("redeliver-0000000001", &request())?;
        let mut aimed = request();
        aimed.endpoint_id = Some(Uuid::from_u128(2));
        let mut other_reason = request();
        other_reason.reason.push('.');
        assert_ne!(base, validate_redelivery("redeliver-0000000001", &aimed)?);
        assert_ne!(
            base,
            validate_redelivery("redeliver-0000000001", &other_reason)?
        );
        assert_eq!(
            base,
            validate_redelivery("redeliver-0000000002", &request())?
        );
        Ok(())
    }

    #[test]
    fn the_principal_separates_keys_from_people() {
        let key = RedeliveryActor::OperatorKey {
            key_id: Uuid::from_u128(7),
            label: "on-call".to_owned(),
        };
        let person = RedeliveryActor::Admin {
            name: "on-call".to_owned(),
        };
        assert_ne!(key.principal(), person.principal());
        assert_eq!(key.operator_key_id(), Some(Uuid::from_u128(7)));
        assert_eq!(person.operator_key_id(), None);
    }
}
