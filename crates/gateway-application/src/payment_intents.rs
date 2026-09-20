use std::sync::Arc;

use gateway_domain::{CurrencyCode, FiatAmount, PaymentIntent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{Clock, IdempotentCreate, PaymentIntentRepository, RepositoryError};

const CREATE_ROUTE: &str = "POST /v1/payment-intents";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePaymentIntent {
    pub amount_minor: String,
    pub currency: String,
    pub reference: String,
    pub description: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePaymentIntentResult {
    pub intent: PaymentIntent,
    pub replayed: bool,
}

#[derive(Debug)]
pub struct PaymentIntentService<R, C> {
    repository: Arc<R>,
    clock: C,
}

impl<R, C> PaymentIntentService<R, C>
where
    R: PaymentIntentRepository,
    C: Clock,
{
    pub const fn new(repository: Arc<R>, clock: C) -> Self {
        Self { repository, clock }
    }

    /// Validates and persists a payment intent under a merchant-scoped
    /// idempotency key.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] for invalid input, an idempotency conflict, or
    /// unavailable storage.
    pub async fn create(
        &self,
        merchant_id: Uuid,
        actor_key_id: Uuid,
        idempotency_key: &str,
        input: CreatePaymentIntent,
    ) -> Result<CreatePaymentIntentResult, ServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let request_hash = request_hash(&input)?;
        let currency = CurrencyCode::new(input.currency)?;
        let amount = FiatAmount::parse_positive(currency, &input.amount_minor)?;
        let intent = PaymentIntent::create(
            merchant_id,
            amount,
            input.reference,
            input.description,
            input.metadata,
            self.clock.now(),
        )?;

        match self
            .repository
            .create_idempotently(
                intent,
                actor_key_id,
                CREATE_ROUTE,
                idempotency_key,
                &request_hash,
            )
            .await?
        {
            IdempotentCreate::Created(intent) => Ok(CreatePaymentIntentResult {
                intent,
                replayed: false,
            }),
            IdempotentCreate::Replayed(intent) => Ok(CreatePaymentIntentResult {
                intent,
                replayed: true,
            }),
        }
    }

    /// Loads a payment intent owned by the merchant.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NotFound`] when the intent is absent or owned by
    /// another merchant, and a repository error when storage is unavailable.
    pub async fn get(
        &self,
        merchant_id: Uuid,
        intent_id: Uuid,
    ) -> Result<PaymentIntent, ServiceError> {
        self.repository
            .find_by_id(merchant_id, intent_id)
            .await?
            .ok_or(ServiceError::NotFound)
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), ServiceError> {
    let valid = (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !valid {
        return Err(ServiceError::InvalidIdempotencyKey);
    }
    Ok(())
}

fn request_hash(input: &CreatePaymentIntent) -> Result<[u8; 32], ServiceError> {
    let payload =
        serde_json::to_vec(input).map_err(|error| ServiceError::Internal(error.to_string()))?;
    Ok(Sha256::digest(payload).into())
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("idempotency key must contain 16 to 128 URL-safe characters")]
    InvalidIdempotencyKey,
    #[error("payment intent was not found")]
    NotFound,
    #[error(transparent)]
    Money(#[from] gateway_domain::MoneyError),
    #[error(transparent)]
    PaymentIntent(#[from] gateway_domain::PaymentIntentError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use gateway_domain::PaymentIntent;
    use time::OffsetDateTime;

    use super::*;
    use crate::{ApiCredential, IdempotentCreate, RepositoryError};

    #[derive(Debug)]
    struct CreatingRepository;

    #[async_trait]
    impl PaymentIntentRepository for CreatingRepository {
        async fn authenticate_api_key(
            &self,
            _secret_hash: &[u8; 32],
        ) -> Result<Option<ApiCredential>, RepositoryError> {
            Ok(None)
        }

        async fn create_idempotently(
            &self,
            intent: PaymentIntent,
            _actor_key_id: Uuid,
            _route: &str,
            _idempotency_key: &str,
            _request_hash: &[u8; 32],
        ) -> Result<IdempotentCreate, RepositoryError> {
            Ok(IdempotentCreate::Created(intent))
        }

        async fn find_by_id(
            &self,
            _merchant_id: Uuid,
            _intent_id: Uuid,
        ) -> Result<Option<PaymentIntent>, RepositoryError> {
            Ok(None)
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::UNIX_EPOCH
        }
    }

    #[tokio::test]
    async fn creates_intent_without_float_money() -> Result<(), ServiceError> {
        let service = PaymentIntentService::new(Arc::new(CreatingRepository), FixedClock);
        let result = service
            .create(
                Uuid::nil(),
                Uuid::nil(),
                "checkout_01JABCDEFG",
                CreatePaymentIntent {
                    amount_minor: "12345".to_owned(),
                    currency: "usd".to_owned(),
                    reference: "order-123".to_owned(),
                    description: None,
                    metadata: empty_object(),
                },
            )
            .await?;

        assert_eq!(result.intent.amount.minor_units, 12_345);
        assert_eq!(result.intent.amount.currency.as_str(), "USD");
        assert!(!result.replayed);
        Ok(())
    }

    #[tokio::test]
    async fn rejects_weak_idempotency_keys_before_storage() {
        let service = PaymentIntentService::new(Arc::new(CreatingRepository), FixedClock);
        let result = service
            .create(
                Uuid::nil(),
                Uuid::nil(),
                "short",
                CreatePaymentIntent {
                    amount_minor: "1".to_owned(),
                    currency: "USD".to_owned(),
                    reference: "order-123".to_owned(),
                    description: None,
                    metadata: empty_object(),
                },
            )
            .await;

        assert!(matches!(result, Err(ServiceError::InvalidIdempotencyKey)));
    }
}
