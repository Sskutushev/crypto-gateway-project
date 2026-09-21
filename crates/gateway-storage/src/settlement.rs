use async_trait::async_trait;
use gateway_application::{
    AttemptSnapshot, ComponentLease, PendingTransfer, RepositoryError, SettlementCommand,
    SettlementRecord, SettlementRepository, UnresolvedTransfer,
};
use gateway_domain::{
    AttemptCandidate, AttemptStatus, CurrencyCode, Memo, RawAmount, RiskDecision,
    SettlementOutcome, SettlementPolicy, SettlementTier, TransferFacts, TransferState,
};
use serde_json::json;
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

const SETTLEMENT_ACTOR: &str = "system";

#[derive(Debug, FromRow)]
struct PendingTransferRow {
    transfer_id: Uuid,
    collector_address_id: Uuid,
    amount_raw: String,
    memo: Option<String>,
    block_time: OffsetDateTime,
    state: String,
    allocated_raw: String,
    independent_groups: i64,
    had_own_node: bool,
    attestation_ids: Vec<Uuid>,
}

impl TryFrom<PendingTransferRow> for PendingTransfer {
    type Error = RepositoryError;

    fn try_from(row: PendingTransferRow) -> Result<Self, Self::Error> {
        Ok(Self {
            facts: TransferFacts {
                transfer_id: row.transfer_id,
                collector_address_id: row.collector_address_id,
                amount_raw: row
                    .amount_raw
                    .parse::<RawAmount>()
                    .map_err(|error| corrupt(error.to_string()))?,
                memo: row
                    .memo
                    .as_deref()
                    .map(Memo::new)
                    .transpose()
                    .map_err(|error| corrupt(error.to_string()))?,
                block_time: row.block_time,
                state: row
                    .state
                    .parse::<TransferState>()
                    .map_err(|error| corrupt(error.to_string()))?,
            },
            independent_groups: u32::try_from(row.independent_groups).unwrap_or(u32::MAX),
            had_own_node: row.had_own_node,
            attestation_ids: row.attestation_ids,
            allocated_raw: parse_total(&row.allocated_raw)?,
        })
    }
}

#[derive(Debug, FromRow)]
struct CandidateRow {
    attempt_id: Uuid,
    payment_intent_id: Uuid,
    merchant_id: Uuid,
    collector_address_id: Uuid,
    expected_amount_raw: String,
    memo_reference: Option<String>,
    leased_from: OffsetDateTime,
    leased_until: OffsetDateTime,
    status: String,
}

impl TryFrom<CandidateRow> for AttemptCandidate {
    type Error = RepositoryError;

    fn try_from(row: CandidateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            attempt_id: row.attempt_id,
            payment_intent_id: row.payment_intent_id,
            merchant_id: row.merchant_id,
            collector_address_id: row.collector_address_id,
            expected_amount_raw: row
                .expected_amount_raw
                .parse::<RawAmount>()
                .map_err(|error| corrupt(error.to_string()))?,
            memo_reference: row
                .memo_reference
                .as_deref()
                .map(Memo::new)
                .transpose()
                .map_err(|error| corrupt(error.to_string()))?,
            leased_from: row.leased_from,
            leased_until: row.leased_until,
            status: AttemptStatus::parse(&row.status)
                .map_err(|error| corrupt(error.to_string()))?,
        })
    }
}

#[derive(Debug, FromRow)]
struct AttemptSnapshotRow {
    attempt_id: Uuid,
    payment_intent_id: Uuid,
    merchant_id: Uuid,
    currency: String,
    fiat_amount_minor: i64,
    expected_amount_raw: String,
    allocated_raw: String,
    status: String,
}

impl TryFrom<AttemptSnapshotRow> for AttemptSnapshot {
    type Error = RepositoryError;

    fn try_from(row: AttemptSnapshotRow) -> Result<Self, Self::Error> {
        Ok(Self {
            attempt_id: row.attempt_id,
            payment_intent_id: row.payment_intent_id,
            merchant_id: row.merchant_id,
            currency: CurrencyCode::new(row.currency)
                .map_err(|error| corrupt(error.to_string()))?,
            fiat_amount_minor: row.fiat_amount_minor,
            expected_amount_raw: row
                .expected_amount_raw
                .parse::<RawAmount>()
                .map_err(|error| corrupt(error.to_string()))?,
            allocated_raw: parse_total(&row.allocated_raw)?,
            status: AttemptStatus::parse(&row.status)
                .map_err(|error| corrupt(error.to_string()))?,
        })
    }
}

