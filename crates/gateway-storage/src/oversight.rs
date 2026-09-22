//! Component health and reconciliation, in SQL.
//!
//! The checks here are written to disagree with the rest of the system. Each
//! one asks a question the happy path already believes it has answered — was a
//! reading ever turned into a fact, does an allocation fit inside its transfer,
//! did a settled payment ever get claimed — and reports every place the answers
//! stop agreeing.

use async_trait::async_trait;
use gateway_application::{
    ComponentState, ComponentStatus, Discrepancy, DiscrepancyKind, HealthRepository,
    ReconciliationRepository, ReconciliationWindow, RepositoryError, RunRecord, ScanFindings,
};
use serde_json::json;
use sqlx::{FromRow, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

/// How many findings of one kind a single run reports.
///
/// A bound keeps one broken component from writing a million rows, and the
/// count of what was examined still says the run was not clean.
const FINDINGS_PER_CHECK: i64 = 500;

#[derive(Debug, FromRow)]
struct ComponentHealthRow {
    component: String,
    state: String,
    detail: Option<String>,
    since: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[async_trait]
impl HealthRepository for PostgresRepository {
    async fn publish_component_state(
        &self,
        component: &str,
        state: ComponentState,
        detail: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<bool, RepositoryError> {
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;
        let previous = sqlx::query_scalar::<_, String>(
            "SELECT state FROM component_health WHERE component = $1 FOR UPDATE",
        )
        .bind(component)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(unavailable)?;

        let changed = previous.as_deref() != Some(state.as_str());
        sqlx::query(
            r"
            INSERT INTO component_health (component, state, detail, since, updated_at)
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (component) DO UPDATE
               SET state = EXCLUDED.state,
                   detail = EXCLUDED.detail,
                   -- The start of a state is when it began, not when it was
                   -- last confirmed: a heartbeat must not reset how long a
                   -- component has been degraded.
                   since = CASE
                       WHEN component_health.state = EXCLUDED.state THEN component_health.since
                       ELSE EXCLUDED.since
                   END,
                   updated_at = EXCLUDED.updated_at
            ",
        )
        .bind(component)
        .bind(state.as_str())
        .bind(detail)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;

        if changed {
            sqlx::query(
                r"
                INSERT INTO component_health_events (
                    id, component, previous_state, new_state, detail, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6)
                ",
            )
            .bind(Uuid::now_v7())
            .bind(component)
            .bind(previous.as_deref())
            .bind(state.as_str())
            .bind(detail)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        }

        transaction.commit().await.map_err(unavailable)?;
        Ok(changed)
    }

    async fn component_statuses(&self) -> Result<Vec<ComponentStatus>, RepositoryError> {
        let rows = sqlx::query_as::<_, ComponentHealthRow>(
            r"
            SELECT component, state, detail, since, updated_at
              FROM component_health
             ORDER BY component
            ",
        )
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
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
            .collect()
    }
}

#[async_trait]
impl ReconciliationRepository for PostgresRepository {
    async fn scan(
        &self,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        window: ReconciliationWindow,
    ) -> Result<ScanFindings, RepositoryError> {
        let transfers_examined = count(
            self,
            "SELECT count(*) FROM chain_transfers WHERE canonicalized_at >= $1 AND canonicalized_at < $2",
            window_start,
            window_end,
        )
        .await?;
        let intents_examined = count(
            self,
            "SELECT count(*) FROM payment_intents WHERE created_at >= $1 AND created_at < $2",
            window_start,
            window_end,
        )
        .await?;

        let mut discrepancies = Vec::new();
        discrepancies.extend(
            self.observed_not_canonical(window_start, window_end, window)
                .await?,
        );
        discrepancies.extend(self.allocation_exceeds_transfer().await?);
        discrepancies.extend(self.settled_not_fulfilled(window_start, window_end).await?);
        discrepancies.extend(self.fulfilled_not_settled(window_start, window_end).await?);
        discrepancies.extend(self.allocated_on_invalidated().await?);
        discrepancies.extend(
            self.aging(
                DiscrepancyKind::UnmatchedInboundAging,
                window_end - window.unmatched_grace,
            )
            .await?,
        );
        discrepancies.extend(
            self.aging(
                DiscrepancyKind::HeldPaymentAging,
                window_end - window.held_grace,
            )
            .await?,
        );
        discrepancies.extend(self.observer_behind(window.max_cursor_lag_blocks).await?);

        Ok(ScanFindings {
            transfers_examined,
            intents_examined,
            discrepancies,
        })
    }

    async fn record_run(&self, record: RunRecord<'_>) -> Result<Uuid, RepositoryError> {
        let RunRecord {
            kind,
            window_start,
            window_end,
            findings,
            status,
            started_at,
            finished_at,
        } = record;
        let run_id = Uuid::now_v7();
        let money = findings
            .discrepancies
            .iter()
            .filter(|discrepancy| discrepancy.kind.affects_money())
            .count();
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;
        sqlx::query(
            r"
            INSERT INTO reconciliation_runs (
                id, kind, window_start, window_end, transfers_examined, intents_examined,
                discrepancy_count, money_discrepancy_count, status, started_at, finished_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ",
        )
        .bind(run_id)
        .bind(kind.as_str())
        .bind(window_start)
        .bind(window_end)
        .bind(i32::try_from(findings.transfers_examined).unwrap_or(i32::MAX))
        .bind(i32::try_from(findings.intents_examined).unwrap_or(i32::MAX))
        .bind(i32::try_from(findings.discrepancies.len()).unwrap_or(i32::MAX))
        .bind(i32::try_from(money).unwrap_or(i32::MAX))
        .bind(status.as_str())
        .bind(started_at)
        .bind(finished_at)
        .execute(&mut *transaction)
        .await
        .map_err(unavailable)?;

        for discrepancy in &findings.discrepancies {
            sqlx::query(
                r"
                INSERT INTO reconciliation_discrepancies (
                    id, run_id, kind, money_affected, transfer_id, payment_intent_id,
                    asset_id, detail, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ",
            )
            .bind(Uuid::now_v7())
            .bind(run_id)
            .bind(discrepancy.kind.as_str())
            .bind(discrepancy.kind.affects_money())
            .bind(discrepancy.transfer_id)
            .bind(discrepancy.payment_intent_id)
            .bind(discrepancy.asset_id)
            .bind(&discrepancy.detail)
            .bind(finished_at)
            .execute(&mut *transaction)
            .await
            .map_err(unavailable)?;
        }

        transaction.commit().await.map_err(unavailable)?;
        Ok(run_id)
    }
}

impl PostgresRepository {
    /// Readings that never became a fact, long after they should have.
    ///
    /// An unknown token is excluded: nobody is owed a canonical fact for a
    /// contract this gateway does not accept.
    async fn observed_not_canonical(
        &self,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        window: ReconciliationWindow,
    ) -> Result<Vec<Discrepancy>, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT observation.tx_hash,
                   observation.event_index,
                   observation.asset_id,
                   min(observation.observed_at) AS first_seen
              FROM chain_observations AS observation
              LEFT JOIN chain_transfers AS transfer
                     ON transfer.chain = observation.chain
                    AND transfer.network = observation.network
                    AND transfer.chain_environment = observation.chain_environment
                    AND transfer.tx_hash = observation.tx_hash
                    AND transfer.event_index = observation.event_index
             WHERE observation.observed_at >= $1
               AND observation.observed_at < $2
               AND observation.asset_id IS NOT NULL
               AND observation.execution_status = 'success'
               AND transfer.id IS NULL
             GROUP BY observation.tx_hash, observation.event_index, observation.asset_id
            HAVING min(observation.observed_at) < $3
             LIMIT $4
            ",
        )
        .bind(window_start)
        .bind(window_end)
        .bind(window_end - window.canonicalization_grace)
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                let tx_hash: String = row.try_get("tx_hash").map_err(unavailable)?;
                let event_index: i32 = row.try_get("event_index").map_err(unavailable)?;
                let asset_id: Option<Uuid> = row.try_get("asset_id").map_err(unavailable)?;
                let first_seen: OffsetDateTime = row.try_get("first_seen").map_err(unavailable)?;
                Ok(Discrepancy {
                    kind: DiscrepancyKind::ObservedNotCanonical,
                    transfer_id: None,
                    payment_intent_id: None,
                    asset_id,
                    detail: json!({
                        "tx_hash": tx_hash,
                        "event_index": event_index,
                        "first_seen": first_seen.unix_timestamp(),
                    }),
                })
            })
            .collect()
    }

    /// The invariant the database already enforces, checked anyway. A check
    /// that can only fail when something else is broken is exactly the check
    /// worth having.
    async fn allocation_exceeds_transfer(&self) -> Result<Vec<Discrepancy>, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT processing.transfer_id,
                   transfer.asset_id,
                   processing.allocated_raw::TEXT AS allocated_raw,
                   transfer.amount_raw::TEXT AS amount_raw
              FROM chain_transfer_processing AS processing
              JOIN chain_transfers AS transfer ON transfer.id = processing.transfer_id
             WHERE processing.allocated_raw > transfer.amount_raw
             LIMIT $1
            ",
        )
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                Ok(Discrepancy {
                    kind: DiscrepancyKind::AllocationExceedsTransfer,
                    transfer_id: Some(row.try_get("transfer_id").map_err(unavailable)?),
                    payment_intent_id: None,
                    asset_id: Some(row.try_get("asset_id").map_err(unavailable)?),
                    detail: json!({
                        "allocated_raw": row.try_get::<String, _>("allocated_raw").map_err(unavailable)?,
                        "amount_raw": row.try_get::<String, _>("amount_raw").map_err(unavailable)?,
                    }),
                })
            })
            .collect()
    }

    /// Money taken with nothing claiming the obligation it paid for.
    async fn settled_not_fulfilled(
        &self,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
    ) -> Result<Vec<Discrepancy>, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT decision.payment_intent_id,
                   decision.transfer_id,
                   transfer.asset_id,
                   coalesce(fulfilment.status, 'missing') AS fulfilment_status
              FROM payment_settlement_decisions AS decision
              JOIN chain_transfers AS transfer ON transfer.id = decision.transfer_id
              LEFT JOIN payment_fulfillments AS fulfilment
                     ON fulfilment.payment_intent_id = decision.payment_intent_id
             WHERE decision.outcome = 'settled'
               AND decision.decided_at >= $1
               AND decision.decided_at < $2
               AND (fulfilment.payment_intent_id IS NULL OR fulfilment.status = 'failed')
             LIMIT $3
            ",
        )
        .bind(window_start)
        .bind(window_end)
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                Ok(Discrepancy {
                    kind: DiscrepancyKind::SettledNotFulfilled,
                    transfer_id: Some(row.try_get("transfer_id").map_err(unavailable)?),
                    payment_intent_id: Some(row.try_get("payment_intent_id").map_err(unavailable)?),
                    asset_id: Some(row.try_get("asset_id").map_err(unavailable)?),
                    detail: json!({
                        "fulfilment_status": row
                            .try_get::<String, _>("fulfilment_status")
                            .map_err(unavailable)?,
                    }),
                })
            })
            .collect()
    }

    /// An obligation claimed with no settlement behind it.
    async fn fulfilled_not_settled(
        &self,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
    ) -> Result<Vec<Discrepancy>, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT fulfilment.payment_intent_id
              FROM payment_fulfillments AS fulfilment
              LEFT JOIN payment_settlement_decisions AS decision
                     ON decision.payment_intent_id = fulfilment.payment_intent_id
                    AND decision.outcome = 'settled'
             WHERE fulfilment.claimed_at >= $1
               AND fulfilment.claimed_at < $2
               AND decision.id IS NULL
             LIMIT $3
            ",
        )
        .bind(window_start)
        .bind(window_end)
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                Ok(Discrepancy {
                    kind: DiscrepancyKind::FulfilledNotSettled,
                    transfer_id: None,
                    payment_intent_id: Some(row.try_get("payment_intent_id").map_err(unavailable)?),
                    asset_id: None,
                    detail: json!({}),
                })
            })
            .collect()
    }

    /// A transfer the chain later invalidated that still carries allocations.
    ///
    /// The product is never withdrawn automatically; this is the finding that
    /// puts it in front of a person.
    async fn allocated_on_invalidated(&self) -> Result<Vec<Discrepancy>, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT allocation.transfer_id,
                   allocation.payment_intent_id,
                   transfer.asset_id
              FROM payment_allocations AS allocation
              JOIN chain_transfer_state_current AS state
                    ON state.transfer_id = allocation.transfer_id
              JOIN chain_transfers AS transfer ON transfer.id = allocation.transfer_id
             WHERE state.state = 'invalidated'
             LIMIT $1
            ",
        )
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                Ok(Discrepancy {
                    kind: DiscrepancyKind::AllocatedOnInvalidatedTransfer,
                    transfer_id: Some(row.try_get("transfer_id").map_err(unavailable)?),
                    payment_intent_id: Some(row.try_get("payment_intent_id").map_err(unavailable)?),
                    asset_id: Some(row.try_get("asset_id").map_err(unavailable)?),
                    detail: json!({}),
                })
            })
            .collect()
    }

    /// Money or payments that have been waiting for a person for too long.
    async fn aging(
        &self,
        kind: DiscrepancyKind,
        older_than: OffsetDateTime,
    ) -> Result<Vec<Discrepancy>, RepositoryError> {
        let state = match kind {
            DiscrepancyKind::UnmatchedInboundAging => "unmatched",
            DiscrepancyKind::HeldPaymentAging => "held",
            other => {
                return Err(corrupt(format!(
                    "{} is not an ageing check",
                    other.as_str()
                )));
            }
        };
        let rows = sqlx::query(
            r"
            SELECT processing.transfer_id,
                   transfer.asset_id,
                   processing.updated_at,
                   transfer.amount_raw::TEXT AS amount_raw
              FROM chain_transfer_processing AS processing
              JOIN chain_transfers AS transfer ON transfer.id = processing.transfer_id
             WHERE processing.processing_state = $1
               AND processing.updated_at < $2
             LIMIT $3
            ",
        )
        .bind(state)
        .bind(older_than)
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                let waiting_since: OffsetDateTime =
                    row.try_get("updated_at").map_err(unavailable)?;
                Ok(Discrepancy {
                    kind,
                    transfer_id: Some(row.try_get("transfer_id").map_err(unavailable)?),
                    payment_intent_id: None,
                    asset_id: Some(row.try_get("asset_id").map_err(unavailable)?),
                    detail: json!({
                        "waiting_since": waiting_since.unix_timestamp(),
                        "amount_raw": row.try_get::<String, _>("amount_raw").map_err(unavailable)?,
                    }),
                })
            })
            .collect()
    }

    /// A source whose cursor sits far behind the head that same source
    /// reported. Nobody else's head is used: a provider is measured against
    /// its own claim.
    async fn observer_behind(
        &self,
        max_lag_blocks: i64,
    ) -> Result<Vec<Discrepancy>, RepositoryError> {
        let rows = sqlx::query(
            r"
            SELECT source.source_key,
                   cursor_row.cursor_value,
                   head.source_head
              FROM chain_cursors AS cursor_row
              JOIN chain_sources AS source ON source.id = cursor_row.source_id
              JOIN LATERAL (
                    SELECT max(observation.source_head) AS source_head
                      FROM chain_observations AS observation
                     WHERE observation.source_id = cursor_row.source_id
              ) AS head ON true
             WHERE cursor_row.cursor_kind = 'block'
               AND cursor_row.cursor_value ~ '^[0-9]+$'
               AND head.source_head IS NOT NULL
               AND head.source_head - cursor_row.cursor_value::BIGINT > $1
             LIMIT $2
            ",
        )
        .bind(max_lag_blocks)
        .bind(FINDINGS_PER_CHECK)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter()
            .map(|row| {
                Ok(Discrepancy {
                    kind: DiscrepancyKind::ObserverBehind,
                    transfer_id: None,
                    payment_intent_id: None,
                    asset_id: None,
                    detail: json!({
                        "source_key": row.try_get::<String, _>("source_key").map_err(unavailable)?,
                        "cursor_value": row
                            .try_get::<String, _>("cursor_value")
                            .map_err(unavailable)?,
                        "source_head": row.try_get::<i64, _>("source_head").map_err(unavailable)?,
                    }),
                })
            })
            .collect()
    }
}

async fn count(
    repository: &PostgresRepository,
    query: &str,
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
) -> Result<u32, RepositoryError> {
    let count: i64 = sqlx::query_scalar(query)
        .bind(window_start)
        .bind(window_end)
        .fetch_one(repository.pool())
        .await
        .map_err(unavailable)?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}
