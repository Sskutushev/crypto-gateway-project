//! Storage for webhook redelivery: the event row is re-queued in place, and
//! the request and its audit row land in the same transaction.

use async_trait::async_trait;
use gateway_application::{
    RedeliveryActor, RedeliveryError, RedeliveryRepository, RedeliveryResult, WebhookRedelivery,
};
use serde_json::json;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, unavailable};

fn storage(error: sqlx::Error) -> RedeliveryError {
    RedeliveryError::Repository(unavailable(error))
}

#[derive(Debug, FromRow)]
struct StoredRedelivery {
    id: Uuid,
    event_id: Uuid,
    merchant_id: Uuid,
    endpoint_id: Option<Uuid>,
    previous_state: String,
    previous_attempts: i32,
    request_hash: Vec<u8>,
    created_at: OffsetDateTime,
}

impl StoredRedelivery {
    fn into_result(self, replayed: bool) -> RedeliveryResult {
        RedeliveryResult {
            id: self.id,
            event_id: self.event_id,
            merchant_id: self.merchant_id,
            endpoint_id: self.endpoint_id,
            previous_state: self.previous_state,
            previous_attempts: self.previous_attempts,
            requested_at: self.created_at,
            replayed,
        }
    }
}

#[derive(Debug, FromRow)]
struct EventRow {
    merchant_id: Option<Uuid>,
    channel: String,
    attempts: i32,
    delivered: bool,
    dead_lettered: bool,
}

#[async_trait]
impl RedeliveryRepository for PostgresRepository {
    // One transaction, read top to bottom: replay, lock, refuse, write.
    #[allow(clippy::too_many_lines)]
    async fn redeliver_webhook_event(
        &self,
        actor: &RedeliveryActor,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        request: &WebhookRedelivery,
        now: OffsetDateTime,
    ) -> Result<RedeliveryResult, RedeliveryError> {
        let principal = actor.principal();
        let mut tx = self.begin().await.map_err(storage)?;
        // Two identical requests racing each other must see one result: the
        // second waits here and then finds the first one's row.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("redeliver:{principal}:{idempotency_key}"))
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let stored = sqlx::query_as::<_, StoredRedelivery>(
            r"SELECT id, event_id, merchant_id, endpoint_id, previous_state, previous_attempts,
                     request_hash, created_at
                FROM webhook_redeliveries
               WHERE principal = $1 AND idempotency_key = $2",
        )
        .bind(&principal)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        if let Some(stored) = stored {
            if stored.request_hash.as_slice() != request_hash {
                return Err(RedeliveryError::IdempotencyConflict);
            }
            tx.commit().await.map_err(storage)?;
            return Ok(stored.into_result(true));
        }

        let event = sqlx::query_as::<_, EventRow>(
            r"SELECT merchant_id, channel, attempts,
                     delivered_at IS NOT NULL AS delivered,
                     dead_lettered_at IS NOT NULL AS dead_lettered
                FROM domain_events
               WHERE id = $1
               FOR UPDATE",
        )
        .bind(request.event_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(RedeliveryError::EventNotFound)?;
        if event.channel != "webhook" {
            return Err(RedeliveryError::NotAWebhook);
        }
        let merchant_id = event.merchant_id.ok_or(RedeliveryError::NotAWebhook)?;
        let previous_state = match (event.delivered, event.dead_lettered) {
            (true, _) => "delivered",
            (false, true) => "dead_lettered",
            (false, false) => return Err(RedeliveryError::StillQueued),
        };
        // An endpoint of another merchant, a disabled one, or a merchant with
        // no endpoint at all is refused here rather than re-queued into a
        // dead letter.
        let reachable: bool = sqlx::query_scalar(
            r"SELECT EXISTS (
                  SELECT 1 FROM webhook_endpoints
                   WHERE merchant_id = $1 AND status = 'active'
                     AND ($2::UUID IS NULL OR id = $2))",
        )
        .bind(merchant_id)
        .bind(request.endpoint_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        if !reachable {
            return Err(RedeliveryError::EndpointNotFound);
        }

        sqlx::query(
            r"UPDATE domain_events
                 SET delivered_at = NULL,
                     dead_lettered_at = NULL,
                     available_at = $2,
                     last_error = NULL,
                     claimed_by = NULL,
                     claimed_until = NULL,
                     attempt_floor = attempts,
                     target_endpoint_id = $3
               WHERE id = $1",
        )
        .bind(request.event_id)
        .bind(now)
        .bind(request.endpoint_id)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let id = Uuid::now_v7();
        let stored = sqlx::query_as::<_, StoredRedelivery>(
            r"INSERT INTO webhook_redeliveries (
                  id, event_id, merchant_id, endpoint_id, principal, operator_key_id, actor_label,
                  idempotency_key, request_hash, reason, previous_state, previous_attempts,
                  created_at)
              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
              RETURNING id, event_id, merchant_id, endpoint_id, previous_state,
                        previous_attempts, request_hash, created_at",
        )
        .bind(id)
        .bind(request.event_id)
        .bind(merchant_id)
        .bind(request.endpoint_id)
        .bind(&principal)
        .bind(actor.operator_key_id())
        .bind(actor.label())
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .bind(request.reason.trim())
        .bind(previous_state)
        .bind(event.attempts)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        sqlx::query(
            r"INSERT INTO audit_events (id, merchant_id, actor_type, actor_id, action,
                  resource_type, resource_id, reason, payload, created_at)
              VALUES ($1, $2, 'operator', $3, 'webhook_event.redeliver', 'domain_event', $4,
                      $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(merchant_id)
        .bind(actor.operator_key_id())
        .bind(request.event_id)
        .bind(request.reason.trim())
        .bind(json!({
            "actor": actor.label(),
            "principal": principal,
            "redelivery_id": id,
            "endpoint_id": request.endpoint_id,
            "previous_state": previous_state,
            "previous_attempts": event.attempts,
        }))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(stored.into_result(false))
    }
}

#[cfg(test)]
mod tests;
