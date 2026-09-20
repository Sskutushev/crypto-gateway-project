use std::str::FromStr;

use async_trait::async_trait;
use gateway_application::{
    ApiCredential, IdempotentCreate, PaymentIntentRepository, RepositoryError,
};
use gateway_domain::{CurrencyCode, FiatAmount, PaymentIntent, PaymentIntentStatus};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PostgresRepository {
    pool: PgPool,
}

impl PostgresRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct PaymentIntentRow {
    id: Uuid,
    merchant_id: Uuid,
    amount_minor: i64,
    currency: String,
    status: String,
    reference: String,
    description: Option<String>,
    metadata: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<PaymentIntentRow> for PaymentIntent {
    type Error = RepositoryError;

    fn try_from(row: PaymentIntentRow) -> Result<Self, Self::Error> {
        let currency = CurrencyCode::new(row.currency)
            .map_err(|error| RepositoryError::CorruptData(error.to_string()))?;
        let amount = FiatAmount::positive(currency, row.amount_minor)
            .map_err(|error| RepositoryError::CorruptData(error.to_string()))?;
        let status = PaymentIntentStatus::from_str(&row.status)
            .map_err(|error| RepositoryError::CorruptData(error.to_string()))?;
        if !row.metadata.is_object() {
            return Err(RepositoryError::CorruptData(
                "payment intent metadata is not an object".to_owned(),
            ));
        }

        Ok(Self {
            id: row.id,
            merchant_id: row.merchant_id,
            amount,
            status,
            reference: row.reference,
            description: row.description,
            metadata: row.metadata,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[async_trait]
impl PaymentIntentRepository for PostgresRepository {
    async fn authenticate_api_key(
        &self,
        secret_hash: &[u8; 32],
    ) -> Result<Option<ApiCredential>, RepositoryError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid)>(
            r"
            UPDATE merchant_api_keys AS api_key
               SET last_used_at = now()
              FROM merchants AS merchant
             WHERE api_key.secret_hash = $1
               AND api_key.revoked_at IS NULL
               AND merchant.id = api_key.merchant_id
               AND merchant.status = 'active'
            RETURNING api_key.merchant_id, api_key.id
            ",
        )
        .bind(secret_hash.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;

        Ok(row.map(|(merchant_id, key_id)| ApiCredential {
            merchant_id,
            key_id,
        }))
    }

    async fn create_idempotently(
        &self,
        intent: PaymentIntent,
        actor_key_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<IdempotentCreate, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(unavailable)?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            r"
            INSERT INTO api_idempotency_records (
                merchant_id, route, idempotency_key, request_hash, resource_id
            ) VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (merchant_id, route, idempotency_key) DO NOTHING
            RETURNING resource_id
            ",
        )
        .bind(intent.merchant_id)
        .bind(route)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .bind(intent.id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?;

        if inserted.is_some() {
            insert_intent(&mut transaction, &intent).await?;
            insert_creation_audit_event(&mut transaction, &intent, actor_key_id).await?;
            sqlx::query(
                r"
                UPDATE api_idempotency_records
                   SET completed_at = now()
                 WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3
                ",
            )
            .bind(intent.merchant_id)
            .bind(route)
            .bind(idempotency_key)
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
            transaction.commit().await.map_err(unavailable)?;
            return Ok(IdempotentCreate::Created(intent));
        }

        let existing = sqlx::query_as::<_, (Vec<u8>, Uuid)>(
            r"
            SELECT request_hash, resource_id
              FROM api_idempotency_records
             WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3
            ",
        )
        .bind(intent.merchant_id)
        .bind(route)
        .bind(idempotency_key)
        .fetch_one(&mut *transaction)
        .await
        .map_err(unavailable)?;

        if existing.0.as_slice() != request_hash {
            return Err(RepositoryError::IdempotencyConflict);
        }

        let replayed = find_intent(&mut transaction, intent.merchant_id, existing.1)
            .await?
            .ok_or_else(|| {
                RepositoryError::CorruptData(
                    "idempotency record references a missing payment intent".to_owned(),
                )
            })?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(IdempotentCreate::Replayed(replayed))
    }

    async fn find_by_id(
        &self,
        merchant_id: Uuid,
        intent_id: Uuid,
    ) -> Result<Option<PaymentIntent>, RepositoryError> {
        let row = sqlx::query_as::<_, PaymentIntentRow>(
            r"
            SELECT id, merchant_id, amount_minor, currency, status, reference,
                   description, metadata, created_at, updated_at
              FROM payment_intents
             WHERE merchant_id = $1 AND id = $2
            ",
        )
        .bind(merchant_id)
        .bind(intent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        row.map(TryInto::try_into).transpose()
    }
}

async fn insert_intent(
    transaction: &mut Transaction<'_, Postgres>,
    intent: &PaymentIntent,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO payment_intents (
            id, merchant_id, amount_minor, currency, status, reference,
            description, metadata, created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ",
    )
    .bind(intent.id)
    .bind(intent.merchant_id)
    .bind(intent.amount.minor_units)
    .bind(intent.amount.currency.as_str())
    .bind(intent.status.as_str())
    .bind(&intent.reference)
    .bind(&intent.description)
    .bind(&intent.metadata)
    .bind(intent.created_at)
    .bind(intent.updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(classify_insert_error)?;
    Ok(())
}

async fn insert_creation_audit_event(
    transaction: &mut Transaction<'_, Postgres>,
    intent: &PaymentIntent,
    actor_key_id: Uuid,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO audit_events (
            id, merchant_id, actor_type, actor_id, action, resource_type,
            resource_id, payload
        ) VALUES ($1, $2, 'api_key', $3, 'payment_intent.created',
                  'payment_intent', $4, $5)
        ",
    )
    .bind(Uuid::now_v7())
    .bind(intent.merchant_id)
    .bind(actor_key_id)
    .bind(intent.id)
    .bind(serde_json::json!({
        "reference": intent.reference,
        "currency": intent.amount.currency.as_str(),
        "amount_minor": intent.amount.minor_units.to_string(),
    }))
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

async fn find_intent(
    transaction: &mut Transaction<'_, Postgres>,
    merchant_id: Uuid,
    intent_id: Uuid,
) -> Result<Option<PaymentIntent>, RepositoryError> {
    let row = sqlx::query_as::<_, PaymentIntentRow>(
        r"
        SELECT id, merchant_id, amount_minor, currency, status, reference,
               description, metadata, created_at, updated_at
          FROM payment_intents
         WHERE merchant_id = $1 AND id = $2
        ",
    )
    .bind(merchant_id)
    .bind(intent_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;
    row.map(TryInto::try_into).transpose()
}

// `Result::map_err` passes the owned SQLx error, so this adapter intentionally
// accepts it by value even though formatting borrows it.
#[allow(clippy::needless_pass_by_value)]
fn unavailable(error: sqlx::Error) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn classify_insert_error(error: sqlx::Error) -> RepositoryError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.constraint() == Some("payment_intents_merchant_id_reference_key")
    {
        return RepositoryError::DuplicateReference;
    }
    unavailable(error)
}
