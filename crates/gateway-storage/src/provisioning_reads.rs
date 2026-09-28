//! Reads for the admin command line. Every query names its columns: the
//! provisioner role may not read a key hash, and a list must not ask for one.

use async_trait::async_trait;
use gateway_application::{
    ApiKeySummary, CollectorSummary, ListRequest, MerchantSummary, ProvisioningError,
    ProvisioningReadRepository, WebhookEndpointSummary,
};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, unavailable};

fn storage(error: sqlx::Error) -> ProvisioningError {
    ProvisioningError::Repository(unavailable(error))
}

/// One row past the page, so the page knows whether another exists.
fn fetch_limit(page: ListRequest) -> i64 {
    i64::from(page.limit) + 1
}

#[derive(Debug, FromRow)]
struct MerchantRow {
    id: Uuid,
    external_id: String,
    display_name: String,
    status: String,
    collector_policy: String,
    created_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct ApiKeyRow {
    id: Uuid,
    merchant_id: Uuid,
    key_prefix: String,
    label: String,
    created_at: OffsetDateTime,
    last_used_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug, FromRow)]
struct EndpointRow {
    id: Uuid,
    merchant_id: Uuid,
    url: String,
    description: Option<String>,
    status: String,
    secret_version: i32,
    previous_valid_until: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    disabled_at: Option<OffsetDateTime>,
}

#[derive(Debug, FromRow)]
struct CollectorRow {
    id: Uuid,
    asset_id: Uuid,
    merchant_id: Option<Uuid>,
    address_text: String,
    state: String,
    open_reservations: i64,
    valid_from: OffsetDateTime,
    retired_at: Option<OffsetDateTime>,
}

#[async_trait]
impl ProvisioningReadRepository for PostgresRepository {
    async fn list_merchants(
        &self,
        page: ListRequest,
    ) -> Result<Vec<MerchantSummary>, ProvisioningError> {
        let rows = sqlx::query_as::<_, MerchantRow>(
            r"SELECT id, external_id, display_name, status, collector_policy, created_at
                FROM merchants
               WHERE $1::UUID IS NULL OR id > $1
               ORDER BY id
               LIMIT $2",
        )
        .bind(page.after)
        .bind(fetch_limit(page))
        .fetch_all(self.pool())
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(|row| MerchantSummary {
                id: row.id,
                external_id: row.external_id,
                display_name: row.display_name,
                status: row.status,
                collector_policy: row.collector_policy,
                created_at: row.created_at,
            })
            .collect())
    }

    async fn list_api_keys(
        &self,
        merchant_id: Uuid,
        page: ListRequest,
    ) -> Result<Vec<ApiKeySummary>, ProvisioningError> {
        let rows = sqlx::query_as::<_, ApiKeyRow>(
            r"SELECT id, merchant_id, key_prefix, label, created_at, last_used_at, revoked_at
                FROM merchant_api_keys
               WHERE merchant_id = $1 AND ($2::UUID IS NULL OR id > $2)
               ORDER BY id
               LIMIT $3",
        )
        .bind(merchant_id)
        .bind(page.after)
        .bind(fetch_limit(page))
        .fetch_all(self.pool())
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(|row| ApiKeySummary {
                id: row.id,
                merchant_id: row.merchant_id,
                prefix: row.key_prefix,
                label: row.label,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
                revoked_at: row.revoked_at,
            })
            .collect())
    }

    async fn list_webhook_endpoints(
        &self,
        merchant_id: Uuid,
        page: ListRequest,
    ) -> Result<Vec<WebhookEndpointSummary>, ProvisioningError> {
        let rows = sqlx::query_as::<_, EndpointRow>(
            r"SELECT id, merchant_id, url, description, status, secret_version,
                     previous_valid_until, created_at, disabled_at
                FROM webhook_endpoints
               WHERE merchant_id = $1 AND ($2::UUID IS NULL OR id > $2)
               ORDER BY id
               LIMIT $3",
        )
        .bind(merchant_id)
        .bind(page.after)
        .bind(fetch_limit(page))
        .fetch_all(self.pool())
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(|row| WebhookEndpointSummary {
                id: row.id,
                merchant_id: row.merchant_id,
                url: row.url,
                description: row.description,
                status: row.status,
                secret_version: row.secret_version,
                previous_secret_valid_until: row.previous_valid_until,
                created_at: row.created_at,
                disabled_at: row.disabled_at,
            })
            .collect())
    }

    async fn list_collectors(
        &self,
        merchant_id: Option<Uuid>,
        page: ListRequest,
    ) -> Result<Vec<CollectorSummary>, ProvisioningError> {
        let rows = sqlx::query_as::<_, CollectorRow>(
            r"SELECT collector.id, collector.asset_id, collector.merchant_id,
                     collector.address_text, collector.state,
                     (SELECT count(*) FROM amount_leases AS lease
                       WHERE lease.collector_address_id = collector.id) AS open_reservations,
                     collector.valid_from, collector.retired_at
                FROM collector_addresses AS collector
               WHERE ($1::UUID IS NULL OR collector.merchant_id = $1)
                 AND ($2::UUID IS NULL OR collector.id > $2)
               ORDER BY collector.id
               LIMIT $3",
        )
        .bind(merchant_id)
        .bind(page.after)
        .bind(fetch_limit(page))
        .fetch_all(self.pool())
        .await
        .map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(|row| CollectorSummary {
                id: row.id,
                asset_id: row.asset_id,
                merchant_id: row.merchant_id,
                address: row.address_text,
                state: row.state,
                open_reservations: row.open_reservations,
                valid_from: row.valid_from,
                retired_at: row.retired_at,
            })
            .collect())
    }
}
