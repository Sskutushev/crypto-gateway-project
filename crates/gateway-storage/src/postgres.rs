//! The `PostgreSQL` repository: one connection pool, one type, the trait
//! implementations split by the aggregate they serve. The row types and the
//! error classification stay here because every submodule reads them.

mod intents;
mod quotes;
#[cfg(test)]
mod tests;

use std::str::FromStr;

use gateway_application::{CollectorCandidate, QuoteContext, RepositoryError};
use gateway_domain::{
    CurrencyCode, FiatAmount, IssuedQuote, MoneyError, PaymentIntent, PaymentIntentStatus,
    PriceSnapshot, QuoteAsset, QuotePolicySnapshot, RailHealth, RailHealthSnapshot, RawAmount,
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
    asset_chain: String,
    asset_network: String,
    asset_environment: String,
    asset_symbol: String,
    asset_decimals: i16,
    contract_address_key: Vec<u8>,
    checkout_token: String,
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// A token contract in the chain's own display form. TRON wallets show
/// base58check; any other chain is shown as the canonical bytes in hex until
/// its adapter supplies a display form.
pub(crate) fn contract_display(chain: &str, key: &[u8]) -> Result<String, RepositoryError> {
    if chain == "tron" {
        // Both TRON byte forms are real: 21 bytes with the 0x41 prefix, and
        // the 20-byte form event logs carry. Anything else is corrupt.
        let address = if key.len() == 20 {
            gateway_tron::from_evm_bytes(key)
        } else {
            gateway_domain::AddressKey::new(key.to_vec())
                .map_err(|_| gateway_tron::TronAddressError::WrongLength)
        }
        .map_err(|error| RepositoryError::CorruptData(error.to_string()))?;
        return gateway_tron::to_base58(&address)
            .map_err(|error| RepositoryError::CorruptData(error.to_string()));
    }
    Ok(key
        .iter()
        .flat_map(|byte| {
            [
                HEX_DIGITS[usize::from(byte >> 4)],
                HEX_DIGITS[usize::from(byte & 0x0f)],
            ]
        })
        .map(char::from)
        .collect())
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
        let decimals = u8::try_from(row.asset_decimals)
            .map_err(|_| RepositoryError::CorruptData("asset decimals out of range".to_owned()))?;
        let asset = QuoteAsset {
            contract_address: contract_display(&row.asset_chain, &row.contract_address_key)?,
            chain: row.asset_chain,
            network: row.asset_network,
            chain_environment: row.asset_environment,
            symbol: row.asset_symbol,
            decimals,
        };

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
            amount: amount_raw.to_decimal_string(decimals),
            asset,
            checkout_token: row.checkout_token,
        })
    }
}

#[derive(Debug, FromRow)]
struct QuoteContextRow {
    collector_address_id: Uuid,
    collector_address: String,
    open_leases: i64,
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
    rail_stop_reason: Option<String>,
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
            candidates: vec![CollectorCandidate {
                id: row.collector_address_id,
                address: row.collector_address,
                open_leases: u64::try_from(row.open_leases)
                    .map_err(|_| corrupt("negative lease count"))?,
            }],
            price,
            policy,
            rail_health,
            rail_stop_reason: row.rail_stop_reason,
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
        && matches!(
            database_error.constraint(),
            Some("payment_attempts_one_live_per_intent")
        )
    {
        // Two quotes for the same order at once: one wins, the other is told
        // the order is not quotable now.
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
