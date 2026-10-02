//! Payment intents: the merchant's obligation, its idempotent creation and
//! its status transitions. One trait, one file.

use async_trait::async_trait;
use gateway_application::{
    ApiCredential, IdempotentCreate, PaymentIntentRepository, RepositoryError,
};
use gateway_domain::PaymentIntent;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    PaymentIntentRow, PostgresRepository, classify_insert_error, insert_system_audit, unavailable,
};

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

    #[allow(clippy::too_many_lines)]
    async fn cancel_idempotently(
        &self,
        merchant_id: Uuid,
        intent_id: Uuid,
        actor_key_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        reason: Option<&str>,
    ) -> Result<Option<IdempotentCreate>, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(unavailable)?;
        let recorded = sqlx::query_scalar::<_, Uuid>(
            r"
            INSERT INTO api_idempotency_records (
                merchant_id, route, idempotency_key, request_hash, resource_id
            ) VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (merchant_id, route, idempotency_key) DO NOTHING
            RETURNING resource_id
            ",
        )
        .bind(merchant_id)
        .bind(route)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .bind(intent_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?;
        if recorded.is_none() {
            let (stored_hash, resource) = sqlx::query_as::<_, (Vec<u8>, Uuid)>(
                "SELECT request_hash, resource_id FROM api_idempotency_records \
                 WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3",
            )
            .bind(merchant_id)
            .bind(route)
            .bind(idempotency_key)
            .fetch_one(&mut *transaction)
            .await
            .map_err(unavailable)?;
            if stored_hash.as_slice() != request_hash {
                return Err(RepositoryError::IdempotencyConflict);
            }
            let intent = find_intent(&mut transaction, merchant_id, resource).await?;
            transaction.commit().await.map_err(unavailable)?;
            return Ok(intent.map(IdempotentCreate::Replayed));
        }

        // The intent row is the lock settlement also takes before it can mark
        // the order paid, so a payment and a cancellation serialise here.
        let Some(status) = sqlx::query_scalar::<_, String>(
            "SELECT status FROM payment_intents WHERE id = $1 AND merchant_id = $2 FOR UPDATE",
        )
        .bind(intent_id)
        .bind(merchant_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?
        else {
            return Ok(None);
        };
        let money_or_decision = sqlx::query_scalar::<_, bool>(
            r"
            SELECT EXISTS (
                SELECT 1 FROM payment_settlement_decisions WHERE payment_intent_id = $1
                UNION ALL
                SELECT 1 FROM chain_transfer_intent_claims WHERE payment_intent_id = $1
            )
            ",
        )
        .bind(intent_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(unavailable)?;
        if !matches!(
            status.as_str(),
            "requires_quote" | "awaiting_payment" | "expired"
        ) || money_or_decision
        {
            return Err(RepositoryError::PaymentIntentNotCancellable);
        }
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            r"
            UPDATE payment_intents
               SET status = 'cancelled', version = version + 1, updated_at = $2
             WHERE id = $1
            ",
        )
        .bind(intent_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;
        // The live attempt closes too. Its amount reservation stays until its
        // late-payment window ends: money that still arrives is recognised and
        // held for a person, never credited elsewhere.
        let cancelled_attempts = sqlx::query_scalar::<_, Uuid>(
            r"
            UPDATE payment_attempts SET status = 'cancelled', updated_at = $2
             WHERE payment_intent_id = $1 AND status = 'awaiting_payment'
            RETURNING id
            ",
        )
        .bind(intent_id)
        .bind(now)
        .fetch_all(&mut *transaction)
        .await
        .map_err(unavailable)?;
        insert_system_audit(
            &mut transaction,
            merchant_id,
            "payment_intent.cancelled",
            "payment_intent",
            intent_id,
            serde_json::json!({
                "actor_key_id": actor_key_id,
                "reason": reason,
                "previous_status": status,
                "cancelled_attempts": cancelled_attempts,
            }),
        )
        .await?;
        sqlx::query(
            r"
            INSERT INTO domain_events (
                id, merchant_id, channel, event_type, aggregate_type, aggregate_id, payload,
                available_at, created_at
            ) VALUES ($1, $2, 'webhook', 'payment_intent.cancelled', 'payment_intent', $3, $4, $5, $5)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(merchant_id)
        .bind(intent_id)
        .bind(serde_json::json!({"payment_intent_id": intent_id, "reason": reason}))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;
        sqlx::query(
            "UPDATE api_idempotency_records SET completed_at = now() \
             WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3",
        )
        .bind(merchant_id)
        .bind(route)
        .bind(idempotency_key)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;
        let intent = find_intent(&mut transaction, merchant_id, intent_id)
            .await?
            .ok_or_else(|| {
                RepositoryError::CorruptData("a cancelled intent is missing".to_owned())
            })?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(Some(IdempotentCreate::Created(intent)))
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