/// Parses a running total, which may legitimately be zero.
fn parse_total(value: &str) -> Result<RawAmount, RepositoryError> {
    if value.trim_start_matches('0').is_empty() {
        return Ok(RawAmount::ZERO);
    }
    value
        .parse::<RawAmount>()
        .map_err(|error| corrupt(error.to_string()))
}

#[async_trait]
impl SettlementRepository for PostgresRepository {
    async fn transfers_awaiting_settlement(
        &self,
        limit: u32,
    ) -> Result<Vec<PendingTransfer>, RepositoryError> {
        let rows = sqlx::query_as::<_, PendingTransferRow>(
            r"
            SELECT transfer.id AS transfer_id,
                   transfer.collector_address_id,
                   transfer.amount_raw::TEXT AS amount_raw,
                   transfer.memo,
                   transfer.block_time,
                   state.state,
                   processing.allocated_raw::TEXT AS allocated_raw,
                   count(DISTINCT attestation.provider_group) AS independent_groups,
                   COALESCE(bool_or(attestation.source_kind = 'own_node'), FALSE) AS had_own_node,
                   COALESCE(array_agg(attestation.id) FILTER (WHERE attestation.id IS NOT NULL),
                            ARRAY[]::UUID[]) AS attestation_ids
              FROM chain_transfers AS transfer
              JOIN chain_transfer_state_current AS state ON state.transfer_id = transfer.id
              JOIN chain_transfer_processing AS processing ON processing.transfer_id = transfer.id
              LEFT JOIN chain_transfer_attestations AS attestation
                     ON attestation.transfer_id = transfer.id
             WHERE processing.processing_state = 'pending'
               AND state.state IN ('finalized', 'invalidated')
             GROUP BY transfer.id, state.state, processing.allocated_raw
             ORDER BY transfer.block_time, transfer.id
             LIMIT $1
            ",
        )
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn match_candidates(
        &self,
        transfer: &TransferFacts,
    ) -> Result<Vec<AttemptCandidate>, RepositoryError> {
        // Live reservations plus archived ones whose window covered the block:
        // a slot released after the late-payment window may already belong to
        // someone else today.
        let rows = sqlx::query_as::<_, CandidateRow>(
            r"
            SELECT attempt.id AS attempt_id,
                   attempt.payment_intent_id,
                   attempt.merchant_id,
                   attempt.collector_address_id,
                   attempt.expected_amount_raw::TEXT AS expected_amount_raw,
                   attempt.memo_reference,
                   lease.leased_from,
                   lease.lease_until AS leased_until,
                   attempt.status
              FROM amount_leases AS lease
              JOIN payment_attempts AS attempt ON attempt.id = lease.attempt_id
             WHERE lease.collector_address_id = $1
               AND (lease.amount_raw = CAST($2 AS NUMERIC)
                    OR ($3::TEXT IS NOT NULL AND attempt.memo_reference = $3))
            UNION ALL
            SELECT attempt.id AS attempt_id,
                   attempt.payment_intent_id,
                   attempt.merchant_id,
                   attempt.collector_address_id,
                   attempt.expected_amount_raw::TEXT AS expected_amount_raw,
                   attempt.memo_reference,
                   history.leased_from,
                   history.leased_until,
                   attempt.status
              FROM amount_lease_history AS history
              JOIN payment_attempts AS attempt ON attempt.id = history.attempt_id
             WHERE history.collector_address_id = $1
               AND history.amount_raw = CAST($2 AS NUMERIC)
               AND $4 >= history.leased_from
               AND $4 < history.leased_until
            ",
        )
        .bind(transfer.collector_address_id)
        .bind(transfer.amount_raw.to_string())
        .bind(transfer.memo.as_ref().map(Memo::as_str))
        .bind(transfer.block_time)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn attempt_snapshot(
        &self,
        attempt_id: Uuid,
    ) -> Result<Option<AttemptSnapshot>, RepositoryError> {
        let row = sqlx::query_as::<_, AttemptSnapshotRow>(
            r"
            SELECT attempt.id AS attempt_id,
                   attempt.payment_intent_id,
                   attempt.merchant_id,
                   intent.currency,
                   intent.amount_minor AS fiat_amount_minor,
                   attempt.expected_amount_raw::TEXT AS expected_amount_raw,
                   COALESCE(
                       (SELECT sum(allocation.allocated_raw)
                          FROM payment_allocations AS allocation
                         WHERE allocation.attempt_id = attempt.id),
                       0
                   )::TEXT AS allocated_raw,
                   attempt.status
              FROM payment_attempts AS attempt
              JOIN payment_intents AS intent ON intent.id = attempt.payment_intent_id
             WHERE attempt.id = $1
            ",
        )
        .bind(attempt_id)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        row.map(TryInto::try_into).transpose()
    }

    async fn find_settlement_policy(
        &self,
        currency: &CurrencyCode,
    ) -> Result<Option<SettlementPolicy>, RepositoryError> {
        let Some((policy_id, version)) = sqlx::query_as::<_, (Uuid, String)>(
            r"
            SELECT id, version
              FROM payment_settlement_policies
             WHERE fiat_currency = $1 AND status = 'active'
            ",
        )
        .bind(currency.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?
        else {
            return Ok(None);
        };

        let tiers = sqlx::query_as::<_, (i64, i32, bool, bool, bool)>(
            r"
            SELECT max_fiat_minor, min_independent_groups, require_own_node,
                   require_risk_allow, auto_settle
              FROM payment_settlement_policy_tiers
             WHERE policy_id = $1
             ORDER BY max_fiat_minor
            ",
        )
        .bind(policy_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        if tiers.is_empty() {
            return Err(corrupt("an active settlement policy has no bands"));
        }

        Ok(Some(SettlementPolicy {
            id: policy_id,
            version,
            tiers: tiers
                .into_iter()
                .map(
                    |(
                        max_fiat_minor,
                        min_independent_groups,
                        require_own_node,
                        require_risk_allow,
                        auto_settle,
                    )| {
                        SettlementTier {
                            max_fiat_minor,
                            min_independent_groups: u32::try_from(min_independent_groups)
                                .unwrap_or(u32::MAX),
                            require_own_node,
                            require_risk_allow,
                            auto_settle,
                        }
                    },
                )
                .collect(),
        }))
    }

    async fn latest_risk(
        &self,
        transfer_id: Uuid,
    ) -> Result<(RiskDecision, Option<Uuid>), RepositoryError> {
        let row = sqlx::query_as::<_, (Uuid, String)>(
            r"
            SELECT id, decision
              FROM payment_risk_evaluations
             WHERE transfer_id = $1
             ORDER BY evaluated_at DESC
             LIMIT 1
            ",
        )
        .bind(transfer_id)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        // An absent screening is recorded as skipped. It is never an allow.
        let Some((id, decision)) = row else {
            return Ok((RiskDecision::Skipped, None));
        };
        Ok((
            RiskDecision::parse(&decision).map_err(|error| corrupt(error.to_string()))?,
            Some(id),
        ))
    }

    async fn settle(
        &self,
        lease: &ComponentLease,
        command: &SettlementCommand,
    ) -> Result<SettlementRecord, RepositoryError> {
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;
        hold_lease(&mut transaction, lease).await?;

        let processing = sqlx::query_as::<_, (String, String)>(
            r"
            SELECT processing.processing_state, transfer.amount_raw::TEXT
              FROM chain_transfer_processing AS processing
              JOIN chain_transfers AS transfer ON transfer.id = processing.transfer_id
             WHERE processing.transfer_id = $1
             FOR UPDATE OF processing
            ",
        )
        .bind(command.transfer_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| corrupt("a canonical transfer has no processing row"))?;

        if processing.0 != "pending" {
            transaction.commit().await.map_err(unavailable)?;
            return Ok(SettlementRecord::AlreadyProcessed);
        }

        let now = OffsetDateTime::now_utc();
        let record = match &command.outcome {
            SettlementOutcome::Hold { reason } => {
                finish_without_money(
                    &mut transaction,
                    command,
                    "held",
                    "held",
                    reason.as_str(),
                    now,
                )
                .await?;
                SettlementRecord::Held
            }
            SettlementOutcome::ManualRequired { reason } => {
                finish_without_money(
                    &mut transaction,
                    command,
                    "held",
                    "manual_required",
                    reason.as_str(),
                    now,
                )
                .await?;
                SettlementRecord::ManualRequired
            }
            SettlementOutcome::Settle { allocate_raw } => {
                allocate_and_finish(
                    &mut transaction,
                    command,
                    *allocate_raw,
                    RawAmount::ZERO,
                    "settled",
                    now,
                )
                .await?
            }
            SettlementOutcome::Partial { allocate_raw } => {
                allocate_and_finish(
                    &mut transaction,
                    command,
                    *allocate_raw,
                    RawAmount::ZERO,
                    "partial",
                    now,
                )
                .await?
            }
            SettlementOutcome::Overpaid {
                allocate_raw,
                remainder_raw,
            } => {
                allocate_and_finish(
                    &mut transaction,
                    command,
                    *allocate_raw,
                    *remainder_raw,
                    "overpaid",
                    now,
                )
                .await?
            }
        };

        transaction.commit().await.map_err(unavailable)?;
        Ok(record)
    }

    async fn record_unresolved(
        &self,
        lease: &ComponentLease,
        transfer: &PendingTransfer,
        unresolved: &UnresolvedTransfer,
    ) -> Result<(), RepositoryError> {
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;
        hold_lease(&mut transaction, lease).await?;
        let now = OffsetDateTime::now_utc();

        sqlx::query(
            r"
            UPDATE chain_transfer_processing
               SET processing_state = 'unmatched',
                   last_error = $2,
                   version = version + 1,
                   updated_at = $3
             WHERE transfer_id = $1 AND processing_state = 'pending'
            ",
        )
        .bind(transfer.facts.transfer_id)
        .bind(unresolved.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;

        let (event_type, payload) = match unresolved {
            UnresolvedTransfer::Ambiguous { attempt_ids } => (
                "AMBIGUOUS_MATCH",
                json!({
                    "transfer_id": transfer.facts.transfer_id,
                    "amount_raw": transfer.facts.amount_raw.to_string(),
                    "attempt_ids": attempt_ids,
                }),
            ),
            UnresolvedTransfer::Unmatched => (
                "UNMATCHED_INBOUND",
                json!({
                    "transfer_id": transfer.facts.transfer_id,
                    "amount_raw": transfer.facts.amount_raw.to_string(),
                    "block_time": transfer.facts.block_time,
                }),
            ),
        };

        insert_payment_event(
            &mut transaction,
            None,
            None,
            None,
            Some(transfer.facts.transfer_id),
            event_type,
            Some(unresolved.as_str()),
            payload.clone(),
            now,
        )
        .await?;
        // Money that nobody can explain is an operator event, not a merchant
        // event: it is queued for a person instead of being announced.
        enqueue_outbox(
            &mut transaction,
            None,
            "operator",
            event_type,
            "chain_transfer",
            transfer.facts.transfer_id,
            payload,
            now,
        )
        .await?;

        transaction.commit().await.map_err(unavailable)?;
        Ok(())
    }
}

async fn hold_lease(
    transaction: &mut Transaction<'_, Postgres>,
    lease: &ComponentLease,
) -> Result<(), RepositoryError> {
    let held = sqlx::query_scalar::<_, i32>(
        r"
        SELECT 1
          FROM component_leases
         WHERE component = $1 AND holder = $2 AND fence_token = $3 AND lease_until > now()
         FOR SHARE
        ",
    )
    .bind(&lease.component)
    .bind(&lease.holder)
    .bind(lease.fence_token)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;

    if held.is_none() {
        return Err(RepositoryError::LeaseLost);
    }
    Ok(())
}

/// Records a decision that moves no money, and parks the transfer.
async fn finish_without_money(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    processing_state: &str,
    outcome: &str,
    reason: &str,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        UPDATE chain_transfer_processing
           SET processing_state = $2, last_error = $3, version = version + 1, updated_at = $4
         WHERE transfer_id = $1
        ",
    )
    .bind(command.transfer_id)
    .bind(processing_state)
    .bind(reason)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    record_decision(
        transaction,
        command,
        outcome,
        reason,
        RawAmount::ZERO,
        RawAmount::ZERO,
        now,
    )
    .await?;
    insert_payment_event(
        transaction,
        Some(command.merchant_id),
        Some(command.payment_intent_id),
        Some(command.attempt_id),
        Some(command.transfer_id),
        if outcome == "manual_required" {
            "MANUAL_REQUIRED"
        } else {
            "RISK_HOLD"
        },
        Some(reason),
        json!({"reason": reason, "policy": command.policy_version}),
        now,
    )
    .await?;
    Ok(())
}

/// Claims the transfer for one payment intent, allocates money under the
/// database's own ceiling, and records everything that follows.
async fn allocate_and_finish(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    allocate_raw: RawAmount,
    remainder_raw: RawAmount,
    outcome: &str,
    now: OffsetDateTime,
) -> Result<SettlementRecord, RepositoryError> {
    if !claim_transfer(transaction, command, now).await? {
        return Ok(SettlementRecord::ForeignClaim);
    }

    // The allocated total can never exceed the transfer. Zero updated rows is
    // a refusal, not a free action.
    let allocated = sqlx::query_scalar::<_, String>(
        r"
        UPDATE chain_transfer_processing AS processing
           SET allocated_raw = processing.allocated_raw + CAST($2 AS NUMERIC),
               processing_state = $3,
               version = processing.version + 1,
               updated_at = $4
          FROM chain_transfers AS transfer
         WHERE processing.transfer_id = transfer.id
           AND processing.transfer_id = $1
           AND processing.allocated_raw + CAST($2 AS NUMERIC) <= transfer.amount_raw
        RETURNING processing.allocated_raw::TEXT
        ",
    )
    .bind(command.transfer_id)
    .bind(allocate_raw.to_string())
    .bind(if outcome == "settled" {
        "settled"
    } else {
        "matched"
    })
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;

    if allocated.is_none() {
        return Err(corrupt(
            "an allocation would have exceeded the transfer it belongs to",
        ));
    }

    let inserted = sqlx::query(
        r"
        INSERT INTO payment_allocations (
            id, attempt_id, payment_intent_id, merchant_id, transfer_id, allocated_raw,
            allocated_by, reason, created_at
        ) VALUES ($1, $2, $3, $4, $5, CAST($6 AS NUMERIC), $7, $8, $9)
        ON CONFLICT (attempt_id, transfer_id) DO NOTHING
        ",
    )
    .bind(Uuid::now_v7())
    .bind(command.attempt_id)
    .bind(command.payment_intent_id)
    .bind(command.merchant_id)
    .bind(command.transfer_id)
    .bind(allocate_raw.to_string())
    .bind(SETTLEMENT_ACTOR)
    .bind(match outcome {
        "settled" => "exact",
        "overpaid" => "overpay_remainder",
        _ => "partial",
    })
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    if inserted.rows_affected() == 0 {
        // The same transfer and attempt were already allocated: a replay.
        return Ok(SettlementRecord::AlreadyProcessed);
    }

    let record = match outcome {
        "settled" | "overpaid" => {
            advance_to_paid(transaction, command, now).await?;
            if outcome == "overpaid" {
                SettlementRecord::Overpaid
            } else {
                SettlementRecord::Settled
            }
        }
        _ => {
            advance_to_partially_paid(transaction, command, now).await?;
            SettlementRecord::PartiallyAllocated
        }
    };

    record_decision(
        transaction,
        command,
        outcome,
        command.match_strategy.as_str(),
        allocate_raw,
        remainder_raw,
        now,
    )
    .await?;

    if !remainder_raw.is_zero() {
        record_remainder(transaction, command, remainder_raw, now).await?;
    }

    Ok(record)
}

/// Records an overpayment remainder. It is never absorbed into a product, and
/// never quietly kept: a person decides what happens to it.
async fn record_remainder(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    remainder_raw: RawAmount,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    insert_payment_event(
        transaction,
        Some(command.merchant_id),
        Some(command.payment_intent_id),
        Some(command.attempt_id),
        Some(command.transfer_id),
        "OVERPAID",
        Some("overpay_remainder"),
        json!({"remainder_raw": remainder_raw.to_string()}),
        now,
    )
    .await?;
    enqueue_outbox(
        transaction,
        Some(command.merchant_id),
        "webhook",
        "OVERPAID",
        "payment_intent",
        command.payment_intent_id,
        json!({
            "payment_intent_id": command.payment_intent_id,
            "transfer_id": command.transfer_id,
            "remainder_raw": remainder_raw.to_string(),
        }),
        now,
    )
    .await?;
    Ok(())
}

/// Claims the transfer for one payment intent.
///
/// One transfer belongs to at most one intent: this is the lock that makes
/// "sixty to this order, forty to that one" impossible. Returns whether the
/// caller owns the claim.
async fn claim_transfer(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    now: OffsetDateTime,
) -> Result<bool, RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO chain_transfer_intent_claims (
            transfer_id, payment_intent_id, attempt_id, merchant_id, match_strategy, claimed_at
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (transfer_id) DO NOTHING
        ",
    )
    .bind(command.transfer_id)
    .bind(command.payment_intent_id)
    .bind(command.attempt_id)
    .bind(command.merchant_id)
    .bind(command.match_strategy.as_str())
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    let owner = sqlx::query_scalar::<_, Uuid>(
        "SELECT payment_intent_id FROM chain_transfer_intent_claims WHERE transfer_id = $1",
    )
    .bind(command.transfer_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(unavailable)?;

    if owner == command.payment_intent_id {
        return Ok(true);
    }

    sqlx::query(
        r"
        UPDATE chain_transfer_processing
           SET processing_state = 'held', last_error = 'claimed_by_another_intent',
               version = version + 1, updated_at = $2
         WHERE transfer_id = $1
        ",
    )
    .bind(command.transfer_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    record_decision(
        transaction,
        command,
        "rejected",
        "claimed_by_another_intent",
        RawAmount::ZERO,
        RawAmount::ZERO,
        now,
    )
    .await?;
    Ok(false)
}

/// Marks the obligation paid and takes the single claim to fulfil it.
async fn advance_to_paid(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        UPDATE payment_attempts
           SET status = 'settled', updated_at = $2
         WHERE id = $1 AND status IN ('awaiting_payment', 'expired')
        ",
    )
    .bind(command.attempt_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    sqlx::query(
        r"
        UPDATE payment_intents
           SET status = 'paid', version = version + 1, updated_at = $2
         WHERE id = $1 AND status <> 'paid'
        ",
    )
    .bind(command.payment_intent_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    // The fulfilment row is the claim. If it already exists, this is a replay
    // and the merchant must not be told twice.
    let claimed = sqlx::query(
        r"
        INSERT INTO payment_fulfillments (
            payment_intent_id, merchant_id, status, claimed_at, attempts
        ) VALUES ($1, $2, 'claimed', $3, 0)
        ON CONFLICT (payment_intent_id) DO NOTHING
        ",
    )
    .bind(command.payment_intent_id)
    .bind(command.merchant_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    insert_payment_event(
        transaction,
        Some(command.merchant_id),
        Some(command.payment_intent_id),
        Some(command.attempt_id),
        Some(command.transfer_id),
        "SETTLED",
        Some(command.match_strategy.as_str()),
        json!({
            "policy": command.policy_version,
            "independent_groups": command.independent_groups,
            "had_own_node": command.had_own_node,
            "risk": command.risk.as_str(),
        }),
        now,
    )
    .await?;

    if claimed.rows_affected() == 1 {
        enqueue_outbox(
            transaction,
            Some(command.merchant_id),
            "webhook",
            "payment_intent.paid",
            "payment_intent",
            command.payment_intent_id,
            json!({
                "payment_intent_id": command.payment_intent_id,
                "transfer_id": command.transfer_id,
                "attempt_id": command.attempt_id,
            }),
            now,
        )
        .await?;
    }
    Ok(())
}

async fn advance_to_partially_paid(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        UPDATE payment_intents
           SET status = 'partially_paid', version = version + 1, updated_at = $2
         WHERE id = $1 AND status IN ('awaiting_payment', 'requires_quote')
        ",
    )
    .bind(command.payment_intent_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;

    insert_payment_event(
        transaction,
        Some(command.merchant_id),
        Some(command.payment_intent_id),
        Some(command.attempt_id),
        Some(command.transfer_id),
        "PARTIAL",
        Some(command.match_strategy.as_str()),
        json!({"policy": command.policy_version}),
        now,
    )
    .await?;
    enqueue_outbox(
        transaction,
        Some(command.merchant_id),
        "webhook",
        "payment_intent.partially_paid",
        "payment_intent",
        command.payment_intent_id,
        json!({
            "payment_intent_id": command.payment_intent_id,
            "transfer_id": command.transfer_id,
        }),
        now,
    )
    .await?;
    Ok(())
}

async fn record_decision(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SettlementCommand,
    outcome: &str,
    reason: &str,
    allocated_raw: RawAmount,
    remainder_raw: RawAmount,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO payment_settlement_decisions (
            id, payment_intent_id, attempt_id, transfer_id, merchant_id, fiat_amount_minor,
            required_policy, distinct_groups, had_own_node, finality_state, risk_decision,
            risk_evaluation_id, attestation_ids, match_strategy, allocated_raw, remainder_raw,
            outcome, decided_by, decided_reason, decided_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
            CAST($15 AS NUMERIC), CAST($16 AS NUMERIC), $17, $18, $19, $20
        )
        ON CONFLICT (payment_intent_id, transfer_id) DO UPDATE
           SET outcome = excluded.outcome,
               decided_reason = excluded.decided_reason,
               allocated_raw = excluded.allocated_raw,
               remainder_raw = excluded.remainder_raw,
               decided_at = excluded.decided_at
        ",
    )
    .bind(Uuid::now_v7())
    .bind(command.payment_intent_id)
    .bind(command.attempt_id)
    .bind(command.transfer_id)
    .bind(command.merchant_id)
    .bind(command.fiat_amount_minor)
    .bind(&command.policy_version)
    .bind(i32::try_from(command.independent_groups).unwrap_or(i32::MAX))
    .bind(command.had_own_node)
    .bind(command.finality_state.as_str())
    .bind(command.risk.as_str())
    .bind(command.risk_evaluation_id)
    .bind(&command.attestation_ids)
    .bind(command.match_strategy.as_str())
    .bind(allocated_raw.to_string())
    .bind(remainder_raw.to_string())
    .bind(outcome)
    .bind(SETTLEMENT_ACTOR)
    .bind(reason)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_payment_event(
    transaction: &mut Transaction<'_, Postgres>,
    merchant_id: Option<Uuid>,
    payment_intent_id: Option<Uuid>,
    attempt_id: Option<Uuid>,
    transfer_id: Option<Uuid>,
    event_type: &str,
    reason_code: Option<&str>,
    payload: serde_json::Value,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO payment_events (
            id, merchant_id, payment_intent_id, attempt_id, transfer_id, event_type,
            reason_code, source, actor, payload, created_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, 'system', $8, $9, $10)
        ",
    )
    .bind(Uuid::now_v7())
    .bind(merchant_id)
    .bind(payment_intent_id)
    .bind(attempt_id)
    .bind(transfer_id)
    .bind(event_type)
    .bind(reason_code)
    .bind(SETTLEMENT_ACTOR)
    .bind(payload)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

/// Writes an outgoing effect inside the money transaction. Delivery happens
/// afterwards; nothing leaves this system from inside a transaction.
#[allow(clippy::too_many_arguments)]
async fn enqueue_outbox(
    transaction: &mut Transaction<'_, Postgres>,
    merchant_id: Option<Uuid>,
    channel: &str,
    event_type: &str,
    aggregate_type: &str,
    aggregate_id: Uuid,
    payload: serde_json::Value,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO domain_events (
            id, merchant_id, channel, event_type, aggregate_type, aggregate_id, payload,
            available_at, created_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        ",
    )
    .bind(Uuid::now_v7())
    .bind(merchant_id)
    .bind(channel)
    .bind(event_type)
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(payload)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

#[cfg(test)]
mod tests;
