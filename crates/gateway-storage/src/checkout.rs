//! The hosted payment page's read: one attempt, by its checkout token.

use std::str::FromStr;

use async_trait::async_trait;
use gateway_application::{CheckoutFacts, CheckoutRepository, CheckoutView, RepositoryError};
use gateway_domain::{CurrencyCode, FiatAmount, QuoteAsset, RawAmount};
use sqlx::FromRow;
use time::OffsetDateTime;

use crate::postgres::{PostgresRepository, contract_display, corrupt, unavailable};

#[derive(Debug, FromRow)]
struct CheckoutRow {
    intent_status: String,
    attempt_status: String,
    merchant_name: String,
    description: Option<String>,
    amount_minor: i64,
    currency: String,
    expected_raw: String,
    quote_expires_at: OffsetDateTime,
    late_payment_until: OffsetDateTime,
    collector_address: String,
    chain: String,
    network: String,
    chain_environment: String,
    symbol: String,
    decimals: i16,
    contract_address_key: Vec<u8>,
    received_raw: String,
    needs_review: bool,
    decided_tx: Option<String>,
    seen_tx: Option<String>,
}

/// A public explorer page for a transaction, where the network has one.
fn explorer_url(chain: &str, network: &str, hash: &str) -> Option<String> {
    let host = match (chain, network) {
        ("tron", "mainnet") => "tronscan.org",
        ("tron", "nile") => "nile.tronscan.org",
        ("tron", "shasta") => "shasta.tronscan.org",
        _ => return None,
    };
    Some(format!("https://{host}/#/transaction/{hash}"))
}

fn raw(value: &str) -> Result<RawAmount, RepositoryError> {
    if value.trim_start_matches('0').is_empty() {
        return Ok(RawAmount::ZERO);
    }
    RawAmount::from_str(value).map_err(|error| corrupt(error.to_string()))
}

#[async_trait]
impl CheckoutRepository for PostgresRepository {
    async fn checkout_view(&self, token: &str) -> Result<Option<CheckoutView>, RepositoryError> {
        let Some(row) = sqlx::query_as::<_, CheckoutRow>(
            r"
            SELECT intent.status AS intent_status,
                   attempt.status AS attempt_status,
                   merchant.display_name AS merchant_name,
                   intent.description,
                   intent.amount_minor,
                   intent.currency,
                   attempt.expected_amount_raw::TEXT AS expected_raw,
                   attempt.quote_expires_at,
                   attempt.late_payment_until,
                   collector.address_text AS collector_address,
                   asset.chain, asset.network, asset.chain_environment,
                   asset.display_symbol AS symbol, asset.decimals, asset.contract_address_key,
                   COALESCE((SELECT sum(allocation.allocated_raw)
                               FROM payment_allocations AS allocation
                              WHERE allocation.attempt_id = attempt.id), 0)::TEXT AS received_raw,
                   EXISTS (SELECT 1 FROM payment_settlement_decisions AS decision
                            WHERE decision.attempt_id = attempt.id
                              AND decision.outcome IN ('held', 'manual_required')) AS needs_review,
                   (SELECT transfer.tx_hash
                      FROM payment_settlement_decisions AS decision
                      JOIN chain_transfers AS transfer ON transfer.id = decision.transfer_id
                     WHERE decision.attempt_id = attempt.id
                     ORDER BY decision.decided_at DESC
                     LIMIT 1) AS decided_tx,
                   -- A transfer of exactly this amount to this address, seen
                   -- by any source since the attempt began: on its way, not paid.
                   (SELECT observation.tx_hash
                      FROM chain_observations AS observation
                     WHERE observation.collector_address_id = attempt.collector_address_id
                       AND observation.amount_raw = attempt.expected_amount_raw
                       AND observation.block_time >= attempt.created_at
                     ORDER BY observation.observed_at DESC
                     LIMIT 1) AS seen_tx
              FROM payment_attempts AS attempt
              JOIN payment_intents AS intent ON intent.id = attempt.payment_intent_id
              JOIN merchants AS merchant ON merchant.id = attempt.merchant_id
              JOIN payment_quotes AS quote ON quote.id = attempt.quote_id
              JOIN chain_assets AS asset ON asset.id = quote.asset_id
              JOIN collector_addresses AS collector ON collector.id = attempt.collector_address_id
             WHERE attempt.checkout_token = $1
            ",
        )
        .bind(token)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?
        else {
            return Ok(None);
        };

        let decimals =
            u8::try_from(row.decimals).map_err(|_| corrupt("asset decimals out of range"))?;
        let currency =
            CurrencyCode::new(row.currency).map_err(|error| corrupt(error.to_string()))?;
        let fiat_amount = FiatAmount::positive(currency, row.amount_minor)
            .map_err(|error| corrupt(error.to_string()))?;
        let amount_raw = raw(&row.expected_raw)?;
        let received = raw(&row.received_raw)?;
        let status = CheckoutFacts {
            intent_status: row.intent_status,
            attempt_status: row.attempt_status,
            needs_review: row.needs_review,
            seen_on_chain: row.seen_tx.is_some(),
        }
        .status();
        let transaction_hash = row.decided_tx.or(row.seen_tx);
        let explorer_url = transaction_hash
            .as_deref()
            .and_then(|hash| explorer_url(&row.chain, &row.network, hash));
        Ok(Some(CheckoutView {
            status,
            merchant_name: row.merchant_name,
            description: row.description,
            fiat_amount,
            asset: QuoteAsset {
                contract_address: contract_display(&row.chain, &row.contract_address_key)?,
                chain: row.chain,
                network: row.network,
                chain_environment: row.chain_environment,
                symbol: row.symbol,
                decimals,
            },
            collector_address: row.collector_address,
            amount: amount_raw.to_decimal_string(decimals),
            amount_raw,
            received: received.to_decimal_string(decimals),
            expires_at: row.quote_expires_at,
            late_payment_until: row.late_payment_until,
            transaction_hash,
            explorer_url,
        }))
    }
}
