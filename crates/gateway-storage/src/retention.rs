//! Retention deletes. Every statement names the rows that are still needed
//! and leaves them; see `gateway_application::retention` for the rules.
//!
//! No row lock is taken: the retention worker runs under a component lease,
//! and a row that another transaction starts referencing between the select
//! and the delete is protected by its foreign key, which fails the batch
//! instead of deleting evidence.

use async_trait::async_trait;
use gateway_application::{RepositoryError, RetentionRepository};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, unavailable};

async fn record(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    deleted: u64,
    older_than: OffsetDateTime,
    detail: serde_json::Value,
) -> Result<(), RepositoryError> {
    if deleted == 0 {
        return Ok(());
    }
    let mut payload = json!({ "deleted": deleted, "older_than": older_than });
    if let (serde_json::Value::Object(target), serde_json::Value::Object(extra)) =
        (&mut payload, detail)
    {
        target.extend(extra);
    }
    sqlx::query(
        r"INSERT INTO audit_events (id, merchant_id, actor_type, actor_id, action, resource_type,
              resource_id, reason, payload, created_at)
          VALUES ($1, NULL, 'system', NULL, 'retention.purge', $2, NULL, 'retention policy', $3,
                  now())",
    )
    .bind(Uuid::now_v7())
    .bind(table)
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(unavailable)?;
    Ok(())
}

#[async_trait]
impl RetentionRepository for PostgresRepository {
    async fn purge_webhook_deliveries(
        &self,
        older_than: OffsetDateTime,
        keep_latest: u32,
        limit: u32,
    ) -> Result<u64, RepositoryError> {
        let mut tx = self.pool().begin().await.map_err(unavailable)?;
        let deleted = sqlx::query(
            r"DELETE FROM webhook_deliveries
               WHERE id IN (
                   SELECT attempt.id
                     FROM webhook_deliveries AS attempt
                     JOIN domain_events AS event ON event.id = attempt.event_id
                    WHERE attempt.delivered_at < $1
                      AND event.delivered_at IS NOT NULL
                      AND event.delivered_at < $1
                      AND (SELECT count(*)
                             FROM webhook_deliveries AS newer
                            WHERE newer.event_id = attempt.event_id
                              AND newer.endpoint_id = attempt.endpoint_id
                              AND newer.attempt > attempt.attempt) >= $2
                    ORDER BY attempt.delivered_at, attempt.id
                    LIMIT $3)",
        )
        .bind(older_than)
        .bind(i64::from(keep_latest))
        .bind(i64::from(limit))
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?
        .rows_affected();
        record(
            &mut tx,
            "webhook_deliveries",
            deleted,
            older_than,
            json!({ "kept_latest_per_endpoint": keep_latest }),
        )
        .await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(deleted)
    }

    async fn purge_observations(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<u64, RepositoryError> {
        let mut tx = self.pool().begin().await.map_err(unavailable)?;
        let deleted = sqlx::query(
            r"DELETE FROM chain_observations
               WHERE id IN (
                   SELECT observation.id
                     FROM chain_observations AS observation
                    WHERE observation.observed_at < $1
                      AND NOT EXISTS (
                          SELECT 1 FROM chain_transfer_attestations AS attestation
                           WHERE attestation.observation_id = observation.id)
                      AND NOT EXISTS (
                          SELECT 1 FROM chain_observation_conflict_items AS item
                           WHERE item.observation_id = observation.id)
                      AND NOT EXISTS (
                          SELECT 1 FROM chain_observation_conflicts AS conflict
                           WHERE conflict.chain = observation.chain
                             AND conflict.network = observation.network
                             AND conflict.chain_environment = observation.chain_environment
                             AND conflict.tx_hash = observation.tx_hash
                             AND conflict.event_index = observation.event_index
                             AND conflict.resolved_at IS NULL)
                      AND EXISTS (
                          SELECT 1
                            FROM chain_transfers AS transfer
                            JOIN chain_transfer_state_current AS state
                              ON state.transfer_id = transfer.id
                           WHERE transfer.chain = observation.chain
                             AND transfer.network = observation.network
                             AND transfer.chain_environment = observation.chain_environment
                             AND transfer.tx_hash = observation.tx_hash
                             AND transfer.event_index = observation.event_index
                             AND state.state IN ('finalized', 'invalidated'))
                    ORDER BY observation.observed_at, observation.id
                    LIMIT $2)",
        )
        .bind(older_than)
        .bind(i64::from(limit))
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?
        .rows_affected();
        record(
            &mut tx,
            "chain_observations",
            deleted,
            older_than,
            json!({}),
        )
        .await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(deleted)
    }

    async fn purge_health_events(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<u64, RepositoryError> {
        let mut tx = self.pool().begin().await.map_err(unavailable)?;
        let deleted = sqlx::query(
            r"DELETE FROM component_health_events
               WHERE id IN (
                   SELECT event.id
                     FROM component_health_events AS event
                    WHERE event.created_at < $1
                      AND EXISTS (
                          SELECT 1 FROM component_health_events AS newer
                           WHERE newer.component = event.component
                             AND (newer.created_at, newer.id) > (event.created_at, event.id))
                    ORDER BY event.created_at, event.id
                    LIMIT $2)",
        )
        .bind(older_than)
        .bind(i64::from(limit))
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?
        .rows_affected();
        record(
            &mut tx,
            "component_health_events",
            deleted,
            older_than,
            json!({}),
        )
        .await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests;
