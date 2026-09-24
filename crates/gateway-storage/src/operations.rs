//! Storage for the operator surface.
//!
//! One submission is one transaction: the snapshot, if there is one, and every
//! reading behind it with its outcome. A refused submission still writes its
//! readings, because the disagreement is the record an operator needs.

use async_trait::async_trait;
use gateway_application::{
    ManualResolution, ManualResolutionResult, OperationsError, OperationsRepository,
    OperatorCredential, OperatorScope, PriceIngestion, PriceOutcome, RailStop, RecordedPrice,
    RepositoryError, RiskSubmission, SettlementCommand, SettlementRecord,
};
use gateway_domain::{
    CurrencyCode, ManualResolutionAction, MatchStrategy, PriceAggregationPolicy, RailHealth,
    RawAmount, RiskDecision, SettlementOutcome, TransferState,
};
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

    async fn risk_provider_allowed(
        &self,
        operator_key_id: Uuid,
        provider: &str,
    ) -> Result<bool, RepositoryError> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM operator_risk_provider_bindings WHERE operator_key_id=$1 AND provider=$2 AND disabled_at IS NULL)",
        )
        .bind(operator_key_id)
        .bind(provider)
        .fetch_one(self.pool())
        .await
        .map_err(unavailable)
    }

    async fn resolve_manual(
        &self,
        credential: &OperatorCredential,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        resolution: &ManualResolution,
        decided_at: OffsetDateTime,
    ) -> Result<ManualResolutionResult, OperationsError> {
        let mut tx = self.pool().begin().await.map_err(unavailable)?;
        // Serialize commands in the same operator/idempotency scope before
        // inspecting business state. A concurrent replay must observe the
        // first committed result instead of racing it for the transfer row.
        let lock_scope = format!("{}:{idempotency_key}", credential.key_id);
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_scope)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM manual_resolution_requests WHERE operator_key_id=$1 AND idempotency_key=$2)",
        )
        .bind(credential.key_id)
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        if exists {
            let replay = load_resolution(
                &mut tx,
                credential.key_id,
                idempotency_key,
                request_hash,
                true,
            )
            .await?;
            tx.commit().await.map_err(unavailable)?;
            return Ok(replay);
        }
        let result = match resolution.action {
            ManualResolutionAction::Honor => {
                honor_transfer(
                    &mut tx,
                    credential,
                    idempotency_key,
                    request_hash,
                    resolution,
                    decided_at,
                )
                .await?
            }
            ManualResolutionAction::Reject => {
                reject_transfer(
                    &mut tx,
                    credential,
                    idempotency_key,
                    request_hash,
                    resolution,
                    decided_at,
                )
                .await?
            }
            ManualResolutionAction::RecordRemainderDisposition => {
                record_external_disposition(
                    &mut tx,
                    credential,
                    idempotency_key,
                    request_hash,
                    resolution,
                    decided_at,
                )
                .await?
            }
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }
}

#[derive(Debug, FromRow)]
struct ManualHonorRow {
    merchant_id: Uuid,
    fiat_amount_minor: i64,
    expected_raw: String,
    attempt_allocated_raw: String,
    transfer_raw: String,
    transfer_allocated_raw: String,
    finality: String,
    processing_state: String,
}

