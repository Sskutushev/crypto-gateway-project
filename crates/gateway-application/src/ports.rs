use async_trait::async_trait;
use gateway_domain::PaymentIntent;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCredential {
    pub merchant_id: Uuid,
    pub key_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotentCreate {
    Created(PaymentIntent),
    Replayed(PaymentIntent),
}

#[async_trait]
pub trait PaymentIntentRepository: Send + Sync {
    async fn authenticate_api_key(
        &self,
        secret_hash: &[u8; 32],
    ) -> Result<Option<ApiCredential>, RepositoryError>;

    async fn create_idempotently(
        &self,
        intent: PaymentIntent,
        actor_key_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<IdempotentCreate, RepositoryError>;

    async fn find_by_id(
        &self,
        merchant_id: Uuid,
        intent_id: Uuid,
    ) -> Result<Option<PaymentIntent>, RepositoryError>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("an idempotency key was reused with a different request")]
    IdempotencyConflict,
    #[error("the payment intent reference already exists for this merchant")]
    DuplicateReference,
    #[error("stored data violated a domain invariant: {0}")]
    CorruptData(String),
    #[error("storage is unavailable: {0}")]
    Unavailable(String),
}
