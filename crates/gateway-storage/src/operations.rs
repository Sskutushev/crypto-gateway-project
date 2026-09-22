//! Storage for the operator surface.
//!
//! One submission is one transaction: the snapshot, if there is one, and every
//! reading behind it with its outcome. A refused submission still writes its
//! readings, because the disagreement is the record an operator needs.

use async_trait::async_trait;
use gateway_application::{
    OperationsRepository, OperatorCredential, OperatorScope, PriceIngestion, PriceOutcome,
    RailStop, RecordedPrice, RepositoryError, RiskSubmission,
};
use gateway_domain::{CurrencyCode, PriceAggregationPolicy, RailHealth};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

#[derive(Debug, FromRow)]
struct OperatorKeyRow {
    id: Uuid,
    label: String,
    scopes: Vec<String>,
}

#[derive(Debug, FromRow)]
struct PricePolicyRow {
    min_price_sources: i32,
    max_price_age_seconds: i64,
    max_price_deviation_bps: i32,
}

#[derive(Debug, FromRow)]
struct RailStopRow {
    id: Uuid,
    asset_id: Uuid,
    reason_code: String,
    detail: Option<String>,
    opened_by: String,
    opened_at: OffsetDateTime,
}

impl From<RailStopRow> for RailStop {
    fn from(row: RailStopRow) -> Self {
        Self {
            id: row.id,
            asset_id: row.asset_id,
            reason_code: row.reason_code,
            detail: row.detail,
            opened_by: row.opened_by,
            opened_at: row.opened_at,
        }
    }
}

#[async_trait]
impl OperationsRepository for PostgresRepository {
    async fn authenticate_operator_key(
        &self,
        secret_hash: &[u8; 32],
    ) -> Result<Option<OperatorCredential>, RepositoryError> {
        let row = sqlx::query_as::<_, OperatorKeyRow>(
            r"
            UPDATE operator_api_keys
               SET last_used_at = now()
             WHERE secret_hash = $1
               AND revoked_at IS NULL
            RETURNING id, label, scopes
            ",
        )
        .bind(secret_hash.as_slice())
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut scopes = Vec::with_capacity(row.scopes.len());
        for scope in &row.scopes {
            // An unrecognised scope is stored corruption, never a permissive
            // default: the key is refused rather than widened.
            scopes.push(
                OperatorScope::parse(scope)
                    .map_err(|error| corrupt(format!("operator key: {error}")))?,
            );
        }
        Ok(Some(OperatorCredential {
            key_id: row.id,
            label: row.label,
            scopes,
        }))
    }

