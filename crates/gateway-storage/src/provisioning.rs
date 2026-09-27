//! Storage for merchant onboarding: every write lands together with its audit
//! row, and every "not found" or "already done" is reported, never swallowed.

use async_trait::async_trait;
use gateway_application::{
    CollectorPolicy, EndpointState, MerchantRecord, NewCollector, ProvisioningError,
    ProvisioningRepository,
};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, unavailable};

const UNIQUE_VIOLATION: &str = "23505";

fn storage(error: sqlx::Error) -> ProvisioningError {
    ProvisioningError::Repository(unavailable(error))
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some(UNIQUE_VIOLATION))
}

/// One audit row, written in the caller's transaction so the change and its
/// record commit or roll back together.
#[allow(clippy::too_many_arguments)]
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    merchant_id: Option<Uuid>,
    actor: &str,
    action: &str,
    resource_type: &str,
    resource_id: Uuid,
    reason: Option<&str>,
    detail: Value,
) -> Result<(), ProvisioningError> {
    let mut payload = json!({ "actor": actor });
    if let (Value::Object(target), Value::Object(extra)) = (&mut payload, detail) {
        target.extend(extra);
    }
    sqlx::query(
        r"INSERT INTO audit_events (id, merchant_id, actor_type, actor_id, action, resource_type,
              resource_id, reason, payload, created_at)
          VALUES ($1, $2, 'operator', NULL, $3, $4, $5, $6, $7, now())",
    )
    .bind(Uuid::now_v7())
    .bind(merchant_id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(reason)
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn active_merchant(
    tx: &mut Transaction<'_, Postgres>,
    merchant_id: Uuid,
) -> Result<CollectorPolicy, ProvisioningError> {
    let policy = sqlx::query_scalar::<_, String>(
        "SELECT collector_policy FROM merchants WHERE id = $1 AND status = 'active' FOR SHARE",
    )
    .bind(merchant_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or(ProvisioningError::MerchantNotFound)?;
    policy.parse()
}

#[async_trait]
impl ProvisioningRepository for PostgresRepository {
    async fn create_merchant(
        &self,
        actor: &str,
        id: Uuid,
        external_id: &str,
        display_name: &str,
        policy: CollectorPolicy,
    ) -> Result<MerchantRecord, ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            r"INSERT INTO merchants (id, external_id, display_name, status, collector_policy)
              VALUES ($1, $2, $3, 'active', $4)
              ON CONFLICT (external_id) DO NOTHING
              RETURNING id",
        )
        .bind(id)
        .bind(external_id)
        .bind(display_name)
        .bind(policy.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let record = if let Some(id) = inserted {
            audit(
                &mut tx,
                Some(id),
                actor,
                "merchant.create",
                "merchant",
                id,
                None,
                json!({ "external_id": external_id, "collector_policy": policy.as_str() }),
            )
            .await?;
            MerchantRecord {
                id,
                external_id: external_id.to_owned(),
                display_name: display_name.to_owned(),
                collector_policy: policy,
                created: true,
            }
        } else {
            // A retry of the same request answers with the merchant it made;
            // a different request under the same external id is a conflict.
            let (existing, name, stored_policy) = sqlx::query_as::<_, (Uuid, String, String)>(
                "SELECT id, display_name, collector_policy FROM merchants WHERE external_id = $1",
            )
            .bind(external_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
            if name != display_name || stored_policy != policy.as_str() {
                return Err(ProvisioningError::MerchantConflict);
            }
            MerchantRecord {
                id: existing,
                external_id: external_id.to_owned(),
                display_name: name,
                collector_policy: policy,
                created: false,
            }
        };
        tx.commit().await.map_err(storage)?;
        Ok(record)
    }

    async fn insert_api_key(
        &self,
        actor: &str,
        merchant_id: Uuid,
        key_id: Uuid,
        prefix: &str,
        secret_hash: &[u8; 32],
        label: &str,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        active_merchant(&mut tx, merchant_id).await?;
        sqlx::query(
            r"INSERT INTO merchant_api_keys (id, merchant_id, key_prefix, secret_hash, label)
              VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(key_id)
        .bind(merchant_id)
        .bind(prefix)
        .bind(secret_hash.as_slice())
        .bind(label)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        audit(
            &mut tx,
            Some(merchant_id),
            actor,
            "api_key.issue",
            "merchant_api_key",
            key_id,
            None,
            json!({ "prefix": prefix, "label": label }),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn revoke_api_key(
        &self,
        actor: &str,
        key_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        let merchant_id = sqlx::query_scalar::<_, Uuid>(
            r"UPDATE merchant_api_keys SET revoked_at = now()
               WHERE id = $1 AND revoked_at IS NULL
              RETURNING merchant_id",
        )
        .bind(key_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(ProvisioningError::KeyNotFound)?;
        audit(
            &mut tx,
            Some(merchant_id),
            actor,
            "api_key.revoke",
            "merchant_api_key",
            key_id,
            Some(reason),
            json!({}),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn insert_webhook_endpoint(
        &self,
        actor: &str,
        merchant_id: Uuid,
        endpoint_id: Uuid,
        url: &str,
        description: Option<&str>,
        fingerprint: &[u8; 32],
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        active_merchant(&mut tx, merchant_id).await?;
        sqlx::query(
            r"INSERT INTO webhook_endpoints (id, merchant_id, url, secret_version,
                  secret_fingerprint, description, status, created_at)
              VALUES ($1, $2, $3, 1, $4, $5, 'active', now())",
        )
        .bind(endpoint_id)
        .bind(merchant_id)
        .bind(url)
        .bind(fingerprint.as_slice())
        .bind(description)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if is_unique_violation(&error) {
                ProvisioningError::Invalid("this URL is already registered for the merchant")
            } else {
                storage(error)
            }
        })?;
        audit(
            &mut tx,
            Some(merchant_id),
            actor,
            "webhook_endpoint.create",
            "webhook_endpoint",
            endpoint_id,
            None,
            json!({ "url": url }),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn endpoint_state(
        &self,
        endpoint_id: Uuid,
    ) -> Result<Option<EndpointState>, ProvisioningError> {
        let row = sqlx::query_as::<_, (Uuid, i32, String)>(
            "SELECT merchant_id, secret_version, status FROM webhook_endpoints WHERE id = $1",
        )
        .bind(endpoint_id)
        .fetch_optional(self.pool())
        .await
        .map_err(storage)?;
        Ok(
            row.map(|(merchant_id, secret_version, status)| EndpointState {
                merchant_id,
                secret_version,
                active: status == "active",
            }),
        )
    }

    async fn rotate_webhook_secret(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        from_version: i32,
        previous_fingerprint: &[u8; 32],
        new_version: i32,
        new_fingerprint: &[u8; 32],
        previous_valid_until: OffsetDateTime,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        // Guarded by the version the caller derived from: a concurrent
        // rotation has moved it, and this one must not overwrite that.
        let merchant_id = sqlx::query_scalar::<_, Uuid>(
            r"UPDATE webhook_endpoints
                 SET secret_version = $3,
                     secret_fingerprint = $4,
                     previous_secret_version = $2,
                     previous_secret_fingerprint = $5,
                     previous_valid_until = $6
               WHERE id = $1 AND secret_version = $2 AND status = 'active'
                 AND secret_fingerprint = $5
              RETURNING merchant_id",
        )
        .bind(endpoint_id)
        .bind(from_version)
        .bind(new_version)
        .bind(new_fingerprint.as_slice())
        .bind(previous_fingerprint.as_slice())
        .bind(previous_valid_until)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(ProvisioningError::EndpointNotFound)?;
        audit(
            &mut tx,
            Some(merchant_id),
            actor,
            "webhook_endpoint.rotate_secret",
            "webhook_endpoint",
            endpoint_id,
            Some(reason),
            json!({
                "from_version": from_version,
                "to_version": new_version,
                "previous_valid_until": previous_valid_until,
            }),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn disable_webhook_endpoint(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        let merchant_id = sqlx::query_scalar::<_, Uuid>(
            r"UPDATE webhook_endpoints SET status = 'disabled', disabled_at = now()
               WHERE id = $1 AND status = 'active'
              RETURNING merchant_id",
        )
        .bind(endpoint_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(ProvisioningError::EndpointNotFound)?;
        audit(
            &mut tx,
            Some(merchant_id),
            actor,
            "webhook_endpoint.disable",
            "webhook_endpoint",
            endpoint_id,
            Some(reason),
            json!({}),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn enqueue_test_event(
        &self,
        actor: &str,
        endpoint_id: Uuid,
        event_id: Uuid,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        let merchant_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT merchant_id FROM webhook_endpoints WHERE id = $1 AND status = 'active'",
        )
        .bind(endpoint_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(ProvisioningError::EndpointNotFound)?;
        sqlx::query(
            r"INSERT INTO domain_events (id, merchant_id, channel, event_type, aggregate_type,
                  aggregate_id, payload, available_at, created_at)
              VALUES ($1, $2, 'webhook', 'webhook.test', 'webhook_endpoint', $3, $4, now(), now())",
        )
        .bind(event_id)
        .bind(merchant_id)
        .bind(endpoint_id)
        .bind(json!({
            "endpoint_id": endpoint_id,
            "note": "A test event. It confirms delivery and signature verification; it is not a payment.",
        }))
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        audit(
            &mut tx,
            Some(merchant_id),
            actor,
            "webhook_endpoint.test",
            "webhook_endpoint",
            endpoint_id,
            None,
            json!({ "event_id": event_id }),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn register_collector(
        &self,
        actor: &str,
        collector: &NewCollector,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        if let Some(merchant_id) = collector.merchant_id {
            // An address registered to a merchant receives that merchant's
            // money only; a merchant on shared collectors is never given one.
            if active_merchant(&mut tx, merchant_id).await? != CollectorPolicy::Own {
                return Err(ProvisioningError::PolicyMismatch);
            }
        }
        let asset_active = sqlx::query_scalar::<_, bool>(
            "SELECT status = 'active' FROM chain_assets WHERE id = $1",
        )
        .bind(collector.asset_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .unwrap_or(false);
        if !asset_active {
            return Err(ProvisioningError::Invalid(
                "the asset does not exist or is not active",
            ));
        }
        sqlx::query(
            r"INSERT INTO collector_addresses (id, asset_id, address_key, address_text, state,
                  valid_from, pinned_sha256, approved_by, merchant_id)
              VALUES ($1, $2, $3, $4, 'active', now(), encode(sha256($3), 'hex'), $5, $6)",
        )
        .bind(collector.id)
        .bind(collector.asset_id)
        .bind(collector.address.as_bytes())
        .bind(&collector.address_text)
        .bind(actor)
        .bind(collector.merchant_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if is_unique_violation(&error) {
                ProvisioningError::AddressTaken
            } else {
                storage(error)
            }
        })?;
        audit(
            &mut tx,
            collector.merchant_id,
            actor,
            "collector.register",
            "collector_address",
            collector.id,
            None,
            json!({
                "address": collector.address_text,
                "asset_id": collector.asset_id,
                "ownership_evidence": collector.ownership_evidence,
            }),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn retire_collector(
        &self,
        actor: &str,
        collector_id: Uuid,
        reason: &str,
    ) -> Result<(), ProvisioningError> {
        let mut tx = self.pool().begin().await.map_err(storage)?;
        let merchant_id = sqlx::query_scalar::<_, Option<Uuid>>(
            r"UPDATE collector_addresses SET state = 'retired', retired_at = now()
               WHERE id = $1 AND state <> 'retired'
              RETURNING merchant_id",
        )
        .bind(collector_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(ProvisioningError::CollectorNotFound)?;
        audit(
            &mut tx,
            merchant_id,
            actor,
            "collector.retire",
            "collector_address",
            collector_id,
            Some(reason),
            json!({}),
        )
        .await?;
        tx.commit().await.map_err(storage)
    }
}

#[cfg(test)]
mod tests;
