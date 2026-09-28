use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use async_trait::async_trait;
use gateway_domain::{IssuedQuote, QuoteError, QuotePlan};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Clock, ExpiryResult, IdempotentQuote, PaymentIntentRepository, QuoteMetrics, QuoteRepository,
    RepositoryError,
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

/// How many of a merchant's addresses one quote request may try when the
/// exact amounts near its price are taken on the first.
const MAX_COLLECTOR_ATTEMPTS: usize = 8;

#[derive(Debug)]
pub struct QuoteService<R, C> {
    repository: Arc<R>,
    clock: C,
    max_open_leases_per_collector: Option<u64>,
    metrics: Arc<QuoteMetrics>,
}

impl<R, C> QuoteService<R, C>
where
    R: PaymentIntentRepository + QuoteRepository,
    C: Clock,
{
    pub fn new(repository: Arc<R>, clock: C) -> Self {
        Self {
            repository,
            clock,
            max_open_leases_per_collector: None,
            metrics: Arc::new(QuoteMetrics::default()),
        }
    }

    /// Refuses new quotes on an address once it holds this many amount
    /// reservations, and moves on to the merchant's next address. The check
    /// reads the count before the collector lock is taken, so concurrent
    /// quotes may pass it together: it is admission control, not a ledger
    /// invariant.
    #[must_use]
    pub const fn with_max_open_leases_per_collector(mut self, limit: u64) -> Self {
        self.max_open_leases_per_collector = Some(limit);
        self
    }

    #[must_use]
    pub fn metrics(&self) -> Arc<QuoteMetrics> {
        Arc::clone(&self.metrics)
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
        let started = Instant::now();
        let result = self
            .issue_inner(merchant_id, actor_key_id, idempotency_key, input)
            .await;
        self.metrics.record(&result, started.elapsed());
        result
    }

    async fn issue_inner(
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
            .load_quote_context(merchant_id, input.asset_id, &intent.amount.currency)
            .await?;
        if let Some(reason) = context.rail_stop_reason {
            return Err(QuoteServiceError::RailStopped(reason));
        }
        let admitted: Vec<_> = context
            .candidates
            .iter()
            .filter(|candidate| {
                self.max_open_leases_per_collector
                    .is_none_or(|limit| candidate.open_leases < limit)
            })
            .take(MAX_COLLECTOR_ATTEMPTS)
            .collect();
        if admitted.is_empty() {
            return Err(QuoteServiceError::CapacityExhausted);
        }

        let mut last_refusal = None;
        for (position, candidate) in admitted.into_iter().enumerate() {
            if position > 0 {
                self.metrics.spillovers.fetch_add(1, Ordering::Relaxed);
            }
            let plan = QuotePlan::build(
                merchant_id,
                intent.id,
                input.asset_id,
                candidate.id,
                candidate.address.clone(),
                intent.amount.clone(),
                context.price.clone(),
                context.policy.clone(),
                context.rail_health.clone(),
                self.clock.now(),
            )?;
            // A refused issue rolls its transaction back, idempotency record
            // included, so the next address starts from a clean slate.
            match self
                .repository
                .issue_quote_idempotently(
                    plan,
                    actor_key_id,
                    ISSUE_ROUTE,
                    idempotency_key,
                    &request_hash,
                )
                .await
            {
                Ok(IdempotentQuote::Issued(quote)) => {
                    return Ok(IssueQuoteResult {
                        quote,
                        replayed: false,
                    });
                }
                Ok(IdempotentQuote::Replayed(quote)) => {
                    return Ok(IssueQuoteResult {
                        quote,
                        replayed: true,
                    });
                }
                // Every exact amount near this price is taken on this address,
                // or it stopped quoting since the context was read: another of
                // the merchant's addresses may still serve the order.
                Err(
                    error @ (RepositoryError::AmountSlotsExhausted
                    | RepositoryError::CollectorUnavailable),
                ) => last_refusal = Some(error),
                Err(error) => return Err(error.into()),
            }
        }
        Err(last_refusal
            .unwrap_or(RepositoryError::CollectorUnavailable)
            .into())
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
    #[error("this rail is closed: {0}")]
    RailStopped(String),
    #[error("every address for this merchant holds its maximum of open reservations")]
    CapacityExhausted,
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
