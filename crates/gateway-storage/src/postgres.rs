use std::str::FromStr;

use async_trait::async_trait;
use gateway_application::{
    ApiCredential, ExpiryResult, IdempotentCreate, IdempotentQuote, PaymentIntentRepository,
    QuoteContext, QuoteRepository, RepositoryError,
};
use gateway_domain::{
    CurrencyCode, FiatAmount, IssuedQuote, MoneyError, PaymentIntent, PaymentIntentStatus,
    PriceSnapshot, QuotePlan, QuotePolicySnapshot, RailHealth, RailHealthSnapshot, RawAmount,
};
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
    pub(crate) const fn pool(&self) -> &PgPool {
        &self.pool
    }

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

#[derive(Debug, FromRow)]
struct IssuedQuoteRow {
    id: Uuid,
    attempt_id: Uuid,
    payment_intent_id: Uuid,
    asset_id: Uuid,
    collector_address_id: Uuid,
    collector_address: String,
    price_snapshot_id: Uuid,
    quote_policy_id: Uuid,
    rail_health_snapshot_id: Uuid,
    fiat_currency: String,
    fiat_amount_minor: i64,
    amount_raw: String,
    rate_numerator: String,
    rate_denominator: String,
    price_sources: Value,
    price_observed_at: OffsetDateTime,
    policy_version: String,
    rail_health_observed_at: OffsetDateTime,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    late_payment_until: OffsetDateTime,
}

impl TryFrom<IssuedQuoteRow> for IssuedQuote {
    type Error = RepositoryError;

    fn try_from(row: IssuedQuoteRow) -> Result<Self, Self::Error> {
        let currency = CurrencyCode::new(row.fiat_currency).map_err(corrupt_money)?;
        let fiat_amount =
            FiatAmount::positive(currency, row.fiat_amount_minor).map_err(corrupt_money)?;
        let amount_raw = RawAmount::from_str(&row.amount_raw).map_err(corrupt_money)?;
        let rate_numerator = RawAmount::from_str(&row.rate_numerator).map_err(corrupt_money)?;
        let rate_denominator = RawAmount::from_str(&row.rate_denominator).map_err(corrupt_money)?;
        if !row.price_sources.is_array() {
            return Err(RepositoryError::CorruptData(
                "quote price sources are not an array".to_owned(),
            ));
        }

        Ok(Self {
            id: row.id,
            attempt_id: row.attempt_id,
            payment_intent_id: row.payment_intent_id,
            asset_id: row.asset_id,
            collector_address_id: row.collector_address_id,
            collector_address: row.collector_address,
            price_snapshot_id: row.price_snapshot_id,
            quote_policy_id: row.quote_policy_id,
            rail_health_snapshot_id: row.rail_health_snapshot_id,
            fiat_amount,
            amount_raw,
            rate_numerator,
            rate_denominator,
            price_sources: row.price_sources,
            price_observed_at: row.price_observed_at,
            policy_version: row.policy_version,
            rail_health_observed_at: row.rail_health_observed_at,
            created_at: row.created_at,
            expires_at: row.expires_at,
            late_payment_until: row.late_payment_until,
        })
    }
}

#[derive(Debug, FromRow)]
struct QuoteContextRow {
    collector_address_id: Uuid,
    collector_address: String,
    price_snapshot_id: Option<Uuid>,
    rate_numerator: Option<String>,
    rate_denominator: Option<String>,
    price_sources: Option<Value>,
    price_observed_at: Option<OffsetDateTime>,
    quote_policy_id: Option<Uuid>,
    policy_version: Option<String>,
    quote_ttl_seconds: Option<i64>,
    late_payment_window_seconds: Option<i64>,
    amount_slot_count: Option<i32>,
    max_price_age_seconds: Option<i64>,
    max_policy_age_seconds: Option<i64>,
    max_rail_health_age_seconds: Option<i64>,
    policy_observed_at: Option<OffsetDateTime>,
    rail_health_snapshot_id: Option<Uuid>,
    rail_health: Option<String>,
    rail_health_observed_at: Option<OffsetDateTime>,
}

impl TryFrom<QuoteContextRow> for QuoteContext {
    type Error = RepositoryError;