#[derive(Debug, FromRow)]
struct StoredResolutionRow {
    id: Uuid,
    action: String,
    transfer_id: Uuid,
    payment_intent_id: Option<Uuid>,
    attempt_id: Option<Uuid>,
    merchant_id: Option<Uuid>,
    allocated_raw: Option<String>,
    remainder_raw: Option<String>,
    request_hash: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
async fn honor_transfer(
    tx: &mut Transaction<'_, Postgres>,
    credential: &OperatorCredential,
    key: &str,
    hash: &[u8; 32],
    resolution: &ManualResolution,
    now: OffsetDateTime,
) -> Result<ManualResolutionResult, OperationsError> {
    let intent_id = resolution
        .payment_intent_id
        .ok_or(OperationsError::InvalidManualResolution)?;
    let attempt_id = resolution
        .attempt_id
        .ok_or(OperationsError::InvalidManualResolution)?;
    let allocate = resolution
        .allocate_raw
        .ok_or(OperationsError::InvalidManualResolution)?;
    let row = sqlx::query_as::<_, ManualHonorRow>(
        r"SELECT i.merchant_id, i.amount_minor AS fiat_amount_minor,
                  a.expected_amount_raw::text AS expected_raw,
                  COALESCE((SELECT sum(pa.allocated_raw) FROM payment_allocations pa WHERE pa.attempt_id=a.id),0)::text AS attempt_allocated_raw,
                  t.amount_raw::text AS transfer_raw,
                  p.allocated_raw::text AS transfer_allocated_raw,
                  s.state AS finality, p.processing_state
             FROM payment_attempts a
             JOIN payment_intents i ON i.id=a.payment_intent_id AND i.merchant_id=a.merchant_id
             JOIN chain_transfers t ON t.id=$1
             JOIN chain_transfer_state_current s ON s.transfer_id=t.id
             JOIN chain_transfer_processing p ON p.transfer_id=t.id
            WHERE a.id=$2 AND i.id=$3
            FOR UPDATE OF a,i,t,p",
    )
    .bind(resolution.transfer_id)
    .bind(attempt_id)
    .bind(intent_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(unavailable)?
    .ok_or(OperationsError::ManualResolutionNotFound)?;
    if row.finality != "finalized" || !matches!(row.processing_state.as_str(), "held" | "unmatched")
    {
        return Err(OperationsError::ManualResolutionConflict);
    }
    let expected = parse_raw(&row.expected_raw)?;
    let attempt_allocated = parse_raw_allow_zero(&row.attempt_allocated_raw)?;
    let transfer_amount = parse_raw(&row.transfer_raw)?;
    let transfer_allocated = parse_raw_allow_zero(&row.transfer_allocated_raw)?;
    let outstanding = expected
        .as_u256()
        .checked_sub(attempt_allocated.as_u256())
        .ok_or(OperationsError::ManualResolutionConflict)?;
    let available = transfer_amount
        .as_u256()
        .checked_sub(transfer_allocated.as_u256())
        .ok_or(OperationsError::ManualResolutionConflict)?;
    let required_allocation = outstanding.min(available);
    if allocate.as_u256() != required_allocation {
        return Err(OperationsError::ManualResolutionConflict);
    }
    let remainder_u256 = available
        .checked_sub(allocate.as_u256())
        .ok_or(OperationsError::ManualResolutionConflict)?;
    let completes = allocate.as_u256() == outstanding;
    let remainder = if remainder_u256.is_zero() {
        RawAmount::ZERO
    } else {
        RawAmount::positive(remainder_u256)
            .map_err(|_| OperationsError::ManualResolutionConflict)?
    };
    let (outcome, outcome_name) = if completes && !remainder.is_zero() {
        (
            SettlementOutcome::Overpaid {
                allocate_raw: allocate,
                remainder_raw: remainder,
            },
            "overpaid",
        )
    } else if completes {
        (
            SettlementOutcome::Settle {
                allocate_raw: allocate,
            },
            "settled",
        )
    } else {
        (
            SettlementOutcome::Partial {
                allocate_raw: allocate,
            },
            "partial",
        )
    };
    if let Some(replay) = insert_resolution(
        tx,
        credential,
        key,
        hash,
        resolution,
        Some(row.merchant_id),
        Some(allocate),
        if remainder.is_zero() {
            None
        } else {
            Some(remainder)
        },
        now,
    )
    .await?
    {
        return Ok(replay);
    }
    let command = SettlementCommand {
        transfer_id: resolution.transfer_id,
        attempt_id,
        payment_intent_id: intent_id,
        merchant_id: row.merchant_id,
        fiat_amount_minor: row.fiat_amount_minor,
        match_strategy: MatchStrategy::Manual,
        outcome,
        policy_version: "manual/operator".to_owned(),
        independent_groups: 0,
        had_own_node: false,
        finality_state: TransferState::Finalized,
        risk: RiskDecision::Review,
        risk_evaluation_id: None,
        attestation_ids: Vec::new(),
    };
    let record = crate::settlement::manual_allocate_and_finish(
        tx,
        &command,
        allocate,
        remainder,
        outcome_name,
        &credential.label,
        &resolution.reason,
        now,
    )
    .await?;
    if matches!(
        record,
        SettlementRecord::ForeignClaim | SettlementRecord::AlreadyProcessed
    ) {
        return Err(OperationsError::ManualResolutionConflict);
    }
    record_admin_audit(tx, credential, resolution, intent_id, now).await?;
    load_resolution(tx, credential.key_id, key, hash, false).await
}

async fn reject_transfer(
    tx: &mut Transaction<'_, Postgres>,
    credential: &OperatorCredential,
    key: &str,
    hash: &[u8; 32],
    resolution: &ManualResolution,
    now: OffsetDateTime,
) -> Result<ManualResolutionResult, OperationsError> {
    let state = sqlx::query_as::<_,(String,String)>("SELECT s.state,p.processing_state FROM chain_transfer_state_current s JOIN chain_transfer_processing p ON p.transfer_id=s.transfer_id WHERE s.transfer_id=$1 FOR UPDATE OF p")
        .bind(resolution.transfer_id).fetch_optional(&mut **tx).await.map_err(unavailable)?
        .ok_or(OperationsError::ManualResolutionNotFound)?;
    if state.0 != "finalized" || !matches!(state.1.as_str(), "held" | "unmatched") {
        return Err(OperationsError::ManualResolutionConflict);
    }
    let has_allocation = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM payment_allocations WHERE transfer_id=$1)",
    )
    .bind(resolution.transfer_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(unavailable)?;
    if has_allocation {
        return Err(OperationsError::ManualResolutionConflict);
    }
    if let Some(replay) =
        insert_resolution(tx, credential, key, hash, resolution, None, None, None, now).await?
    {
        return Ok(replay);
    }
    sqlx::query("UPDATE chain_transfer_processing SET processing_state='resolved',last_error=$2,version=version+1,updated_at=$3 WHERE transfer_id=$1")
        .bind(resolution.transfer_id).bind(&resolution.reason).bind(now).execute(&mut **tx).await.map_err(unavailable)?;
    record_admin_audit(tx, credential, resolution, resolution.transfer_id, now).await?;
    load_resolution(tx, credential.key_id, key, hash, false).await
}

async fn record_external_disposition(
    tx: &mut Transaction<'_, Postgres>,
    credential: &OperatorCredential,
    key: &str,
    hash: &[u8; 32],
    resolution: &ManualResolution,
    now: OffsetDateTime,
) -> Result<ManualResolutionResult, OperationsError> {
    let intent_id = resolution
        .payment_intent_id
        .ok_or(OperationsError::InvalidManualResolution)?;
    let remainder = resolution
        .remainder_raw
        .ok_or(OperationsError::InvalidManualResolution)?;
    let disposition = resolution
        .disposition
        .ok_or(OperationsError::InvalidManualResolution)?;
    let merchant=sqlx::query_as::<_,(Uuid,String)>("SELECT merchant_id,remainder_raw::text FROM payment_settlement_decisions WHERE transfer_id=$1 AND payment_intent_id=$2 AND outcome='overpaid' FOR UPDATE")
        .bind(resolution.transfer_id).bind(intent_id).fetch_optional(&mut **tx).await.map_err(unavailable)?
        .ok_or(OperationsError::ManualResolutionNotFound)?;
    if parse_raw(&merchant.1)? != remainder {
        return Err(OperationsError::ManualResolutionConflict);
    }
    if let Some(replay) = insert_resolution(
        tx,
        credential,
        key,
        hash,
        resolution,
        Some(merchant.0),
        None,
        Some(remainder),
        now,
    )
    .await?
    {
        return Ok(replay);
    }
    let resolution_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM manual_resolution_requests WHERE operator_key_id=$1 AND idempotency_key=$2",
    )
    .bind(credential.key_id)
    .bind(key)
    .fetch_one(&mut **tx)
    .await
    .map_err(unavailable)?;
    sqlx::query("INSERT INTO overpayment_remainder_dispositions(id,resolution_id,transfer_id,payment_intent_id,merchant_id,remainder_raw,disposition,external_reference,recorded_by,reason,recorded_at) VALUES($1,$2,$3,$4,$5,CAST($6 AS NUMERIC),$7,$8,$9,$10,$11)")
        .bind(Uuid::now_v7())
        .bind(resolution_id)
        .bind(resolution.transfer_id)
        .bind(intent_id)
        .bind(merchant.0)
        .bind(remainder.to_string())
        .bind(disposition.as_str())
        .bind(&resolution.external_reference)
        .bind(credential.key_id)
        .bind(&resolution.reason)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
            {
                OperationsError::ManualResolutionConflict
            } else {
                OperationsError::Repository(unavailable(error))
            }
        })?;
    let closed = sqlx::query(
        "UPDATE chain_transfer_processing SET processing_state='resolved', last_error=NULL, version=version+1, updated_at=$2 WHERE transfer_id=$1 AND processing_state='matched'",
    )
    .bind(resolution.transfer_id)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(unavailable)?;
    if closed.rows_affected() != 1 {
        return Err(OperationsError::ManualResolutionConflict);
    }
    record_admin_audit(tx, credential, resolution, intent_id, now).await?;
    load_resolution(tx, credential.key_id, key, hash, false).await
}

