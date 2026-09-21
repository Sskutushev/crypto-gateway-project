use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{IssuedQuote, QuoteError, QuotePlan};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Clock, ExpiryResult, IdempotentQuote, PaymentIntentRepository, QuoteRepository, RepositoryError,
};

const ISSUE_ROUTE: &str = "POST /v1/payment-intents/:id/quotes";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueQuote {
    pub payment_intent_id: Uuid,
    pub asset_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueQuoteResult {
    pub quote: IssuedQuote,
    pub replayed: bool,
}

#[derive(Debug)]
pub struct QuoteService<R, C> {
    repository: Arc<R>,
    clock: C,
}

impl<R, C> QuoteService<R, C>
where
    R: PaymentIntentRepository + QuoteRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C) -> Self {
        Self { repository, clock }
    }

    /// Issues one immutable quote and exact-amount lease.
    ///
    /// Evidence is deliberately supplied by trusted application adapters, not
    /// by a public HTTP payload. Missing or stale evidence fails closed.
    ///
    /// # Errors
    ///
    /// Returns [`QuoteServiceError`] when evidence, intent state, idempotency,
    /// collector state, or storage prevents safe issuance.
    pub async fn issue(
        &self,
        merchant_id: Uuid,
        actor_key_id: Uuid,
        idempotency_key: &str,
        input: IssueQuote,
    ) -> Result<IssueQuoteResult, QuoteServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let request_hash = request_hash(&input);
        if let Some(quote) = self
            .repository
            .find_quote_replay(merchant_id, ISSUE_ROUTE, idempotency_key, &request_hash)
            .await?
        {
            return Ok(IssueQuoteResult {
                quote,
                replayed: true,
            });
        }
        let intent = self
            .repository
            .find_by_id(merchant_id, input.payment_intent_id)
            .await?
            .ok_or(QuoteServiceError::PaymentIntentNotFound)?;
        let context = self
            .repository
            .load_quote_context(input.asset_id, &intent.amount.currency)
            .await?;
        let plan = QuotePlan::build(
            merchant_id,
            intent.id,
            input.asset_id,
            context.collector_address_id,
            context.collector_address,
            intent.amount,
            context.price,
            context.policy,
            context.rail_health,
            self.clock.now(),
        )?;

        match self
            .repository
            .issue_quote_idempotently(
                plan,
                actor_key_id,
                ISSUE_ROUTE,
                idempotency_key,
                &request_hash,
            )
            .await?
        {
            IdempotentQuote::Issued(quote) => Ok(IssueQuoteResult {
                quote,
                replayed: false,
            }),
            IdempotentQuote::Replayed(quote) => Ok(IssueQuoteResult {
                quote,
                replayed: true,
            }),
        }
    }

    /// Archives leases whose separately retained late-payment window ended.
    ///
    /// # Errors
    ///
    /// Returns [`QuoteServiceError`] when the batch limit is invalid or the
    /// database transaction cannot complete.
    pub async fn expire_due(&self, limit: u32) -> Result<ExpiryResult, QuoteServiceError> {
        if limit == 0 || limit > 10_000 {
            return Err(QuoteServiceError::InvalidExpiryBatchLimit);
        }
        Ok(self
            .repository
            .expire_quotes_and_archive_leases(self.clock.now(), limit)
            .await?)
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), QuoteServiceError> {
    let valid = (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !valid {
        return Err(QuoteServiceError::InvalidIdempotencyKey);
    }
    Ok(())
}

fn request_hash(input: &IssueQuote) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(input.payment_intent_id.as_bytes());
    digest.update(input.asset_id.as_bytes());
    digest.finalize().into()
}

#[derive(Debug, Error)]
pub enum QuoteServiceError {
    #[error("idempotency key must contain 16 to 128 URL-safe characters")]
    InvalidIdempotencyKey,
    #[error("payment intent was not found")]
    PaymentIntentNotFound,
    #[error("expiry batch limit must be between 1 and 10000")]
    InvalidExpiryBatchLimit,
    #[error(transparent)]
    Quote(#[from] QuoteError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl QuoteServiceError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            _ => false,
        }
    }
}

/// Bounded sweep of quotes whose deadlines passed and of leases whose
/// separately retained late-payment window ended.
///
/// The scheduler that drives this port must stay outside the application
/// layer, so the sweep is expressed as one bounded, retryable operation
/// instead of an owned background task.
#[async_trait]
pub trait ExpirySweeper: Send + Sync {
    /// Runs one bounded expiry batch.
    ///
    /// # Errors
    ///
    /// Returns [`QuoteServiceError`] when the batch limit is invalid or the
    /// database transaction cannot complete.
    async fn sweep_expired(&self, limit: u32) -> Result<ExpiryResult, QuoteServiceError>;
}

#[async_trait]
impl<R, C> ExpirySweeper for QuoteService<R, C>
where
    R: PaymentIntentRepository + QuoteRepository,
    C: Clock,
{
    async fn sweep_expired(&self, limit: u32) -> Result<ExpiryResult, QuoteServiceError> {
        self.expire_due(limit).await
    }
}
