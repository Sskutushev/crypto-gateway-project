//! The settlement export's reads: decisions grouped by day, and the
//! allocation rows behind the same decisions summed independently.

use std::str::FromStr;

use async_trait::async_trait;
use gateway_application::{
    AccountingRepository, AllocationControl, RepositoryError, SettlementDay, SettlementLedger,
};
use gateway_domain::RawAmount;
use sqlx::FromRow;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

/// Outcomes that moved money. `held`, `manual_required`, `rejected` and the
/// rest allocated nothing and are not settled money.
const MONEY_OUTCOMES: &[&str] = &["settled", "overpaid", "partial"];

#[derive(Debug, FromRow)]
struct DayRow {
    day: Date,
    merchant_id: Uuid,
    merchant_external_id: String,
    asset_id: Uuid,
    fiat_currency: String,
    payments: i64,
    partial_payments: i64,
    overpaid_payments: i64,
    allocated_raw: String,
    fiat_minor: String,
    remainder_raw: String,
}

#[derive(Debug, FromRow)]
struct ControlRow {
    asset_id: Uuid,
    allocation_rows: i64,
    allocated_raw: String,
}

fn raw(value: &str) -> Result<RawAmount, RepositoryError> {
    if !value.is_empty() && value.bytes().all(|byte| byte == b'0') {
        return Ok(RawAmount::ZERO);
    }
    RawAmount::from_str(value).map_err(|_| corrupt(format!("a settled sum is not raw: {value}")))
}

fn count(value: i64) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| corrupt("a count is negative"))
}

#[async_trait]
impl AccountingRepository for PostgresRepository {
    async fn settlement_ledger(
        &self,
        start: OffsetDateTime,
        end: OffsetDateTime,
        merchant_id: Option<Uuid>,
        max_rows: usize,
    ) -> Result<SettlementLedger, RepositoryError> {
        // One snapshot for both questions, so the control sum is taken over
        // exactly the decisions the rows were built from.
        let mut tx = self.pool().begin().await.map_err(unavailable)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let rows = sqlx::query_as::<_, DayRow>(
            r"SELECT (decision.decided_at AT TIME ZONE 'UTC')::date AS day,
                     decision.merchant_id,
                     merchant.external_id AS merchant_external_id,
                     transfer.asset_id,
                     intent.currency::text AS fiat_currency,
                     count(*) FILTER (WHERE decision.outcome IN ('settled', 'overpaid'))
                         AS payments,
                     count(*) FILTER (WHERE decision.outcome = 'partial') AS partial_payments,
                     count(*) FILTER (WHERE decision.outcome = 'overpaid') AS overpaid_payments,
                     sum(decision.allocated_raw)::text AS allocated_raw,
                     coalesce(sum(decision.fiat_amount_minor)
                              FILTER (WHERE decision.outcome IN ('settled', 'overpaid')), 0)::text
                         AS fiat_minor,
                     sum(decision.remainder_raw)::text AS remainder_raw
                FROM payment_settlement_decisions AS decision
                JOIN chain_transfers AS transfer ON transfer.id = decision.transfer_id
                JOIN payment_intents AS intent ON intent.id = decision.payment_intent_id
                JOIN merchants AS merchant ON merchant.id = decision.merchant_id
               WHERE decision.outcome = ANY($1)
                 AND decision.decided_at >= $2
                 AND decision.decided_at < $3
                 AND ($4::UUID IS NULL OR decision.merchant_id = $4)
               GROUP BY 1, 2, 3, 4, 5
               ORDER BY 1, 2, 4, 5
               LIMIT $5",
        )
        .bind(MONEY_OUTCOMES)
        .bind(start)
        .bind(end)
        .bind(merchant_id)
        .bind(
            i64::try_from(max_rows)
                .unwrap_or(i64::MAX)
                .saturating_add(1),
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        let controls = sqlx::query_as::<_, ControlRow>(
            r"SELECT transfer.asset_id,
                     count(*) AS allocation_rows,
                     sum(allocation.allocated_raw)::text AS allocated_raw
                FROM payment_allocations AS allocation
                JOIN payment_settlement_decisions AS decision
                  ON decision.payment_intent_id = allocation.payment_intent_id
                 AND decision.transfer_id = allocation.transfer_id
                JOIN chain_transfers AS transfer ON transfer.id = allocation.transfer_id
               WHERE decision.outcome = ANY($1)
                 AND decision.decided_at >= $2
                 AND decision.decided_at < $3
                 AND ($4::UUID IS NULL OR decision.merchant_id = $4)
               GROUP BY transfer.asset_id
               ORDER BY transfer.asset_id",
        )
        .bind(MONEY_OUTCOMES)
        .bind(start)
        .bind(end)
        .bind(merchant_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;

        let mut days = Vec::with_capacity(rows.len());
        for row in rows {
            days.push(SettlementDay {
                day: row.day,
                merchant_id: row.merchant_id,
                merchant_external_id: row.merchant_external_id,
                asset_id: row.asset_id,
                fiat_currency: row.fiat_currency,
                payments: count(row.payments)?,
                partial_payments: count(row.partial_payments)?,
                overpaid_payments: count(row.overpaid_payments)?,
                allocated_raw: raw(&row.allocated_raw)?,
                fiat_minor: row
                    .fiat_minor
                    .parse::<i128>()
                    .map_err(|_| corrupt("a fiat sum is not an integer"))?,
                remainder_raw: raw(&row.remainder_raw)?,
            });
        }
        let mut allocations = Vec::with_capacity(controls.len());
        for control in controls {
            allocations.push(AllocationControl {
                asset_id: control.asset_id,
                allocation_rows: count(control.allocation_rows)?,
                allocated_raw: raw(&control.allocated_raw)?,
            });
        }
        Ok(SettlementLedger { days, allocations })
    }
}