#[allow(clippy::too_many_arguments)]
async fn insert_resolution(
    tx: &mut Transaction<'_, Postgres>,
    credential: &OperatorCredential,
    key: &str,
    hash: &[u8; 32],
    resolution: &ManualResolution,
    merchant: Option<Uuid>,
    allocated: Option<RawAmount>,
    remainder: Option<RawAmount>,
    now: OffsetDateTime,
) -> Result<Option<ManualResolutionResult>, OperationsError> {
    let inserted=sqlx::query("INSERT INTO manual_resolution_requests(id,operator_key_id,idempotency_key,request_hash,action,transfer_id,payment_intent_id,attempt_id,merchant_id,allocated_raw,remainder_raw,disposition,external_reference,reason,actor_label,result_status,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,CAST($10 AS NUMERIC),CAST($11 AS NUMERIC),$12,$13,$14,$15,'completed',$16) ON CONFLICT(operator_key_id,idempotency_key) DO NOTHING")
        .bind(Uuid::now_v7()).bind(credential.key_id).bind(key).bind(hash.as_slice()).bind(resolution.action.as_str()).bind(resolution.transfer_id).bind(resolution.payment_intent_id).bind(resolution.attempt_id).bind(merchant).bind(allocated.map(|v|v.to_string())).bind(remainder.map(|v|v.to_string())).bind(resolution.disposition.map(gateway_domain::RemainderDisposition::as_str)).bind(&resolution.external_reference).bind(&resolution.reason).bind(&credential.label).bind(now).execute(&mut **tx).await.map_err(unavailable)?;
    if inserted.rows_affected() == 0 {
        return Ok(Some(
            load_resolution(tx, credential.key_id, key, hash, true).await?,
        ));
    }
    Ok(None)
}