    fn try_from(row: QuoteContextRow) -> Result<Self, Self::Error> {
        let price = row
            .price_snapshot_id
            .map(|id| {
                Ok(PriceSnapshot {
                    id,
                    rate_numerator: RawAmount::from_str(&required(
                        row.rate_numerator,
                        "price rate numerator",
                    )?)
                    .map_err(corrupt_money)?,
                    rate_denominator: RawAmount::from_str(&required(
                        row.rate_denominator,
                        "price rate denominator",
                    )?)
                    .map_err(corrupt_money)?,
                    sources: required(row.price_sources, "price sources")?,
                    observed_at: required(row.price_observed_at, "price observed_at")?,
                })
            })
            .transpose()?;
        let policy = row
            .quote_policy_id
            .map(|id| {
                Ok(QuotePolicySnapshot {
                    id,
                    version: required(row.policy_version, "policy version")?,
                    quote_ttl_seconds: required(row.quote_ttl_seconds, "policy quote ttl")?,
                    late_payment_window_seconds: required(
                        row.late_payment_window_seconds,
                        "policy late payment window",
                    )?,
                    amount_slot_count: u32::try_from(required(
                        row.amount_slot_count,
                        "policy amount slot count",
                    )?)
                    .map_err(|_| corrupt("policy amount slot count is negative"))?,
                    max_price_age_seconds: required(
                        row.max_price_age_seconds,
                        "policy max price age",
                    )?,
                    max_policy_age_seconds: required(
                        row.max_policy_age_seconds,
                        "policy max policy age",
                    )?,
                    max_rail_health_age_seconds: required(
                        row.max_rail_health_age_seconds,
                        "policy max rail health age",
                    )?,
                    observed_at: required(row.policy_observed_at, "policy observed_at")?,
                })
            })
            .transpose()?;
        let rail_health = row
            .rail_health_snapshot_id
            .map(|id| {
                let health = match required(row.rail_health, "rail health")?.as_str() {
                    "healthy" => RailHealth::Healthy,
                    "degraded" => RailHealth::Degraded,
                    "unavailable" => RailHealth::Unavailable,
                    value => return Err(corrupt(format!("unknown rail health: {value}"))),
                };
                Ok(RailHealthSnapshot {
                    id,
                    health,
                    observed_at: required(row.rail_health_observed_at, "rail health observed_at")?,
                })
            })
            .transpose()?;

        Ok(Self {
            collector_address_id: row.collector_address_id,
            collector_address: row.collector_address,
            price,
            policy,
            rail_health,
        })
    }
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

#[async_trait]
impl QuoteRepository for PostgresRepository {
    async fn find_quote_replay(
        &self,
        merchant_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<Option<IssuedQuote>, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(unavailable)?;
        let existing = sqlx::query_as::<_, (Vec<u8>, Uuid)>(
            r"
            SELECT request_hash, resource_id
              FROM api_idempotency_records
             WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3
            ",
        )
        .bind(merchant_id)
        .bind(route)
        .bind(idempotency_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?;
        let Some((existing_hash, quote_id)) = existing else {
            transaction.commit().await.map_err(unavailable)?;
            return Ok(None);
        };
        if existing_hash.as_slice() != request_hash {
            return Err(RepositoryError::IdempotencyConflict);
        }
        let quote = find_quote(&mut transaction, merchant_id, quote_id)
            .await?
            .ok_or_else(|| {
                RepositoryError::CorruptData(
                    "idempotency record references a missing quote".to_owned(),
                )
            })?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(Some(quote))
    }

    async fn load_quote_context(
        &self,
        asset_id: Uuid,
        currency: &CurrencyCode,
    ) -> Result<QuoteContext, RepositoryError> {
        let row = sqlx::query_as::<_, QuoteContextRow>(
            r"
            SELECT collector.id AS collector_address_id,
                   collector.address_text AS collector_address,
                   price.id AS price_snapshot_id,
                   price.rate_numerator::TEXT AS rate_numerator,
                   price.rate_denominator::TEXT AS rate_denominator,
                   price.sources AS price_sources,
                   price.observed_at AS price_observed_at,
                   policy.id AS quote_policy_id,
                   policy.version AS policy_version,
                   policy.quote_ttl_seconds,
                   policy.late_payment_window_seconds,
                   policy.amount_slot_count,
                   policy.max_price_age_seconds,
                   policy.max_policy_age_seconds,
                   policy.max_rail_health_age_seconds,
                   policy.observed_at AS policy_observed_at,
                   rail.id AS rail_health_snapshot_id,
                   rail.health AS rail_health,
                   rail.observed_at AS rail_health_observed_at
              FROM collector_addresses AS collector
              JOIN chain_assets AS asset ON asset.id = collector.asset_id
              LEFT JOIN LATERAL (
                    SELECT snapshot.id, snapshot.rate_numerator,
                           snapshot.rate_denominator, snapshot.sources,
                           snapshot.observed_at
                      FROM price_snapshots AS snapshot
                     WHERE snapshot.asset_id = collector.asset_id
                       AND snapshot.fiat_currency = $2
                     ORDER BY snapshot.observed_at DESC, snapshot.id DESC
                     LIMIT 1
              ) AS price ON true
              LEFT JOIN LATERAL (
                    SELECT candidate.id, candidate.version,
                           candidate.quote_ttl_seconds,
                           candidate.late_payment_window_seconds,
                           candidate.amount_slot_count,
                           candidate.max_price_age_seconds,
                           candidate.max_policy_age_seconds,
                           candidate.max_rail_health_age_seconds,
                           candidate.observed_at
                      FROM quote_policies AS candidate
                     WHERE candidate.asset_id = collector.asset_id
                       AND candidate.fiat_currency = $2
                       AND candidate.status = 'active'
                     LIMIT 1
              ) AS policy ON true
              LEFT JOIN LATERAL (
                    SELECT snapshot.id, snapshot.health, snapshot.observed_at
                      FROM rail_health_snapshots AS snapshot
                     WHERE snapshot.asset_id = collector.asset_id
                     ORDER BY snapshot.observed_at DESC, snapshot.id DESC
                     LIMIT 1
              ) AS rail ON true
             WHERE collector.asset_id = $1
               AND collector.state = 'active'
               AND collector.valid_from <= now()
               AND asset.status = 'active'
             ORDER BY collector.valid_from, collector.id
             LIMIT 1
            ",
        )
        .bind(asset_id)
        .bind(currency.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?
        .ok_or(RepositoryError::CollectorUnavailable)?;
        row.try_into()
    }

    #[allow(clippy::too_many_lines)]
    async fn issue_quote_idempotently(
        &self,
        plan: QuotePlan,
        actor_key_id: Uuid,
        route: &str,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<IdempotentQuote, RepositoryError> {
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
        .bind(plan.merchant_id())
        .bind(route)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .bind(plan.quote_id())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?;

        if inserted.is_none() {
            let existing = sqlx::query_as::<_, (Vec<u8>, Uuid)>(
                r"
                SELECT request_hash, resource_id
                  FROM api_idempotency_records
                 WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3
                ",
            )
            .bind(plan.merchant_id())
            .bind(route)
            .bind(idempotency_key)
            .fetch_one(&mut *transaction)
            .await
            .map_err(unavailable)?;
            if existing.0.as_slice() != request_hash {
                return Err(RepositoryError::IdempotencyConflict);
            }
            let quote = find_quote(&mut transaction, plan.merchant_id(), existing.1)
                .await?
                .ok_or_else(|| {
                    RepositoryError::CorruptData(
                        "idempotency record references a missing quote".to_owned(),
                    )
                })?;
            transaction.commit().await.map_err(unavailable)?;
            return Ok(IdempotentQuote::Replayed(quote));
        }

        let intent = sqlx::query_as::<_, (String, i64, String)>(
            r"
            SELECT status, amount_minor, currency
              FROM payment_intents
             WHERE id = $1 AND merchant_id = $2
             FOR UPDATE
            ",
        )
        .bind(plan.payment_intent_id())
        .bind(plan.merchant_id())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?
        .ok_or(RepositoryError::PaymentIntentNotQuotable)?;
        if intent.0 != "requires_quote"
            || intent.1 != plan.fiat_amount().minor_units
            || intent.2 != plan.fiat_amount().currency.as_str()
        {
            return Err(RepositoryError::PaymentIntentNotQuotable);
        }

        let collector_exists = sqlx::query_scalar::<_, Uuid>(
            r"
            SELECT collector.id
              FROM collector_addresses AS collector
              JOIN chain_assets AS asset ON asset.id = collector.asset_id
              JOIN price_snapshots AS price ON price.id = $4
              JOIN quote_policies AS policy ON policy.id = $5
              JOIN rail_health_snapshots AS rail ON rail.id = $6
             WHERE collector.id = $1
               AND collector.asset_id = $2
               AND collector.state = 'active'
               AND collector.valid_from <= $3
               AND asset.status = 'active'
               AND price.asset_id = asset.id
               AND price.fiat_currency = $7
               AND price.rate_numerator = CAST($8 AS NUMERIC)
               AND price.rate_denominator = CAST($9 AS NUMERIC)
               AND price.sources = $10
               AND price.observed_at = $11
               AND policy.asset_id = asset.id
               AND policy.fiat_currency = $7
               AND policy.status = 'active'
               AND policy.version = $12
               AND policy.quote_ttl_seconds = $13
               AND policy.late_payment_window_seconds = $14
               AND policy.amount_slot_count = $15
               AND policy.max_price_age_seconds = $16
               AND policy.max_policy_age_seconds = $17
               AND policy.max_rail_health_age_seconds = $18
               AND policy.observed_at = $19
               AND rail.asset_id = asset.id
               AND rail.health = 'healthy'
               AND rail.observed_at = $20
               AND collector.address_text = $21
             FOR UPDATE OF collector
            ",
        )
        .bind(plan.collector_address_id())
        .bind(plan.asset_id())
        .bind(plan.created_at())
        .bind(plan.price_snapshot_id())
        .bind(plan.quote_policy_id())
        .bind(plan.rail_health_snapshot_id())
        .bind(plan.fiat_amount().currency.as_str())
        .bind(plan.rate_numerator().to_string())
        .bind(plan.rate_denominator().to_string())
        .bind(plan.price_sources())
        .bind(plan.price_observed_at())
        .bind(plan.policy_version())
        .bind(plan.expires_at().unix_timestamp() - plan.created_at().unix_timestamp())
        .bind(plan.late_payment_until().unix_timestamp() - plan.expires_at().unix_timestamp())
        .bind(i32::try_from(plan.amount_slot_count()).map_err(|_| {
            RepositoryError::CorruptData("amount slot count exceeds PostgreSQL integer".to_owned())
        })?)
        .bind(plan.max_price_age_seconds())
        .bind(plan.max_policy_age_seconds())
        .bind(plan.max_rail_health_age_seconds())
        .bind(plan.policy_observed_at())
        .bind(plan.rail_health_observed_at())
        .bind(plan.collector_address())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?;
        if collector_exists.is_none() {
            return Err(RepositoryError::CollectorUnavailable);
        }

        let slot = sqlx::query_scalar::<_, i32>(
            r"
            SELECT slot
              FROM generate_series(0, $3::integer - 1) AS slot
             WHERE NOT EXISTS (
                    SELECT 1
                      FROM amount_leases AS lease
                     WHERE lease.collector_address_id = $1
                       AND lease.amount_raw = CAST($2 AS NUMERIC) + slot
             )
             ORDER BY slot
             LIMIT 1
            ",
        )
        .bind(plan.collector_address_id())
        .bind(plan.base_amount_raw().to_string())
        .bind(i32::try_from(plan.amount_slot_count()).map_err(|_| {
            RepositoryError::CorruptData("amount slot count exceeds PostgreSQL integer".to_owned())
        })?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?
        .ok_or(RepositoryError::AmountSlotsExhausted)?;
        let slot = u32::try_from(slot)
            .map_err(|_| RepositoryError::CorruptData("negative amount slot".to_owned()))?;
        let amount_raw = plan
            .base_amount_raw()
            .checked_add_u32(slot)
            .map_err(|error| RepositoryError::CorruptData(error.to_string()))?;

        let lease_id = insert_quote_attempt_and_lease(&mut transaction, &plan, amount_raw).await?;
        let updated = sqlx::query(
            r"
            UPDATE payment_intents
               SET status = 'awaiting_payment', version = version + 1, updated_at = $3
             WHERE id = $1 AND merchant_id = $2 AND status = 'requires_quote'
            ",
        )
        .bind(plan.payment_intent_id())
        .bind(plan.merchant_id())
        .bind(plan.created_at())
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;
        if updated.rows_affected() != 1 {
            return Err(RepositoryError::PaymentIntentNotQuotable);
        }
        insert_quote_audit_events(&mut transaction, &plan, actor_key_id, lease_id, amount_raw)
            .await?;
        sqlx::query(
            r"
            UPDATE api_idempotency_records
               SET completed_at = now()
             WHERE merchant_id = $1 AND route = $2 AND idempotency_key = $3
            ",
        )
        .bind(plan.merchant_id())
        .bind(route)
        .bind(idempotency_key)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;

        let quote = find_quote(&mut transaction, plan.merchant_id(), plan.quote_id())
            .await?
            .ok_or_else(|| RepositoryError::CorruptData("issued quote is missing".to_owned()))?;
        transaction.commit().await.map_err(unavailable)?;
        Ok(IdempotentQuote::Issued(quote))
    }

    #[allow(clippy::too_many_lines)]
    async fn expire_quotes_and_archive_leases(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<ExpiryResult, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(unavailable)?;
        let limit = i64::from(limit);
        let due_attempts = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid)>(
            r"
            SELECT attempt.id, attempt.payment_intent_id, attempt.quote_id, attempt.merchant_id
              FROM payment_attempts AS attempt
             WHERE attempt.status = 'awaiting_payment'
               AND attempt.quote_expires_at <= $1
             ORDER BY attempt.quote_expires_at, attempt.id
             LIMIT $2
             FOR UPDATE OF attempt SKIP LOCKED
            ",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await
        .map_err(unavailable)?;

        for (attempt_id, intent_id, quote_id, merchant_id) in &due_attempts {
            let attempt_updated = sqlx::query(
                "UPDATE payment_attempts SET status = 'expired', updated_at = $2 WHERE id = $1",
            )
            .bind(attempt_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
            let intent_updated = sqlx::query(
                r"
                UPDATE payment_intents
                   SET status = 'expired', version = version + 1, updated_at = $2
                 WHERE id = $1 AND status = 'awaiting_payment'
                ",
            )
            .bind(intent_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
            if attempt_updated.rows_affected() != 1 || intent_updated.rows_affected() != 1 {
                return Err(RepositoryError::CorruptData(
                    "quote expiry did not advance both attempt and payment intent".to_owned(),
                ));
            }
            for (action, resource_type, resource_id, payload) in [
                (
                    "payment_quote.expired",
                    "payment_quote",
                    *quote_id,
                    serde_json::json!({"attempt_id": attempt_id, "expired_at": now}),
                ),
                (
                    "payment_attempt.expired",
                    "payment_attempt",
                    *attempt_id,
                    serde_json::json!({"quote_id": quote_id, "expired_at": now}),
                ),
                (
                    "payment_intent.expired",
                    "payment_intent",
                    *intent_id,
                    serde_json::json!({"quote_id": quote_id, "attempt_id": attempt_id, "expired_at": now}),
                ),
            ] {
                insert_system_audit(
                    &mut transaction,
                    *merchant_id,
                    action,
                    resource_type,
                    resource_id,
                    payload,
                )
                .await?;
            }
        }

        let due_leases = sqlx::query_as::<
            _,
            (
                Uuid,
                Uuid,
                String,
                Uuid,
                OffsetDateTime,
                OffsetDateTime,
                Uuid,
            ),
        >(
            r"
            SELECT lease.id, lease.collector_address_id, lease.amount_raw::TEXT,
                   lease.attempt_id, lease.leased_from, lease.lease_until, attempt.merchant_id
              FROM amount_leases AS lease
              JOIN payment_attempts AS attempt ON attempt.id = lease.attempt_id
             WHERE lease.lease_until <= $1
             ORDER BY lease.lease_until, lease.id
             LIMIT $2
             FOR UPDATE OF lease SKIP LOCKED
            ",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await
        .map_err(unavailable)?;

        for (
            lease_id,
            collector_id,
            amount_raw,
            attempt_id,
            leased_from,
            lease_until,
            merchant_id,
        ) in &due_leases
        {
            sqlx::query(
                r"
                INSERT INTO amount_lease_history (
                    id, lease_id, collector_address_id, amount_raw, attempt_id,
                    leased_from, leased_until, released_at, release_reason
                ) VALUES ($1, $2, $3, CAST($4 AS NUMERIC), $5, $6, $7, $8, 'expired')
                ",
            )
            .bind(Uuid::now_v7())
            .bind(lease_id)
            .bind(collector_id)
            .bind(amount_raw)
            .bind(attempt_id)
            .bind(leased_from)
            .bind(lease_until)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
            sqlx::query("DELETE FROM amount_leases WHERE id = $1")
                .bind(lease_id)
                .execute(&mut *transaction)
                .await
                .map_err(unavailable)?;
            insert_system_audit(
                &mut transaction,
                *merchant_id,
                "amount_lease.archived",
                "amount_lease",
                *lease_id,
                serde_json::json!({
                    "attempt_id": attempt_id,
                    "amount_raw": amount_raw,
                    "release_reason": "expired",
                    "released_at": now,
                }),
            )
            .await?;
        }

        let result = ExpiryResult {
            quotes_expired: u64::try_from(due_attempts.len()).map_err(|_| {
                RepositoryError::CorruptData("expiry result count overflow".to_owned())
            })?,
            leases_archived: u64::try_from(due_leases.len()).map_err(|_| {
                RepositoryError::CorruptData("archive result count overflow".to_owned())
            })?,
        };
        transaction.commit().await.map_err(unavailable)?;
        Ok(result)
    }
}

async fn insert_quote_attempt_and_lease(
    transaction: &mut Transaction<'_, Postgres>,
    plan: &QuotePlan,
    amount_raw: RawAmount,
) -> Result<Uuid, RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO payment_quotes (
            id, merchant_id, payment_intent_id, asset_id, collector_address_id,
            price_snapshot_id, quote_policy_id, rail_health_snapshot_id,
            fiat_currency, fiat_amount_minor, base_amount_raw, amount_raw,
            rate_numerator, rate_denominator, price_sources, price_observed_at,
            policy_version, rail_health_observed_at, created_at, expires_at,
            late_payment_until
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            CAST($11 AS NUMERIC), CAST($12 AS NUMERIC), CAST($13 AS NUMERIC),
            CAST($14 AS NUMERIC), $15, $16, $17, $18, $19, $20, $21
        )
        ",
    )
    .bind(plan.quote_id())
    .bind(plan.merchant_id())
    .bind(plan.payment_intent_id())
    .bind(plan.asset_id())
    .bind(plan.collector_address_id())
    .bind(plan.price_snapshot_id())
    .bind(plan.quote_policy_id())
    .bind(plan.rail_health_snapshot_id())
    .bind(plan.fiat_amount().currency.as_str())
    .bind(plan.fiat_amount().minor_units)
    .bind(plan.base_amount_raw().to_string())
    .bind(amount_raw.to_string())
    .bind(plan.rate_numerator().to_string())
    .bind(plan.rate_denominator().to_string())
    .bind(plan.price_sources())
    .bind(plan.price_observed_at())
    .bind(plan.policy_version())
    .bind(plan.rail_health_observed_at())
    .bind(plan.created_at())
    .bind(plan.expires_at())
    .bind(plan.late_payment_until())
    .execute(&mut **transaction)
    .await
    .map_err(classify_quote_insert_error)?;

    sqlx::query(
        r"
        INSERT INTO payment_attempts (
            id, merchant_id, payment_intent_id, quote_id, collector_address_id,
            expected_amount_raw, status, quote_expires_at, late_payment_until,
            created_at, updated_at
        ) VALUES (
            $1, $2, $3, $4, $5, CAST($6 AS NUMERIC), 'awaiting_payment',
            $7, $8, $9, $9
        )
        ",
    )
    .bind(plan.attempt_id())
    .bind(plan.merchant_id())
    .bind(plan.payment_intent_id())
    .bind(plan.quote_id())
    .bind(plan.collector_address_id())
    .bind(amount_raw.to_string())
    .bind(plan.expires_at())
    .bind(plan.late_payment_until())
    .bind(plan.created_at())
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    let lease_id = Uuid::now_v7();
    sqlx::query(
        r"
        INSERT INTO amount_leases (
            id, collector_address_id, amount_raw, attempt_id, leased_from, lease_until
        ) VALUES ($1, $2, CAST($3 AS NUMERIC), $4, $5, $6)
        ",
    )
    .bind(lease_id)
    .bind(plan.collector_address_id())
    .bind(amount_raw.to_string())
    .bind(plan.attempt_id())
    .bind(plan.created_at())
    .bind(plan.late_payment_until())
    .execute(&mut **transaction)
    .await
    .map_err(classify_lease_insert_error)?;
    Ok(lease_id)
}

async fn insert_quote_audit_events(
    transaction: &mut Transaction<'_, Postgres>,
    plan: &QuotePlan,
    actor_key_id: Uuid,
    lease_id: Uuid,
    amount_raw: RawAmount,
) -> Result<(), RepositoryError> {
    for (action, resource_type, resource_id, payload) in [
        (
            "payment_quote.issued",
            "payment_quote",
            plan.quote_id(),
            serde_json::json!({
                "attempt_id": plan.attempt_id(),
                "payment_intent_id": plan.payment_intent_id(),
                "amount_raw": amount_raw.to_string(),
                "expires_at": plan.expires_at(),
                "late_payment_until": plan.late_payment_until(),
                "policy_version": plan.policy_version(),
            }),
        ),
        (
            "amount_lease.allocated",
            "amount_lease",
            lease_id,
            serde_json::json!({
                "attempt_id": plan.attempt_id(),
                "collector_address_id": plan.collector_address_id(),
                "amount_raw": amount_raw.to_string(),
                "lease_until": plan.late_payment_until(),
            }),
        ),
        (
            "payment_intent.awaiting_payment",
            "payment_intent",
            plan.payment_intent_id(),
            serde_json::json!({"quote_id": plan.quote_id(), "attempt_id": plan.attempt_id()}),
        ),
    ] {
        sqlx::query(
            r"
            INSERT INTO audit_events (
                id, merchant_id, actor_type, actor_id, action, resource_type,
                resource_id, payload, created_at
            ) VALUES ($1, $2, 'api_key', $3, $4, $5, $6, $7, $8)
            ",
        )
        .bind(Uuid::now_v7())
        .bind(plan.merchant_id())
        .bind(actor_key_id)
        .bind(action)
        .bind(resource_type)
        .bind(resource_id)
        .bind(payload)
        .bind(plan.created_at())
        .execute(&mut **transaction)
        .await
        .map_err(unavailable)?;
    }
    Ok(())
}

async fn insert_system_audit(
    transaction: &mut Transaction<'_, Postgres>,
    merchant_id: Uuid,
    action: &str,
    resource_type: &str,
    resource_id: Uuid,
    payload: Value,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO audit_events (
            id, merchant_id, actor_type, action, resource_type, resource_id, payload
        ) VALUES ($1, $2, 'system', $3, $4, $5, $6)
        ",
    )
    .bind(Uuid::now_v7())
    .bind(merchant_id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(payload)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

async fn find_quote(
    transaction: &mut Transaction<'_, Postgres>,
    merchant_id: Uuid,
    quote_id: Uuid,
) -> Result<Option<IssuedQuote>, RepositoryError> {
    let row = sqlx::query_as::<_, IssuedQuoteRow>(
        r"
        SELECT quote.id, attempt.id AS attempt_id, quote.payment_intent_id,
               quote.asset_id, quote.collector_address_id,
               collector.address_text AS collector_address,
               quote.price_snapshot_id, quote.quote_policy_id,
               quote.rail_health_snapshot_id, quote.fiat_currency,
               quote.fiat_amount_minor, quote.amount_raw::TEXT,
               quote.rate_numerator::TEXT, quote.rate_denominator::TEXT,
               quote.price_sources, quote.price_observed_at, quote.policy_version,
               quote.rail_health_observed_at, quote.created_at, quote.expires_at,
               quote.late_payment_until
          FROM payment_quotes AS quote
          JOIN payment_attempts AS attempt ON attempt.quote_id = quote.id
          JOIN collector_addresses AS collector ON collector.id = quote.collector_address_id
         WHERE quote.merchant_id = $1 AND quote.id = $2
        ",
    )
    .bind(merchant_id)
    .bind(quote_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;
    row.map(TryInto::try_into).transpose()
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
pub(crate) fn unavailable(error: sqlx::Error) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn corrupt_money(error: MoneyError) -> RepositoryError {
    RepositoryError::CorruptData(error.to_string())
}

pub(crate) fn corrupt(message: impl Into<String>) -> RepositoryError {
    RepositoryError::CorruptData(message.into())
}

fn required<T>(value: Option<T>, field: &str) -> Result<T, RepositoryError> {
    value.ok_or_else(|| corrupt(format!("quote context omitted {field}")))
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

#[allow(clippy::needless_pass_by_value)]
fn classify_quote_insert_error(error: sqlx::Error) -> RepositoryError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.constraint() == Some("payment_quotes_payment_intent_id_key")
    {
        return RepositoryError::PaymentIntentNotQuotable;
    }
    unavailable(error)
}

#[allow(clippy::needless_pass_by_value)]
fn classify_lease_insert_error(error: sqlx::Error) -> RepositoryError {
    if let sqlx::Error::Database(database_error) = &error
        && matches!(
            database_error.constraint(),
            Some(
                "amount_leases_collector_address_id_amount_raw_key"
                    | "amount_leases_attempt_id_key"
            )
        )
    {
        return RepositoryError::AmountSlotsExhausted;
    }
    unavailable(error)
}

#[cfg(test)]
mod tests {
    use std::{env, error::Error, str::FromStr, sync::Arc};

    use gateway_application::{
        Clock, IdempotentQuote, IssueQuote, QuoteRepository, QuoteService, RepositoryError,
    };
    use gateway_domain::{
        CurrencyCode, FiatAmount, PriceSnapshot, QuotePlan, QuotePolicySnapshot, RailHealth,
        RailHealthSnapshot, RawAmount,
    };
    use gateway_scheduler::{BatchConfig, ExpiryScheduler, RetryPolicy};
    use serde_json::json;
    use sqlx::{PgPool, postgres::PgPoolOptions};
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::PostgresRepository;
    use crate::{migrate, test_support::DATABASE};

    const MERCHANT_ONE: Uuid = Uuid::from_u128(101);
    const MERCHANT_TWO: Uuid = Uuid::from_u128(102);
    const ACTOR_KEY: Uuid = Uuid::from_u128(201);
    const ASSET_ID: Uuid = Uuid::from_u128(301);
    const COLLECTOR_ID: Uuid = Uuid::from_u128(401);
    const PRICE_SNAPSHOT_ID: Uuid = Uuid::from_u128(601);
    const QUOTE_POLICY_ID: Uuid = Uuid::from_u128(602);
    const RAIL_HEALTH_SNAPSHOT_ID: Uuid = Uuid::from_u128(603);
    const INTENT_ONE: Uuid = Uuid::from_u128(501);
    const INTENT_TWO: Uuid = Uuid::from_u128(502);

    #[derive(Debug, Clone, Copy)]
    struct FixedClock(OffsetDateTime);

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            self.0
        }
    }

    #[tokio::test]
    #[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
    #[allow(clippy::too_many_lines)]
    async fn quote_leases_are_concurrent_replayable_isolated_and_archived_exactly_once()
    -> Result<(), Box<dyn Error>> {
        let _fixture = DATABASE.lock().await;
        let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await?;
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        reset_database(&pool, now).await?;
        let repository = PostgresRepository::new(pool.clone());
        let first_plan = plan(MERCHANT_ONE, INTENT_ONE, now)?;
        let first_quote_id = first_plan.quote_id();
        let second_plan = plan(MERCHANT_ONE, INTENT_TWO, now)?;
        let first_repository = repository.clone();
        let second_repository = repository.clone();

        let first = first_repository.issue_quote_idempotently(
            first_plan,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "quote_first_000000000001",
            &[1; 32],
        );
        let second = second_repository.issue_quote_idempotently(
            second_plan,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "quote_second_00000000001",
            &[2; 32],
        );
        let (first, second) = tokio::join!(first, second);
        let first = match first? {
            IdempotentQuote::Issued(quote) => quote,
            IdempotentQuote::Replayed(_) => return Err("first issue unexpectedly replayed".into()),
        };
        let second = match second? {
            IdempotentQuote::Issued(quote) => quote,
            IdempotentQuote::Replayed(_) => return Err("second issue unexpectedly replayed".into()),
        };
        let mut amounts = [first.amount_raw.to_string(), second.amount_raw.to_string()];
        amounts.sort();
        assert_eq!(amounts, ["100".to_owned(), "101".to_owned()]);

        let replay_plan = plan(MERCHANT_ONE, INTENT_ONE, now)?;
        let replay = repository
            .issue_quote_idempotently(
                replay_plan,
                ACTOR_KEY,
                "POST /v1/payment-intents/:id/quotes",
                "quote_first_000000000001",
                &[1; 32],
            )
            .await?;
        match replay {
            IdempotentQuote::Replayed(quote) => assert_eq!(quote.id, first_quote_id),
            IdempotentQuote::Issued(_) => return Err("replay unexpectedly issued a quote".into()),
        }

        let foreign = repository
            .issue_quote_idempotently(
                plan(MERCHANT_TWO, INTENT_ONE, now)?,
                ACTOR_KEY,
                "POST /v1/payment-intents/:id/quotes",
                "quote_foreign_00000000001",
                &[3; 32],
            )
            .await;
        assert!(matches!(
            foreign,
            Err(RepositoryError::PaymentIntentNotQuotable)
        ));

        // Two sweeps run at once, as an overlapping scheduler tick or a second
        // replica would. Each attempt must expire exactly once.
        let quote_expiry = now + Duration::minutes(15);
        let (first_sweep, second_sweep) = tokio::join!(
            repository.expire_quotes_and_archive_leases(quote_expiry, 100),
            repository.expire_quotes_and_archive_leases(quote_expiry, 100)
        );
        let first_sweep = first_sweep?;
        let second_sweep = second_sweep?;
        assert_eq!(first_sweep.quotes_expired + second_sweep.quotes_expired, 2);
        assert_eq!(
            first_sweep.leases_archived + second_sweep.leases_archived,
            0
        );
        assert_eq!(count(&pool, "amount_leases").await?, 2);
        assert_eq!(count(&pool, "amount_lease_history").await?, 0);

        let before_late_window = repository
            .expire_quotes_and_archive_leases(quote_expiry + Duration::days(29), 100)
            .await?;
        assert_eq!(before_late_window.quotes_expired, 0);
        assert_eq!(before_late_window.leases_archived, 0);
        assert_eq!(count(&pool, "amount_leases").await?, 2);

        let late_deadline = quote_expiry + Duration::days(30);
        let (first_archive, second_archive) = tokio::join!(
            repository.expire_quotes_and_archive_leases(late_deadline, 100),
            repository.expire_quotes_and_archive_leases(late_deadline, 100)
        );
        let archived_leases = first_archive?.leases_archived + second_archive?.leases_archived;
        assert_eq!(archived_leases, 2);
        assert_eq!(count(&pool, "amount_leases").await?, 0);
        assert_eq!(count(&pool, "amount_lease_history").await?, 2);

        let issued_audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action = 'payment_quote.issued'",
        )
        .fetch_one(&pool)
        .await?;
        let archived_audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action = 'amount_lease.archived'",
        )
        .fetch_one(&pool)
        .await?;
        let attempt_expiry_audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action = 'payment_attempt.expired'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(issued_audits, 2);
        assert_eq!(archived_audits, 2);
        assert_eq!(attempt_expiry_audits, 2);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
    async fn the_scheduler_expires_and_archives_through_the_application_stack()
    -> Result<(), Box<dyn Error>> {
        let _fixture = DATABASE.lock().await;
        let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await?;
        let issued_at = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        reset_database(&pool, issued_at).await?;
        let repository = Arc::new(PostgresRepository::new(pool.clone()));
        QuoteService::new(Arc::clone(&repository), FixedClock(issued_at))
            .issue(
                MERCHANT_ONE,
                ACTOR_KEY,
                "quote_scheduler_00000001",
                IssueQuote {
                    payment_intent_id: INTENT_ONE,
                    asset_id: ASSET_ID,
                },
            )
            .await?;

        // The scheduler drives the real service, which drives the real
        // transaction: nothing in this path is a test double.
        let after_late_window = issued_at + Duration::days(31);
        let scheduler = ExpiryScheduler::new(
            Arc::new(QuoteService::new(
                Arc::clone(&repository),
                FixedClock(after_late_window),
            )),
            BatchConfig {
                interval: std::time::Duration::from_secs(1),
                batch_limit: 100,
                max_batches_per_tick: 4,
                lease_seconds: 30,
                retry: RetryPolicy::default(),
            },
        )?;

        let report = scheduler.run_once().await?;

        assert_eq!(report.quotes_expired, 1);
        assert_eq!(report.leases_archived, 1);
        assert_eq!(report.retries, 0);
        assert!(report.drained);
        assert_eq!(count(&pool, "amount_leases").await?, 0);
        assert_eq!(count(&pool, "amount_lease_history").await?, 1);
        let intent_status: String =
            sqlx::query_scalar("SELECT status FROM payment_intents WHERE id = $1")
                .bind(INTENT_ONE)
                .fetch_one(&pool)
                .await?;
        assert_eq!(intent_status, "expired");
        let metrics = scheduler.metrics().snapshot();
        assert_eq!(metrics.runs_succeeded, 1);
        assert_eq!(metrics.runs_failed, 0);
        assert_eq!(metrics.batches_executed, 1);
        assert_eq!(metrics.backlog_left, 0);

        // A second sweep has nothing left to do and must not re-archive.
        let repeat = scheduler.run_once().await?;
        assert_eq!(repeat.quotes_expired, 0);
        assert_eq!(repeat.leases_archived, 0);
        assert_eq!(count(&pool, "amount_lease_history").await?, 1);
        Ok(())
    }

    fn plan(
        merchant_id: Uuid,
        payment_intent_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<QuotePlan, Box<dyn Error>> {
        Ok(QuotePlan::build(
            merchant_id,
            payment_intent_id,
            ASSET_ID,
            COLLECTOR_ID,
            "TTestCollector".to_owned(),
            FiatAmount::positive(CurrencyCode::new("USD")?, 1_000)?,
            Some(PriceSnapshot {
                id: PRICE_SNAPSHOT_ID,
                rate_numerator: RawAmount::from_str("1")?,
                rate_denominator: RawAmount::from_str("10")?,
                sources: json!([
                    {"provider_group":"source-a","observed_at":now},
                    {"provider_group":"source-b","observed_at":now}
                ]),
                observed_at: now,
            }),
            Some(QuotePolicySnapshot {
                id: QUOTE_POLICY_ID,
                version: "policy-v1".to_owned(),
                quote_ttl_seconds: 900,
                late_payment_window_seconds: 2_592_000,
                amount_slot_count: 2,
                max_price_age_seconds: 60,
                max_policy_age_seconds: 60,
                max_rail_health_age_seconds: 60,
                observed_at: now,
            }),
            Some(RailHealthSnapshot {
                id: RAIL_HEALTH_SNAPSHOT_ID,
                health: RailHealth::Healthy,
                observed_at: now,
            }),
            now,
        )?)
    }

    #[allow(clippy::too_many_lines)]
    async fn reset_database(pool: &PgPool, now: OffsetDateTime) -> Result<(), Box<dyn Error>> {
        migrate(pool).await?;
        sqlx::query("TRUNCATE chain_assets, merchants CASCADE")
            .execute(pool)
            .await?;
        for (merchant_id, external_id) in [
            (MERCHANT_ONE, "quote-merchant-one"),
            (MERCHANT_TWO, "quote-merchant-two"),
        ] {
            sqlx::query(
                "INSERT INTO merchants (id, external_id, display_name, status) \
                 VALUES ($1, $2, $2, 'active')",
            )
            .bind(merchant_id)
            .bind(external_id)
            .execute(pool)
            .await?;
        }
        sqlx::query(
            r"
            INSERT INTO chain_assets (
                id, chain, network, chain_environment, contract_address_key,
                display_symbol, decimals, status, pinned_sha256, approved_by
            ) VALUES (
                $1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active',
                encode(sha256($2), 'hex'), 'test-fixture'
            )
            ",
        )
        .bind(ASSET_ID)
        .bind([7_u8; 20].as_slice())
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO collector_addresses (
                id, asset_id, address_key, address_text, state, valid_from,
                pinned_sha256, approved_by
            ) VALUES (
                $1, $2, $3, 'TTestCollector', 'active', $4,
                encode(sha256($3), 'hex'), 'test-fixture'
            )
            ",
        )
        .bind(COLLECTOR_ID)
        .bind(ASSET_ID)
        .bind([8_u8; 21].as_slice())
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO price_snapshots (
                id, asset_id, fiat_currency, rate_numerator, rate_denominator,
                sources, observed_at
            ) VALUES ($1, $2, 'USD', 1, 10, $3, $4)
            ",
        )
        .bind(PRICE_SNAPSHOT_ID)
        .bind(ASSET_ID)
        .bind(json!([
            {"provider_group":"source-a","observed_at":now},
            {"provider_group":"source-b","observed_at":now}
        ]))
        .bind(now)
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO quote_policies (
                id, asset_id, fiat_currency, version, status,
                quote_ttl_seconds, late_payment_window_seconds,
                amount_slot_count, max_price_age_seconds,
                max_policy_age_seconds, max_rail_health_age_seconds, observed_at
            ) VALUES ($1, $2, 'USD', 'policy-v1', 'active', 900, 2592000,
                      2, 60, 60, 60, $3)
            ",
        )
        .bind(QUOTE_POLICY_ID)
        .bind(ASSET_ID)
        .bind(now)
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO rail_health_snapshots (id, asset_id, health, observed_at)
            VALUES ($1, $2, 'healthy', $3)
            ",
        )
        .bind(RAIL_HEALTH_SNAPSHOT_ID)
        .bind(ASSET_ID)
        .bind(now)
        .execute(pool)
        .await?;
        for (intent_id, reference) in [(INTENT_ONE, "order-one"), (INTENT_TWO, "order-two")] {
            sqlx::query(
                r"
                INSERT INTO payment_intents (
                    id, merchant_id, amount_minor, currency, status, reference,
                    created_at, updated_at
                ) VALUES ($1, $2, 1000, 'USD', 'requires_quote', $3, $4, $4)
                ",
            )
            .bind(intent_id)
            .bind(MERCHANT_ONE)
            .bind(reference)
            .bind(OffsetDateTime::UNIX_EPOCH)
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    async fn count(pool: &PgPool, table: &str) -> Result<i64, Box<dyn Error>> {
        let query = match table {
            "amount_leases" => "SELECT count(*) FROM amount_leases",
            "amount_lease_history" => "SELECT count(*) FROM amount_lease_history",
            _ => return Err("unsupported test table".into()),
        };
        Ok(sqlx::query_scalar(query).fetch_one(pool).await?)
    }
}