    async fn price_policy(
        &self,
        asset_id: Uuid,
        currency: &CurrencyCode,
    ) -> Result<Option<PriceAggregationPolicy>, RepositoryError> {
        let row = sqlx::query_as::<_, PricePolicyRow>(
            r"
            SELECT min_price_sources, max_price_age_seconds, max_price_deviation_bps
              FROM quote_policies
             WHERE asset_id = $1
               AND fiat_currency = $2
               AND status = 'active'
             LIMIT 1
            ",
        )
        .bind(asset_id)
        .bind(currency.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        row.map(|row| {
            Ok(PriceAggregationPolicy {
                min_sources: u32::try_from(row.min_price_sources)
                    .map_err(|_| corrupt("quote policy holds a negative source count"))?,
                max_age_seconds: row.max_price_age_seconds,
                max_deviation_bps: u32::try_from(row.max_price_deviation_bps)
                    .map_err(|_| corrupt("quote policy holds a negative deviation ceiling"))?,
            })
        })
        .transpose()
    }

    async fn record_price_ingestion(
        &self,
        ingestion: &PriceIngestion,
    ) -> Result<Option<RecordedPrice>, RepositoryError> {
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;

        let recorded = match &ingestion.outcome {
            PriceOutcome::Aggregated(aggregated) => {
                let used: Vec<usize> = aggregated.used.clone();
                let sources = sources_json(ingestion, &used)?;
                // The snapshot is only as fresh as the stalest reading behind
                // it: taking the newest would make the evidence look younger
                // than it is, and staleness is what closes new quotes.
                let observed_at = used
                    .iter()
                    .filter_map(|index| ingestion.readings.get(*index))
                    .map(|reading| reading.observed_at)
                    .min()
                    .ok_or_else(|| corrupt("an aggregated price used no reading"))?;
                let snapshot_id = Uuid::now_v7();
                sqlx::query(
                    r"
                    INSERT INTO price_snapshots (
                        id, asset_id, fiat_currency, rate_numerator, rate_denominator,
                        sources, observed_at, ingested_by, source_group_count, deviation_bps
                    ) VALUES (
                        $1, $2, $3, CAST($4 AS NUMERIC), CAST($5 AS NUMERIC),
                        $6, $7, $8, $9, $10
                    )
                    ",
                )
                .bind(snapshot_id)
                .bind(ingestion.asset_id)
                .bind(ingestion.currency.as_str())
                .bind(aggregated.rate_numerator.to_string())
                .bind(aggregated.rate_denominator.to_string())
                .bind(&sources)
                .bind(observed_at)
                .bind(ingestion.ingested_by)
                .bind(i32::try_from(aggregated.group_count).unwrap_or(i32::MAX))
                .bind(i32::try_from(aggregated.deviation_bps).unwrap_or(i32::MAX))
                .execute(&mut *transaction)
                .await
                .map_err(unavailable)?;

                for (index, reading) in ingestion.readings.iter().enumerate() {
                    let discard = aggregated
                        .discarded
                        .iter()
                        .find(|(discarded, _)| *discarded == index)
                        .map(|(_, reason)| reason.as_str());
                    let snapshot = if discard.is_none() {
                        Some(snapshot_id)
                    } else {
                        None
                    };
                    insert_reading(&mut transaction, ingestion, reading, snapshot, discard).await?;
                }

                Some(RecordedPrice {
                    snapshot_id,
                    rate_numerator: aggregated.rate_numerator.to_string(),
                    rate_denominator: aggregated.rate_denominator.to_string(),
                    group_count: aggregated.group_count,
                    deviation_bps: aggregated.deviation_bps,
                    observed_at,
                })
            }
            PriceOutcome::Refused(reason) => {
                for reading in &ingestion.readings {
                    insert_reading(&mut transaction, ingestion, reading, None, Some(reason))
                        .await?;
                }
                None
            }
        };

        transaction.commit().await.map_err(unavailable)?;
        Ok(recorded)
    }

    async fn record_rail_health(
        &self,
        asset_id: Uuid,
        health: RailHealth,
        detail: Option<String>,
        ingested_by: Uuid,
        observed_at: OffsetDateTime,
    ) -> Result<Uuid, RepositoryError> {
        let id = Uuid::now_v7();
        sqlx::query(
            r"
            INSERT INTO rail_health_snapshots (
                id, asset_id, health, observed_at, ingested_by, detail
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ",
        )
        .bind(id)
        .bind(asset_id)
        .bind(health_text(health))
        .bind(observed_at)
        .bind(ingested_by)
        .bind(detail)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(id)
    }

    async fn open_rail_stop(
        &self,
        asset_id: Uuid,
        reason_code: &str,
        detail: Option<&str>,
        opened_by: &str,
        opened_at: OffsetDateTime,
    ) -> Result<RailStop, RepositoryError> {
        let inserted = sqlx::query_as::<_, RailStopRow>(
            r"
            INSERT INTO rail_stops (
                id, asset_id, reason_code, detail, opened_by, opened_at
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT DO NOTHING
            RETURNING id, asset_id, reason_code, detail, opened_by, opened_at
            ",
        )
        .bind(Uuid::now_v7())
        .bind(asset_id)
        .bind(reason_code)
        .bind(detail)
        .bind(opened_by)
        .bind(opened_at)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        if let Some(row) = inserted {
            return Ok(row.into());
        }
        // A rail that is already closed stays closed under the reason it was
        // closed with. Reopening it under a second reason would lose the first.
        self.find_open_rail_stop(asset_id)
            .await?
            .ok_or_else(|| corrupt("a rail stop conflicted with nothing that is open"))
    }

    async fn clear_rail_stop(
        &self,
        asset_id: Uuid,
        cleared_by: &str,
        reason: &str,
        cleared_at: OffsetDateTime,
    ) -> Result<bool, RepositoryError> {
        let cleared = sqlx::query(
            r"
            UPDATE rail_stops
               SET cleared_by = $2, cleared_reason = $3, cleared_at = $4
             WHERE asset_id = $1
               AND cleared_at IS NULL
            ",
        )
        .bind(asset_id)
        .bind(cleared_by)
        .bind(reason)
        .bind(cleared_at)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(cleared.rows_affected() > 0)
    }

    async fn find_open_rail_stop(
        &self,
        asset_id: Uuid,
    ) -> Result<Option<RailStop>, RepositoryError> {
        let row = sqlx::query_as::<_, RailStopRow>(
            r"
            SELECT id, asset_id, reason_code, detail, opened_by, opened_at
              FROM rail_stops
             WHERE asset_id = $1 AND cleared_at IS NULL
             LIMIT 1
            ",
        )
        .bind(asset_id)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(row.map(Into::into))
    }

    async fn record_risk_evaluation(
        &self,
        submission: &RiskSubmission,
        submitted_by: Uuid,
    ) -> Result<Uuid, RepositoryError> {
        let id = Uuid::now_v7();
        sqlx::query(
            r"
            INSERT INTO payment_risk_evaluations (
                id, transfer_id, provider, decision, score, reasons, evaluated_at, submitted_by
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (transfer_id, provider, evaluated_at) DO NOTHING
            ",
        )
        .bind(id)
        .bind(submission.transfer_id)
        .bind(&submission.provider)
        .bind(submission.decision.as_str())
        .bind(submission.score)
        .bind(&submission.reasons)
        .bind(submission.evaluated_at)
        .bind(submitted_by)
        .execute(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(id)
    }
}

/// The evidence summary stored on the snapshot itself, so a snapshot answers
/// "which sources, at which moment" without a join.
fn sources_json(ingestion: &PriceIngestion, used: &[usize]) -> Result<Value, RepositoryError> {
    let mut entries = Vec::with_capacity(used.len());
    for index in used {
        let reading = ingestion
            .readings
            .get(*index)
            .ok_or_else(|| corrupt("an aggregated price named a reading that was not submitted"))?;
        entries.push(json!({
            "source_key": reading.source_key,
            "provider_group": reading.provider_group,
            "rate_numerator": reading.rate_numerator.to_string(),
            "rate_denominator": reading.rate_denominator.to_string(),
            "observed_at": reading.observed_at.unix_timestamp(),
        }));
    }
    Ok(Value::Array(entries))
}

/// Stores one reading exactly once.
///
/// A resubmitted reading is the same claim from the same source at the same
/// moment, so the conflict is genuine idempotency rather than a lost record.
async fn insert_reading(
    transaction: &mut Transaction<'_, Postgres>,
    ingestion: &PriceIngestion,
    reading: &gateway_domain::PriceReading,
    snapshot_id: Option<Uuid>,
    discard_reason: Option<&str>,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO price_readings (
            id, asset_id, fiat_currency, source_key, provider_group,
            rate_numerator, rate_denominator, observed_at, received_at,
            ingested_by, snapshot_id, discard_reason
        ) VALUES (
            $1, $2, $3, $4, $5,
            CAST($6 AS NUMERIC), CAST($7 AS NUMERIC), $8, $9,
            $10, $11, $12
        )
        ON CONFLICT (asset_id, fiat_currency, source_key, observed_at) DO NOTHING
        ",
    )
    .bind(Uuid::now_v7())
    .bind(ingestion.asset_id)
    .bind(ingestion.currency.as_str())
    .bind(&reading.source_key)
    .bind(&reading.provider_group)
    .bind(reading.rate_numerator.to_string())
    .bind(reading.rate_denominator.to_string())
    .bind(reading.observed_at)
    .bind(ingestion.received_at)
    .bind(ingestion.ingested_by)
    .bind(snapshot_id)
    .bind(discard_reason)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

const fn health_text(health: RailHealth) -> &'static str {
    match health {
        RailHealth::Healthy => "healthy",
        RailHealth::Degraded => "degraded",
        RailHealth::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod tests;