async fn load_resolution(
    tx: &mut Transaction<'_, Postgres>,
    operator: Uuid,
    key: &str,
    hash: &[u8; 32],
    replayed: bool,
) -> Result<ManualResolutionResult, OperationsError> {
    let r=sqlx::query_as::<_,StoredResolutionRow>("SELECT id,action,transfer_id,payment_intent_id,attempt_id,merchant_id,allocated_raw::text,remainder_raw::text,request_hash FROM manual_resolution_requests WHERE operator_key_id=$1 AND idempotency_key=$2").bind(operator).bind(key).fetch_one(&mut **tx).await.map_err(unavailable)?;
    if r.request_hash.as_slice() != hash {
        return Err(OperationsError::Repository(
            RepositoryError::IdempotencyConflict,
        ));
    }
    let action = match r.action.as_str() {
        "honor" => ManualResolutionAction::Honor,
        "reject" => ManualResolutionAction::Reject,
        "record_remainder_disposition" => ManualResolutionAction::RecordRemainderDisposition,
        _ => {
            return Err(OperationsError::Repository(corrupt(
                "unknown manual resolution action",
            )));
        }
    };
    Ok(ManualResolutionResult {
        id: r.id,
        action,
        transfer_id: r.transfer_id,
        payment_intent_id: r.payment_intent_id,
        attempt_id: r.attempt_id,
        merchant_id: r.merchant_id,
        allocated_raw: r.allocated_raw.as_deref().map(parse_raw).transpose()?,
        remainder_raw: r.remainder_raw.as_deref().map(parse_raw).transpose()?,
        replayed,
    })
}

async fn record_admin_audit(
    tx: &mut Transaction<'_, Postgres>,
    credential: &OperatorCredential,
    resolution: &ManualResolution,
    resource_id: Uuid,
    now: OffsetDateTime,
) -> Result<(), OperationsError> {
    sqlx::query("INSERT INTO audit_events(id,merchant_id,actor_type,actor_id,action,resource_type,resource_id,reason,payload,created_at) VALUES($1,NULL,'operator',$2,$3,'manual_resolution',$4,$5,$6,$7)")
        .bind(Uuid::now_v7()).bind(credential.key_id).bind(resolution.action.as_str()).bind(resource_id).bind(&resolution.reason).bind(json!({"transfer_id":resolution.transfer_id,"external_reference":resolution.external_reference})).bind(now).execute(&mut **tx).await.map_err(unavailable)?;
    Ok(())
}

fn parse_raw(value: &str) -> Result<RawAmount, OperationsError> {
    value
        .parse::<RawAmount>()
        .map_err(|e| OperationsError::Repository(corrupt(e.to_string())))
}
fn parse_raw_allow_zero(value: &str) -> Result<RawAmount, OperationsError> {
    if value.trim_start_matches('0').is_empty() {
        Ok(RawAmount::ZERO)
    } else {
        parse_raw(value)
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
