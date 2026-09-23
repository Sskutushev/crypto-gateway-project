use std::str::FromStr;

use async_trait::async_trait;
use gateway_application::{
    ComponentState, ComponentStatus, ConflictItem, DeadLetter, DiscrepancyAggregate,
    EvidenceAllocation, EvidenceAttempt, EvidenceFulfillment, EvidenceIntent, EvidencePaymentEvent,
    EvidenceQuote, EvidenceSettlementDecision, EvidenceTransfer, HeldPayment, MinorUnits,
    ObservationConflict, OperatorReadRepository, Overview, PageRequest, PaymentIntentEvidence,
    RailStop, ReconciliationDiscrepancy, ReconciliationRun, ReconciliationRunSummary,
    RepositoryError, SettlementDecisionSummary, StateCount, UnmatchedTransfer,
    WebhookDeliverySummary,
};
use gateway_domain::RawAmount;
use serde_json::Value;
use sqlx::{FromRow, Row, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::PostgresRepository;

#[allow(clippy::needless_pass_by_value)]
fn unavailable(error: sqlx::Error) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}
fn corrupt(message: impl Into<String>) -> RepositoryError {
    RepositoryError::CorruptData(message.into())
}

#[derive(FromRow)]
struct ComponentRow {
    component: String,
    state: String,
    detail: Option<String>,
    since: OffsetDateTime,
    updated_at: OffsetDateTime,
}
#[derive(FromRow)]
struct RailStopRow {
    id: Uuid,
    asset_id: Uuid,
    reason_code: String,
    detail: Option<String>,
    opened_by: String,
    opened_at: OffsetDateTime,
}
#[derive(FromRow)]
struct RunSummaryRow {
    id: Uuid,
    kind: String,
    status: String,
    started_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
}
#[derive(FromRow)]
struct AggregateRow {
    kind: String,
    money_affected: bool,
    count: i64,
}
#[derive(FromRow)]
struct CountRow {
    state: String,
    count: i64,
}
#[derive(FromRow)]
struct ConflictRow {
    id: Uuid,
    chain: String,
    network: String,
    environment: String,
    tx_hash: String,
    event_index: i32,
    field: String,
    created_at: OffsetDateTime,
    items: Value,
}
#[derive(FromRow)]
struct TransferRow {
    id: Uuid,
    chain: String,
    network: String,
    environment: String,
    tx_hash: String,
    event_index: i32,
    collector_address: String,
    asset_id: Uuid,
    amount_raw: String,
    block_time: OffsetDateTime,
    finality_state: String,
    unmatched_at: OffsetDateTime,
}
#[derive(FromRow)]
struct HeldRow {
    id: Uuid,
    merchant_id: Uuid,
    transfer_ids: Vec<Uuid>,
    outcome: Option<String>,
    risk_decision: Option<String>,
    decided_reason: Option<String>,
    decided_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}
#[derive(FromRow)]
struct DeadRow {
    id: Uuid,
    merchant_id: Option<Uuid>,
    event_type: String,
    channel: String,
    aggregate_type: String,
    aggregate_id: Uuid,
    attempts: i32,
    last_error: Option<String>,
    dead_lettered_at: OffsetDateTime,
    deliveries: Value,
}

#[derive(FromRow)]
struct ReconciliationRunRow {
    id: Uuid,
    kind: String,
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
    transfers_examined: i32,
    intents_examined: i32,
    discrepancy_count: i32,
    money_discrepancy_count: i32,
    status: String,
    started_at: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct ReconciliationDiscrepancyRow {
    id: Uuid,
    run_id: Uuid,
    kind: String,
    money_affected: bool,
    transfer_id: Option<Uuid>,
    payment_intent_id: Option<Uuid>,
    asset_id: Option<Uuid>,
    detail: Value,
    created_at: OffsetDateTime,
    resolved_at: Option<OffsetDateTime>,
    resolved_by: Option<String>,
    resolution: Option<String>,
}

fn positive_count(value: i64) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| corrupt("stored count is negative"))
}

