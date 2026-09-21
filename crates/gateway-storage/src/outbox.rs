use async_trait::async_trait;
use gateway_application::{
    DeliveryAttempt, OutboxEvent, OutboxRepository, RepositoryError, WebhookEndpoint,
};
use serde_json::Value;
use sqlx::FromRow;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

#[derive(Debug, FromRow)]
struct OutboxEventRow {
    id: Uuid,
    merchant_id: Option<Uuid>,
    event_type: String,
    aggregate_type: String,
    aggregate_id: Uuid,
    payload: Value,
    attempts: i32,
    created_at: OffsetDateTime,
}

impl From<OutboxEventRow> for OutboxEvent {
    fn from(row: OutboxEventRow) -> Self {
        Self {
            id: row.id,
            merchant_id: row.merchant_id,
            event_type: row.event_type,
            aggregate_type: row.aggregate_type,
            aggregate_id: row.aggregate_id,
            payload: row.payload,
            attempts: row.attempts,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct WebhookEndpointRow {
    id: Uuid,
    merchant_id: Uuid,
    url: String,
    secret_version: i32,
    secret_fingerprint: Vec<u8>,
}

impl TryFrom<WebhookEndpointRow> for WebhookEndpoint {
    type Error = RepositoryError;

    fn try_from(row: WebhookEndpointRow) -> Result<Self, Self::Error> {
        let fingerprint: [u8; 32] = row
            .secret_fingerprint
            .try_into()
            .map_err(|_| corrupt("a webhook endpoint fingerprint is not 32 bytes"))?;
        Ok(Self {
            id: row.id,
            merchant_id: row.merchant_id,
            url: row.url,
            secret_version: row.secret_version,
            secret_fingerprint: fingerprint,
        })
    }
}

#[async_trait]
impl OutboxRepository for PostgresRepository {
    async fn claim_due_events(
        &self,
        holder: &str,
        limit: u32,
        visibility_seconds: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<OutboxEvent>, RepositoryError> {
        if visibility_seconds <= 0 {
            return Err(corrupt(
                "a delivery claim needs a positive visibility window",
            ));
        }
        let claimed_until = now
            .checked_add(Duration::seconds(visibility_seconds))
            .ok_or_else(|| corrupt("the delivery visibility window overflowed"))?;

        // Claiming under SKIP LOCKED lets several delivery workers share the
        // queue without ever handing the same event to two of them.
        let rows = sqlx::query_as::<_, OutboxEventRow>(
            r"
            WITH due AS (
                SELECT id
                  FROM domain_events
                 WHERE channel = 'webhook'
                   AND delivered_at IS NULL
                   AND dead_lettered_at IS NULL
                   AND available_at <= $3
                   AND (claimed_until IS NULL OR claimed_until < $3)
                 ORDER BY available_at
                 LIMIT $2
                 FOR UPDATE SKIP LOCKED
            )
            UPDATE domain_events AS event
               SET claimed_by = $1, claimed_until = $4
              FROM due
             WHERE event.id = due.id
            RETURNING event.id, event.merchant_id, event.event_type, event.aggregate_type,
                      event.aggregate_id, event.payload, event.attempts, event.created_at
            ",
        )
        .bind(holder)
        .bind(i64::from(limit))
        .bind(now)
        .bind(claimed_until)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn active_endpoints(
        &self,
        merchant_id: Uuid,
    ) -> Result<Vec<WebhookEndpoint>, RepositoryError> {
        let rows = sqlx::query_as::<_, WebhookEndpointRow>(
            r"
            SELECT id, merchant_id, url, secret_version, secret_fingerprint
              FROM webhook_endpoints
             WHERE merchant_id = $1 AND status = 'active'
             ORDER BY created_at
            ",
        )
        .bind(merchant_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn record_attempt(
        &self,
        attempt: &DeliveryAttempt,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            r"
            INSERT INTO webhook_deliveries (
                id, event_id, endpoint_id, attempt, response_status, error, duration_ms,
                delivered_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (event_id, endpoint_id, attempt) DO UPDATE
               SET response_status = excluded.response_status,
                   error = excluded.error,
                   duration_ms = excluded.duration_ms,
                   delivered_at = excluded.delivered_at
            ",
        )
        .bind(Uuid::now_v7())
        .bind(attempt.event_id)
        .bind(attempt.endpoint_id)
        .bind(attempt.attempt)
        .bind(attempt.status)
        .bind(attempt.error.as_deref())
        .bind(i32::try_from(attempt.duration_ms).unwrap_or(i32::MAX))
        .bind(now)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    async fn mark_delivered(
        &self,
        event_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            r"
            UPDATE domain_events
               SET delivered_at = $2,
                   attempts = attempts + 1,
                   claimed_by = NULL,
                   claimed_until = NULL,
                   last_error = NULL
             WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL
            ",
        )
        .bind(event_id)
        .bind(now)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    async fn reschedule(
        &self,
        event_id: Uuid,
        available_at: OffsetDateTime,
        error: &str,
        _now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            r"
            UPDATE domain_events
               SET available_at = $2,
                   attempts = attempts + 1,
                   last_error = $3,
                   claimed_by = NULL,
                   claimed_until = NULL
             WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL
            ",
        )
        .bind(event_id)
        .bind(available_at)
        .bind(error)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(())
    }

    async fn dead_letter(
        &self,
        event_id: Uuid,
        error: &str,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            r"
            UPDATE domain_events
               SET dead_lettered_at = $2,
                   attempts = attempts + 1,
                   last_error = $3,
                   claimed_by = NULL,
                   claimed_until = NULL
             WHERE id = $1 AND delivered_at IS NULL AND dead_lettered_at IS NULL
            ",
        )
        .bind(event_id)
        .bind(now)
        .bind(error)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(())
    }
}
