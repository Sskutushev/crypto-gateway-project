use async_trait::async_trait;
use gateway_domain::{
    CurrencyCode, IssuedQuote, PaymentIntent, PriceSnapshot, QuotePlan, QuotePolicySnapshot,
    RailHealthSnapshot,
};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotentQuote {
    Issued(IssuedQuote),
    Replayed(IssuedQuote),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryResult {
    pub quotes_expired: u64,
    pub leases_archived: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteContext {
    pub collector_address_id: Uuid,
    pub collector_address: String,
    pub price: Option<PriceSnapshot>,
    pub policy: Option<QuotePolicySnapshot>,
    pub rail_health: Option<RailHealthSnapshot>,
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

#[async_trait]
pub trait QuoteRepository: Send + Sync {
    async fn find_quote_replay(
        &self,
        merchant_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<Option<IssuedQuote>, RepositoryError>;

    async fn load_quote_context(
        &self,
        asset_id: Uuid,
        currency: &CurrencyCode,
    ) -> Result<QuoteContext, RepositoryError>;

    async fn issue_quote_idempotently(
        &self,
        plan: QuotePlan,
        actor_key_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<IdempotentQuote, RepositoryError>;

    async fn expire_quotes_and_archive_leases(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<ExpiryResult, RepositoryError>;
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
    #[error("the payment intent cannot receive a quote in its current state")]
    PaymentIntentNotQuotable,
    #[error("the requested asset or collector address is unavailable")]
    CollectorUnavailable,
    #[error("all exact-amount slots are currently leased")]
    AmountSlotsExhausted,
    #[error("stored data violated a domain invariant: {0}")]
    CorruptData(String),
    #[error("this component no longer holds its lease")]
    LeaseLost,
    #[error("storage is unavailable: {0}")]
    Unavailable(String),
}

impl RepositoryError {
    /// Reports whether a retry can plausibly succeed without operator action.
    ///
    /// Only storage outages are transient. Conflicts and invariant violations
    /// describe stored state, so retrying them would hide a real defect.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}