#[async_trait]
impl OperatorReadRepository for PostgresRepository {
    /// Returns a complete, bounded operational snapshot so callers never
    /// mistake a partially successful scrape for a healthy system.
    #[allow(clippy::too_many_lines)]
    async fn overview(&self) -> Result<Overview, RepositoryError> {
        let component_rows = sqlx::query_as::<_, ComponentRow>(
            r"
            SELECT component, state, detail, since, updated_at
            FROM component_health
            ORDER BY component
            LIMIT 200
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;
        let components = component_rows
            .into_iter()
            .map(|row| {
                Ok(ComponentStatus {
                    component: row.component,
                    state: ComponentState::parse(&row.state)
                        .map_err(|error| corrupt(error.to_string()))?,
                    detail: row.detail,
                    since: row.since,
                    updated_at: row.updated_at,
                })
            })
            .collect::<Result<_, RepositoryError>>()?;
        let rail_rows = sqlx::query_as::<_, RailStopRow>(
            r"
            SELECT id, asset_id, reason_code, detail, opened_by, opened_at
            FROM rail_stops
            WHERE cleared_at IS NULL
            ORDER BY id DESC
            LIMIT 200
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;
        let open_rail_stops = rail_rows
            .into_iter()
            .map(|row| RailStop {
                id: row.id,
                asset_id: row.asset_id,
                reason_code: row.reason_code,
                detail: row.detail,
                opened_by: row.opened_by,
                opened_at: row.opened_at,
            })
            .collect();
        let run_rows = sqlx::query_as::<_, RunSummaryRow>(
            r"
            SELECT DISTINCT ON (kind) id, kind, status, started_at, finished_at
            FROM reconciliation_runs
            ORDER BY kind, started_at DESC
            LIMIT 2
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;
        let latest_reconciliation = run_rows
            .into_iter()
            .map(|row| ReconciliationRunSummary {
                id: row.id,
                kind: row.kind,
                status: row.status,
                started_at: row.started_at,
                finished_at: row.finished_at,
            })
            .collect();
        let aggregate_rows = sqlx::query_as::<_, AggregateRow>(
            r"
            SELECT kind, money_affected, count(*) AS count
            FROM reconciliation_discrepancies
            WHERE resolved_at IS NULL
            GROUP BY kind, money_affected
            ORDER BY kind
            LIMIT 200
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;
        let open_discrepancies = aggregate_rows
            .into_iter()
            .map(|row| {
                Ok(DiscrepancyAggregate {
                    kind: row.kind,
                    money_affected: row.money_affected,
                    count: positive_count(row.count)?,
                })
            })
            .collect::<Result<_, RepositoryError>>()?;
        let transfers_by_processing_state = counts(
            self,
            r"
            SELECT processing_state AS state, count(*) AS count
            FROM chain_transfer_processing
            GROUP BY processing_state
            ORDER BY processing_state
            LIMIT 200
            ",
        )
        .await?;
        let payment_intents_by_status = counts(
            self,
            r"
            SELECT status AS state, count(*) AS count
            FROM payment_intents
            GROUP BY status
            ORDER BY status
            LIMIT 200
            ",
        )
        .await?;
        let row = sqlx::query(
            r"
            SELECT count(*) FILTER (
                       WHERE delivered_at IS NULL AND dead_lettered_at IS NULL
                   ) AS pending,
                   count(*) FILTER (WHERE dead_lettered_at IS NOT NULL) AS dead
            FROM domain_events
            ",
        )
        .fetch_one(self.pool())
        .await
        .map_err(unavailable)?;
        let conflicts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM chain_observation_conflicts WHERE resolved_at IS NULL",
        )
        .fetch_one(self.pool())
        .await
        .map_err(unavailable)?;
        Ok(Overview {
            components,
            open_rail_stops,
            latest_reconciliation,
            open_discrepancies,
            transfers_by_processing_state,
            payment_intents_by_status,
            outbox_pending: positive_count(row.try_get("pending").map_err(unavailable)?)?,
            outbox_dead_lettered: positive_count(row.try_get("dead").map_err(unavailable)?)?,
            observation_conflicts_open: positive_count(conflicts)?,
        })
    }

    async fn conflicts(
        &self,
        page: PageRequest,
    ) -> Result<Vec<ObservationConflict>, RepositoryError> {
        let rows=sqlx::query_as::<_,ConflictRow>(r"SELECT c.id,c.chain,c.network,c.chain_environment AS environment,c.tx_hash,c.event_index,c.field,c.created_at,COALESCE(jsonb_agg(jsonb_build_object('observation_id',i.observation_id,'field_value',i.field_value) ORDER BY i.observation_id) FILTER (WHERE i.observation_id IS NOT NULL),'[]'::jsonb) AS items FROM chain_observation_conflicts c LEFT JOIN chain_observation_conflict_items i ON i.conflict_id=c.id WHERE c.resolved_at IS NULL AND ($1::uuid IS NULL OR c.id<$1) GROUP BY c.id ORDER BY c.id DESC LIMIT $2").bind(page.before).bind(i64::from(page.limit) + 1).fetch_all(self.pool()).await.map_err(unavailable)?;
        rows.into_iter()
            .map(|r| {
                let items: Vec<Value> =
                    serde_json::from_value(r.items).map_err(|e| corrupt(e.to_string()))?;
                let items = items
                    .into_iter()
                    .map(|v| {
                        Ok(ConflictItem {
                            observation_id: serde_json::from_value(
                                v.get("observation_id")
                                    .cloned()
                                    .ok_or_else(|| corrupt("conflict item lacks observation_id"))?,
                            )
                            .map_err(|e| corrupt(e.to_string()))?,
                            field_value: v
                                .get("field_value")
                                .cloned()
                                .ok_or_else(|| corrupt("conflict item lacks field_value"))?,
                        })
                    })
                    .collect::<Result<_, RepositoryError>>()?;
                Ok(ObservationConflict {
                    id: r.id,
                    chain: r.chain,
                    network: r.network,
                    environment: r.environment,
                    tx_hash: r.tx_hash,
                    event_index: r.event_index,
                    field: r.field,
                    created_at: r.created_at,
                    items,
                })
            })
            .collect()
    }

    async fn unmatched_transfers(
        &self,
        page: PageRequest,
    ) -> Result<Vec<UnmatchedTransfer>, RepositoryError> {
        sqlx::query_as::<_,TransferRow>("SELECT t.id,t.chain,t.network,t.chain_environment AS environment,t.tx_hash,t.event_index,c.address_text AS collector_address,t.asset_id,t.amount_raw::text,t.block_time,s.state AS finality_state,p.updated_at AS unmatched_at FROM chain_transfers t JOIN collector_addresses c ON c.id=t.collector_address_id JOIN chain_transfer_state_current s ON s.transfer_id=t.id JOIN chain_transfer_processing p ON p.transfer_id=t.id WHERE p.processing_state='unmatched' AND ($1::uuid IS NULL OR t.id<$1) ORDER BY t.id DESC LIMIT $2").bind(page.before).bind(i64::from(page.limit) + 1).fetch_all(self.pool()).await.map_err(unavailable)?.into_iter().map(|r|Ok(UnmatchedTransfer{id:r.id,chain:r.chain,network:r.network,environment:r.environment,tx_hash:r.tx_hash,event_index:r.event_index,collector_address:r.collector_address,asset_id:r.asset_id,amount_raw:RawAmount::from_str(&r.amount_raw).map_err(|e|corrupt(e.to_string()))?,block_time:r.block_time,finality_state:r.finality_state,unmatched_at:r.unmatched_at})).collect()
    }

    async fn held_payments(&self, page: PageRequest) -> Result<Vec<HeldPayment>, RepositoryError> {
        let items = sqlx::query_as::<_,HeldRow>(r"SELECT p.id,p.merchant_id,COALESCE(array_agg(DISTINCT d.transfer_id) FILTER (WHERE d.transfer_id IS NOT NULL),'{}') AS transfer_ids,(array_agg(d.outcome ORDER BY d.decided_at DESC))[1] AS outcome,(array_agg(d.risk_decision ORDER BY d.decided_at DESC))[1] AS risk_decision,(array_agg(d.decided_reason ORDER BY d.decided_at DESC))[1] AS decided_reason,max(d.decided_at) AS decided_at,p.created_at FROM payment_intents p LEFT JOIN payment_settlement_decisions d ON d.payment_intent_id=p.id LEFT JOIN chain_transfer_processing tp ON tp.transfer_id=d.transfer_id WHERE (p.status='risk_hold' OR tp.processing_state='held') AND ($1::uuid IS NULL OR p.id<$1) GROUP BY p.id ORDER BY p.id DESC LIMIT $2").bind(page.before).bind(i64::from(page.limit) + 1).fetch_all(self.pool()).await.map_err(unavailable)?.into_iter().map(|r| { let latest_decision=match(r.outcome,r.risk_decision,r.decided_at){(Some(outcome),Some(risk_decision),Some(decided_at))=>Some(SettlementDecisionSummary{outcome,risk_decision,decided_reason:r.decided_reason,decided_at}),_=>None}; HeldPayment{id:r.id,merchant_id:r.merchant_id,transfer_ids:r.transfer_ids,latest_decision,created_at:r.created_at}}).collect();
        Ok(items)
    }

    async fn dead_letters(&self, page: PageRequest) -> Result<Vec<DeadLetter>, RepositoryError> {
        let rows=sqlx::query_as::<_,DeadRow>(r"SELECT e.id,e.merchant_id,e.event_type,e.channel,e.aggregate_type,e.aggregate_id,e.attempts,e.last_error,e.dead_lettered_at,COALESCE((SELECT jsonb_agg(jsonb_build_object('attempt',x.attempt,'response_status',x.response_status,'error',x.error,'delivered_at',x.delivered_at) ORDER BY x.attempt DESC) FROM (SELECT attempt,response_status,error,delivered_at FROM webhook_deliveries WHERE event_id=e.id ORDER BY attempt DESC LIMIT 3)x),'[]'::jsonb) deliveries FROM domain_events e WHERE e.dead_lettered_at IS NOT NULL AND ($1::uuid IS NULL OR e.id<$1) ORDER BY e.id DESC LIMIT $2").bind(page.before).bind(i64::from(page.limit) + 1).fetch_all(self.pool()).await.map_err(unavailable)?;
        rows.into_iter()
            .map(|r| {
                #[derive(serde::Deserialize)]
                struct D {
                    attempt: i32,
                    response_status: Option<i32>,
                    error: Option<String>,
                    #[serde(with = "time::serde::rfc3339")]
                    delivered_at: OffsetDateTime,
                }
                let ds: Vec<D> =
                    serde_json::from_value(r.deliveries).map_err(|e| corrupt(e.to_string()))?;
                Ok(DeadLetter {
                    id: r.id,
                    merchant_id: r.merchant_id,
                    event_type: r.event_type,
                    channel: r.channel,
                    aggregate_type: r.aggregate_type,
                    aggregate_id: r.aggregate_id,
                    attempts: r.attempts,
                    last_error: r.last_error,
                    dead_lettered_at: r.dead_lettered_at,
                    deliveries: ds
                        .into_iter()
                        .map(|d| WebhookDeliverySummary {
                            attempt: d.attempt,
                            response_status: d.response_status,
                            error: d.error,
                            delivered_at: d.delivered_at,
                        })
                        .collect(),
                })
            })
            .collect()
    }

    async fn reconciliation_runs(
        &self,
        page: PageRequest,
    ) -> Result<Vec<ReconciliationRun>, RepositoryError> {
        Ok(sqlx::query_as::<_, ReconciliationRunRow>("SELECT * FROM reconciliation_runs WHERE ($1::uuid IS NULL OR id<$1) ORDER BY id DESC LIMIT $2").bind(page.before).bind(i64::from(page.limit) + 1).fetch_all(self.pool()).await.map_err(unavailable)?.into_iter().map(|r| ReconciliationRun { id:r.id, kind:r.kind, window_start:r.window_start, window_end:r.window_end, transfers_examined:r.transfers_examined, intents_examined:r.intents_examined, discrepancy_count:r.discrepancy_count, money_discrepancy_count:r.money_discrepancy_count, status:r.status, started_at:r.started_at, finished_at:r.finished_at }).collect())
    }
    async fn open_discrepancies(
        &self,
        page: PageRequest,
    ) -> Result<Vec<ReconciliationDiscrepancy>, RepositoryError> {
        Ok(sqlx::query_as::<_, ReconciliationDiscrepancyRow>("SELECT * FROM reconciliation_discrepancies WHERE resolved_at IS NULL AND ($1::uuid IS NULL OR id<$1) ORDER BY id DESC LIMIT $2").bind(page.before).bind(i64::from(page.limit) + 1).fetch_all(self.pool()).await.map_err(unavailable)?.into_iter().map(|r| ReconciliationDiscrepancy { id:r.id, run_id:r.run_id, kind:r.kind, money_affected:r.money_affected, transfer_id:r.transfer_id, payment_intent_id:r.payment_intent_id, asset_id:r.asset_id, detail:r.detail, created_at:r.created_at, resolved_at:r.resolved_at, resolved_by:r.resolved_by, resolution:r.resolution }).collect())
    }
    #[allow(clippy::too_many_lines)]
    async fn payment_intent_evidence(
        &self,
        intent_id: Uuid,
    ) -> Result<Option<PaymentIntentEvidence>, RepositoryError> {
        let Some(intent_row) = sqlx::query(
            r"
            SELECT id, merchant_id, amount_minor, currency, status, reference,
                   description, metadata, created_at, updated_at
            FROM payment_intents
            WHERE id = $1
            ",
        )
        .bind(intent_id)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?
        else {
            return Ok(None);
        };
        let intent = EvidenceIntent {
            id: intent_row.try_get("id").map_err(unavailable)?,
            merchant_id: intent_row.try_get("merchant_id").map_err(unavailable)?,
            amount_minor: MinorUnits(intent_row.try_get("amount_minor").map_err(unavailable)?),
            currency: intent_row.try_get("currency").map_err(unavailable)?,
            status: intent_row.try_get("status").map_err(unavailable)?,
            reference: intent_row.try_get("reference").map_err(unavailable)?,
            description: intent_row.try_get("description").map_err(unavailable)?,
            metadata: intent_row.try_get("metadata").map_err(unavailable)?,
            created_at: intent_row.try_get("created_at").map_err(unavailable)?,
            updated_at: intent_row.try_get("updated_at").map_err(unavailable)?,
        };

        let attempt_rows = sqlx::query(
            r"
            SELECT id, merchant_id, payment_intent_id, quote_id,
                   collector_address_id, expected_amount_raw::text AS expected_amount_raw,
                   status, quote_expires_at, late_payment_until, created_at, updated_at
            FROM payment_attempts
            WHERE payment_intent_id = $1
            ORDER BY id
            LIMIT 200
            ",
        )
        .bind(intent_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;
        let mut attempts = Vec::with_capacity(attempt_rows.len());
        for row in attempt_rows {
            let attempt_id = row.try_get("id").map_err(unavailable)?;
            let quote_rows = sqlx::query(
                r"
                SELECT id, merchant_id, payment_intent_id, asset_id,
                       collector_address_id, fiat_currency, fiat_amount_minor,
                       base_amount_raw::text AS base_amount_raw,
                       amount_raw::text AS amount_raw,
                       rate_numerator::text AS rate_numerator,
                       rate_denominator::text AS rate_denominator,
                       price_sources, price_observed_at, policy_version,
                       rail_health_observed_at, created_at, expires_at, late_payment_until
                FROM payment_quotes
                WHERE id = $1
                LIMIT 1
                ",
            )
            .bind(row.try_get::<Uuid, _>("quote_id").map_err(unavailable)?)
            .fetch_all(self.pool())
            .await
            .map_err(unavailable)?;
            let quotes = quote_rows
                .into_iter()
                .map(|row| evidence_quote(&row))
                .collect::<Result<Vec<_>, _>>()?;
            attempts.push(EvidenceAttempt {
                id: attempt_id,
                merchant_id: row.try_get("merchant_id").map_err(unavailable)?,
                payment_intent_id: row.try_get("payment_intent_id").map_err(unavailable)?,
                quote_id: row.try_get("quote_id").map_err(unavailable)?,
                collector_address_id: row.try_get("collector_address_id").map_err(unavailable)?,
                expected_amount_raw: raw(&row, "expected_amount_raw")?,
                status: row.try_get("status").map_err(unavailable)?,
                quote_expires_at: row.try_get("quote_expires_at").map_err(unavailable)?,
                late_payment_until: row.try_get("late_payment_until").map_err(unavailable)?,
                created_at: row.try_get("created_at").map_err(unavailable)?,
                updated_at: row.try_get("updated_at").map_err(unavailable)?,
                quotes,
            });
        }

        let allocations = sqlx::query(
            r"
            SELECT id, attempt_id, payment_intent_id, merchant_id, transfer_id,
                   allocated_raw::text AS allocated_raw, allocated_by, reason, created_at
            FROM payment_allocations
            WHERE payment_intent_id = $1
            ORDER BY id
            LIMIT 200
            ",
        )
        .bind(intent_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(|row| evidence_allocation(&row))
        .collect::<Result<Vec<_>, _>>()?;
        let settlement_decisions = sqlx::query(
            r"
            SELECT id, payment_intent_id, attempt_id, transfer_id, merchant_id,
                   fiat_amount_minor, required_policy, distinct_groups, had_own_node,
                   finality_state, risk_decision, risk_evaluation_id, attestation_ids,
                   match_strategy, allocated_raw::text AS allocated_raw,
                   remainder_raw::text AS remainder_raw, outcome, decided_by,
                   decided_reason, decided_at
            FROM payment_settlement_decisions
            WHERE payment_intent_id = $1
            ORDER BY id
            LIMIT 200
            ",
        )
        .bind(intent_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(|row| evidence_decision(&row))
        .collect::<Result<Vec<_>, _>>()?;
        let fulfillment = sqlx::query(
            r"
            SELECT payment_intent_id, merchant_id, status, claimed_at,
                   fulfilled_at, attempts, last_error
            FROM payment_fulfillments
            WHERE payment_intent_id = $1
            LIMIT 1
            ",
        )
        .bind(intent_id)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?
        .map(|row| evidence_fulfillment(&row))
        .transpose()?;
        let payment_events = sqlx::query(
            r"
            SELECT id, merchant_id, payment_intent_id, attempt_id, transfer_id,
                   event_type, previous_status, new_status, reason_code, source,
                   actor, payload, created_at
            FROM payment_events
            WHERE payment_intent_id = $1
            ORDER BY created_at, id
            LIMIT 200
            ",
        )
        .bind(intent_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(|row| evidence_event(&row))
        .collect::<Result<Vec<_>, _>>()?;
        let transfers = sqlx::query(
            r"
            SELECT t.id, t.asset_id, t.collector_address_id, t.chain, t.network,
                   t.chain_environment AS environment, t.tx_hash, t.event_index,
                   t.block_number, t.block_hash, t.block_time, t.from_address_text,
                   t.to_address_text, t.amount_raw::text AS amount_raw, t.decimals,
                   t.memo, t.canonicalization_policy, t.verifier_version,
                   t.canonicalized_at, s.state AS current_state,
                   (SELECT count(*) FROM chain_transfer_attestations a
                    WHERE a.transfer_id = t.id) AS attestation_count
            FROM chain_transfer_intent_claims c
            JOIN chain_transfers t ON t.id = c.transfer_id
            JOIN chain_transfer_state_current s ON s.transfer_id = t.id
            WHERE c.payment_intent_id = $1
            ORDER BY t.id
            LIMIT 200
            ",
        )
        .bind(intent_id)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(|row| evidence_transfer(&row))
        .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(PaymentIntentEvidence {
            intent,
            attempts,
            allocations,
            settlement_decisions,
            fulfillment,
            payment_events,
            transfers,
        }))
    }
}

fn raw(row: &PgRow, column: &str) -> Result<RawAmount, RepositoryError> {
    let value: String = row.try_get(column).map_err(unavailable)?;
    if value == "0" {
        return Ok(RawAmount::ZERO);
    }
    RawAmount::from_str(&value).map_err(|error| corrupt(error.to_string()))
}

fn evidence_quote(row: &PgRow) -> Result<EvidenceQuote, RepositoryError> {
    Ok(EvidenceQuote {
        id: row.try_get("id").map_err(unavailable)?,
        merchant_id: row.try_get("merchant_id").map_err(unavailable)?,
        payment_intent_id: row.try_get("payment_intent_id").map_err(unavailable)?,
        asset_id: row.try_get("asset_id").map_err(unavailable)?,
        collector_address_id: row.try_get("collector_address_id").map_err(unavailable)?,
        fiat_currency: row.try_get("fiat_currency").map_err(unavailable)?,
        fiat_amount_minor: MinorUnits(row.try_get("fiat_amount_minor").map_err(unavailable)?),
        base_amount_raw: raw(row, "base_amount_raw")?,
        amount_raw: raw(row, "amount_raw")?,
        rate_numerator: raw(row, "rate_numerator")?,
        rate_denominator: raw(row, "rate_denominator")?,
        price_sources: row.try_get("price_sources").map_err(unavailable)?,
        price_observed_at: row.try_get("price_observed_at").map_err(unavailable)?,
        policy_version: row.try_get("policy_version").map_err(unavailable)?,
        rail_health_observed_at: row
            .try_get("rail_health_observed_at")
            .map_err(unavailable)?,
        created_at: row.try_get("created_at").map_err(unavailable)?,
        expires_at: row.try_get("expires_at").map_err(unavailable)?,
        late_payment_until: row.try_get("late_payment_until").map_err(unavailable)?,
    })
}

fn evidence_allocation(row: &PgRow) -> Result<EvidenceAllocation, RepositoryError> {
    Ok(EvidenceAllocation {
        id: row.try_get("id").map_err(unavailable)?,
        attempt_id: row.try_get("attempt_id").map_err(unavailable)?,
        payment_intent_id: row.try_get("payment_intent_id").map_err(unavailable)?,
        merchant_id: row.try_get("merchant_id").map_err(unavailable)?,
        transfer_id: row.try_get("transfer_id").map_err(unavailable)?,
        allocated_raw: raw(row, "allocated_raw")?,
        allocated_by: row.try_get("allocated_by").map_err(unavailable)?,
        reason: row.try_get("reason").map_err(unavailable)?,
        created_at: row.try_get("created_at").map_err(unavailable)?,
    })
}

fn evidence_decision(row: &PgRow) -> Result<EvidenceSettlementDecision, RepositoryError> {
    Ok(EvidenceSettlementDecision {
        id: row.try_get("id").map_err(unavailable)?,
        payment_intent_id: row.try_get("payment_intent_id").map_err(unavailable)?,
        attempt_id: row.try_get("attempt_id").map_err(unavailable)?,
        transfer_id: row.try_get("transfer_id").map_err(unavailable)?,
        merchant_id: row.try_get("merchant_id").map_err(unavailable)?,
        fiat_amount_minor: MinorUnits(row.try_get("fiat_amount_minor").map_err(unavailable)?),
        required_policy: row.try_get("required_policy").map_err(unavailable)?,
        distinct_groups: row.try_get("distinct_groups").map_err(unavailable)?,
        had_own_node: row.try_get("had_own_node").map_err(unavailable)?,
        finality_state: row.try_get("finality_state").map_err(unavailable)?,
        risk_decision: row.try_get("risk_decision").map_err(unavailable)?,
        risk_evaluation_id: row.try_get("risk_evaluation_id").map_err(unavailable)?,
        attestation_ids: row.try_get("attestation_ids").map_err(unavailable)?,
        match_strategy: row.try_get("match_strategy").map_err(unavailable)?,
        allocated_raw: raw(row, "allocated_raw")?,
        remainder_raw: raw(row, "remainder_raw")?,
        outcome: row.try_get("outcome").map_err(unavailable)?,
        decided_by: row.try_get("decided_by").map_err(unavailable)?,
        decided_reason: row.try_get("decided_reason").map_err(unavailable)?,
        decided_at: row.try_get("decided_at").map_err(unavailable)?,
    })
}

fn evidence_fulfillment(row: &PgRow) -> Result<EvidenceFulfillment, RepositoryError> {
    Ok(EvidenceFulfillment {
        payment_intent_id: row.try_get("payment_intent_id").map_err(unavailable)?,
        merchant_id: row.try_get("merchant_id").map_err(unavailable)?,
        status: row.try_get("status").map_err(unavailable)?,
        claimed_at: row.try_get("claimed_at").map_err(unavailable)?,
        fulfilled_at: row.try_get("fulfilled_at").map_err(unavailable)?,
        attempts: row.try_get("attempts").map_err(unavailable)?,
        last_error: row.try_get("last_error").map_err(unavailable)?,
    })
}

fn evidence_event(row: &PgRow) -> Result<EvidencePaymentEvent, RepositoryError> {
    Ok(EvidencePaymentEvent {
        id: row.try_get("id").map_err(unavailable)?,
        merchant_id: row.try_get("merchant_id").map_err(unavailable)?,
        payment_intent_id: row.try_get("payment_intent_id").map_err(unavailable)?,
        attempt_id: row.try_get("attempt_id").map_err(unavailable)?,
        transfer_id: row.try_get("transfer_id").map_err(unavailable)?,
        event_type: row.try_get("event_type").map_err(unavailable)?,
        previous_status: row.try_get("previous_status").map_err(unavailable)?,
        new_status: row.try_get("new_status").map_err(unavailable)?,
        reason_code: row.try_get("reason_code").map_err(unavailable)?,
        source: row.try_get("source").map_err(unavailable)?,
        actor: row.try_get("actor").map_err(unavailable)?,
        payload: row.try_get("payload").map_err(unavailable)?,
        created_at: row.try_get("created_at").map_err(unavailable)?,
    })
}

fn evidence_transfer(row: &PgRow) -> Result<EvidenceTransfer, RepositoryError> {
    Ok(EvidenceTransfer {
        id: row.try_get("id").map_err(unavailable)?,
        asset_id: row.try_get("asset_id").map_err(unavailable)?,
        collector_address_id: row.try_get("collector_address_id").map_err(unavailable)?,
        chain: row.try_get("chain").map_err(unavailable)?,
        network: row.try_get("network").map_err(unavailable)?,
        environment: row.try_get("environment").map_err(unavailable)?,
        tx_hash: row.try_get("tx_hash").map_err(unavailable)?,
        event_index: row.try_get("event_index").map_err(unavailable)?,
        block_number: row.try_get("block_number").map_err(unavailable)?,
        block_hash: row.try_get("block_hash").map_err(unavailable)?,
        block_time: row.try_get("block_time").map_err(unavailable)?,
        from_address: row.try_get("from_address_text").map_err(unavailable)?,
        to_address: row.try_get("to_address_text").map_err(unavailable)?,
        amount_raw: raw(row, "amount_raw")?,
        decimals: row.try_get("decimals").map_err(unavailable)?,
        memo: row.try_get("memo").map_err(unavailable)?,
        canonicalization_policy: row
            .try_get("canonicalization_policy")
            .map_err(unavailable)?,
        verifier_version: row.try_get("verifier_version").map_err(unavailable)?,
        canonicalized_at: row.try_get("canonicalized_at").map_err(unavailable)?,
        current_state: row.try_get("current_state").map_err(unavailable)?,
        attestation_count: positive_count(row.try_get("attestation_count").map_err(unavailable)?)?,
    })
}

async fn counts(
    repository: &PostgresRepository,
    query: &str,
) -> Result<Vec<StateCount>, RepositoryError> {
    sqlx::query_as::<_, CountRow>(query)
        .fetch_all(repository.pool())
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(|r| {
            Ok(StateCount {
                state: r.state,
                count: positive_count(r.count)?,
            })
        })
        .collect()
}
