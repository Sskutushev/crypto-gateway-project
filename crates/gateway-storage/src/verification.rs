use async_trait::async_trait;
use gateway_application::{
    ChainEventKey, ComponentLease, RepositoryError, VerdictOutcome, VerificationRepository,
};
use gateway_domain::{
    AddressKey, CanonicalTransfer, ChainEnvironment, EvidenceReading, ExecutionStatus,
    FieldConflict, FinalityPolicy, Memo, ObservationKind, ObservedTransfer, RawAmount,
    SourceFinality, TransferState, TxHash, Verdict, VerifiedTransfer,
};
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

const CANONICALIZATION_POLICY: &str = "independent-groups-and-own-reread";

#[derive(Debug, FromRow)]
struct FinalityPolicyRow {
    id: Uuid,
    version: String,
    min_confirmations: i64,
    required_source_finality: String,
    min_independent_groups: i32,
    max_evidence_age_seconds: i64,
    observed_at: OffsetDateTime,
}

impl TryFrom<FinalityPolicyRow> for FinalityPolicy {
    type Error = RepositoryError;

    fn try_from(row: FinalityPolicyRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            version: row.version,
            min_confirmations: row.min_confirmations,
            required_source_finality: row
                .required_source_finality
                .parse::<SourceFinality>()
                .map_err(|error| corrupt(error.to_string()))?,
            min_independent_groups: u32::try_from(row.min_independent_groups)
                .map_err(|_| corrupt("a finality policy carries a negative group count"))?,
            max_evidence_age_seconds: row.max_evidence_age_seconds,
            observed_at: row.observed_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct EventRow {
    chain: String,
    network: String,
    chain_environment: String,
    tx_hash: String,
    event_index: i32,
}

impl TryFrom<EventRow> for ChainEventKey {
    type Error = RepositoryError;

    fn try_from(row: EventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            chain: row.chain,
            network: row.network,
            chain_environment: row
                .chain_environment
                .parse::<ChainEnvironment>()
                .map_err(|error| corrupt(error.to_string()))?,
            tx_hash: TxHash::new(&row.tx_hash).map_err(|error| corrupt(error.to_string()))?,
            event_index: row.event_index,
        })
    }
}

#[derive(Debug, FromRow)]
#[allow(clippy::struct_field_names)]
struct EvidenceRow {
    observation_id: Uuid,
    source_id: Uuid,
    provider_group: String,
    source_kind: String,
    source_principal: String,
    declared_principal: String,
    observation_kind: String,
    asset_id: Option<Uuid>,
    collector_address_id: Option<Uuid>,
    chain: String,
    network: String,
    chain_environment: String,
    tx_hash: String,
    event_index: i32,
    block_number: Option<i64>,
    block_hash: Option<String>,
    parent_hash: Option<String>,
    block_time: Option<OffsetDateTime>,
    token_key: Vec<u8>,
    token_display: String,
    from_address_key: Vec<u8>,
    from_address_text: String,
    to_address_key: Vec<u8>,
    to_address_text: String,
    amount_raw: String,
    decimals: i16,
    memo: Option<String>,
    execution_status: String,
    source_finality: String,
    source_head: Option<i64>,
    evidence_sha256: String,
    evidence_uri: Option<String>,
    observed_at: OffsetDateTime,
}

impl TryFrom<EvidenceRow> for EvidenceReading {
    type Error = RepositoryError;

    fn try_from(row: EvidenceRow) -> Result<Self, Self::Error> {
        let transfer = ObservedTransfer {
            chain: row.chain,
            network: row.network,
            chain_environment: row
                .chain_environment
                .parse::<ChainEnvironment>()
                .map_err(|error| corrupt(error.to_string()))?,
            tx_hash: TxHash::new(&row.tx_hash).map_err(|error| corrupt(error.to_string()))?,
            event_index: row.event_index,
            block_number: row.block_number,
            block_hash: row.block_hash,
            parent_hash: row.parent_hash,
            block_time: row.block_time,
            token_key: AddressKey::new(row.token_key)
                .map_err(|error| corrupt(error.to_string()))?,
            token_display: row.token_display,
            from_address: AddressKey::new(row.from_address_key)
                .map_err(|error| corrupt(error.to_string()))?,
            from_address_text: row.from_address_text,
            to_address: AddressKey::new(row.to_address_key)
                .map_err(|error| corrupt(error.to_string()))?,
            to_address_text: row.to_address_text,
            amount_raw: row
                .amount_raw
                .parse::<RawAmount>()
                .map_err(|error| corrupt(error.to_string()))?,
            decimals: row.decimals,
            memo: row
                .memo
                .as_deref()
                .map(Memo::new)
                .transpose()
                .map_err(|error| corrupt(error.to_string()))?,
            execution_status: row
                .execution_status
                .parse::<ExecutionStatus>()
                .map_err(|error| corrupt(error.to_string()))?,
            source_finality: row
                .source_finality
                .parse::<SourceFinality>()
                .map_err(|error| corrupt(error.to_string()))?,
            source_head: row.source_head,
            evidence_sha256: row.evidence_sha256,
            evidence_uri: row.evidence_uri,
        };
        Ok(Self {
            observation_id: row.observation_id,
            source_id: row.source_id,
            provider_group: row.provider_group,
            source_kind: row.source_kind,
            source_principal: row.source_principal,
            declared_principal: row.declared_principal,
            kind: row
                .observation_kind
                .parse::<ObservationKind>()
                .map_err(|error| corrupt(error.to_string()))?,
            asset_id: row.asset_id,
            collector_address_id: row.collector_address_id,
            transfer,
            observed_at: row.observed_at,
        })
    }
}

#[async_trait]
impl VerificationRepository for PostgresRepository {
    async fn find_finality_policy(
        &self,
        chain: &str,
        network: &str,
        environment: ChainEnvironment,
    ) -> Result<Option<FinalityPolicy>, RepositoryError> {
        let row = sqlx::query_as::<_, FinalityPolicyRow>(
            r"
            SELECT id, version, min_confirmations, required_source_finality,
                   min_independent_groups, max_evidence_age_seconds, observed_at
              FROM chain_finality_policies
             WHERE chain = $1 AND network = $2 AND chain_environment = $3 AND status = 'active'
            ",
        )
        .bind(chain)
        .bind(network)
        .bind(environment.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        row.map(TryInto::try_into).transpose()
    }

    async fn events_awaiting_verdict(
        &self,
        limit: u32,
    ) -> Result<Vec<ChainEventKey>, RepositoryError> {
        // An event is decided again only when new evidence arrived after the
        // last decision: a verdict is a checkpoint, not a silence.
        let rows = sqlx::query_as::<_, EventRow>(
            r"
            SELECT observation.chain,
                   observation.network,
                   observation.chain_environment,
                   observation.tx_hash,
                   observation.event_index
              FROM chain_observations AS observation
              LEFT JOIN chain_event_verdicts AS verdict
                     ON verdict.chain = observation.chain
                    AND verdict.network = observation.network
                    AND verdict.chain_environment = observation.chain_environment
                    AND verdict.tx_hash = observation.tx_hash
                    AND verdict.event_index = observation.event_index
             GROUP BY observation.chain, observation.network, observation.chain_environment,
                      observation.tx_hash, observation.event_index,
                      verdict.verdict, verdict.decided_at
            HAVING verdict.verdict IS NULL
                OR (verdict.verdict IN ('insufficient', 'verified')
                    AND max(observation.observed_at) > verdict.decided_at)
             ORDER BY min(observation.observed_at)
             LIMIT $1
            ",
        )
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn evidence_for(
        &self,
        event: &ChainEventKey,
    ) -> Result<Vec<EvidenceReading>, RepositoryError> {
        let rows = sqlx::query_as::<_, EvidenceRow>(
            r"
            SELECT observation.id AS observation_id,
                   observation.source_id,
                   source.provider_group,
                   source.kind AS source_kind,
                   observation.source_principal,
                   source.db_principal AS declared_principal,
                   observation.observation_kind,
                   observation.asset_id,
                   observation.collector_address_id,
                   observation.chain,
                   observation.network,
                   observation.chain_environment,
                   observation.tx_hash,
                   observation.event_index,
                   observation.block_number,
                   observation.block_hash,
                   observation.parent_hash,
                   observation.block_time,
                   observation.token_key,
                   observation.token_display,
                   observation.from_address_key,
                   observation.from_address_text,
                   observation.to_address_key,
                   observation.to_address_text,
                   observation.amount_raw::TEXT AS amount_raw,
                   observation.decimals,
                   observation.memo,
                   observation.execution_status,
                   observation.source_finality,
                   observation.source_head,
                   observation.evidence_sha256,
                   observation.evidence_uri,
                   observation.observed_at
              FROM chain_observations AS observation
              JOIN chain_sources AS source ON source.id = observation.source_id
             WHERE observation.chain = $1
               AND observation.network = $2
               AND observation.chain_environment = $3
               AND observation.tx_hash = $4
               AND observation.event_index = $5
             ORDER BY observation.observed_at, observation.id
            ",
        )
        .bind(&event.chain)
        .bind(&event.network)
        .bind(event.chain_environment.as_str())
        .bind(event.tx_hash.as_str())
        .bind(event.event_index)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn commit_verdict(
        &self,
        lease: &ComponentLease,
        event: &ChainEventKey,
        verdict: &Verdict,
        evidence_count: u32,
        verifier_version: &str,
        now: OffsetDateTime,
    ) -> Result<VerdictOutcome, RepositoryError> {
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;
        hold_lease(&mut transaction, lease).await?;

        let outcome = match verdict {
            Verdict::Verified(verified) => {
                commit_verified(&mut transaction, event, verified, verifier_version, now).await?
            }
            Verdict::Conflicted {
                conflicts,
                discarded,
            } => {
                commit_conflicts(&mut transaction, event, conflicts, now).await?;
                record_verdict(
                    &mut transaction,
                    event,
                    "conflicted",
                    Some(&conflict_fields(conflicts)),
                    evidence_count,
                    0,
                    u32::try_from(discarded.len()).unwrap_or(u32::MAX),
                    None,
                    verifier_version,
                    now,
                )
                .await?;
                VerdictOutcome::ConflictRecorded
            }
            Verdict::Insufficient { reason, discarded } => {
                record_verdict(
                    &mut transaction,
                    event,
                    "insufficient",
                    Some(reason.as_str()),
                    evidence_count,
                    0,
                    u32::try_from(discarded.len()).unwrap_or(u32::MAX),
                    None,
                    verifier_version,
                    now,
                )
                .await?;
                VerdictOutcome::Pending
            }
            Verdict::Rejected { reason, discarded } => {
                record_verdict(
                    &mut transaction,
                    event,
                    "rejected",
                    Some(reason.as_str()),
                    evidence_count,
                    0,
                    u32::try_from(discarded.len()).unwrap_or(u32::MAX),
                    None,
                    verifier_version,
                    now,
                )
                .await?;
                VerdictOutcome::Refused
            }
        };

        transaction.commit().await.map_err(unavailable)?;
        Ok(outcome)
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

async fn commit_verified(
    transaction: &mut Transaction<'_, Postgres>,
    event: &ChainEventKey,
    verified: &VerifiedTransfer,
    verifier_version: &str,
    now: OffsetDateTime,
) -> Result<VerdictOutcome, RepositoryError> {
    let transfer = &verified.transfer;
    let created = sqlx::query_scalar::<_, Uuid>(
        r"
        INSERT INTO chain_transfers (
            id, asset_id, collector_address_id, chain, network, chain_environment,
            tx_hash, event_index, block_number, block_hash, block_time, token_key,
            from_address_key, from_address_text, to_address_key, to_address_text,
            amount_raw, decimals, memo, canonicalization_policy, verifier_version,
            canonicalized_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
            CAST($17 AS NUMERIC), $18, $19, $20, $21, $22
        )
        ON CONFLICT (chain, network, chain_environment, tx_hash, event_index) DO NOTHING
        RETURNING id
        ",
    )
    .bind(Uuid::now_v7())
    .bind(transfer.asset_id)
    .bind(transfer.collector_address_id)
    .bind(&transfer.chain)
    .bind(&transfer.network)
    .bind(transfer.chain_environment.as_str())
    .bind(transfer.tx_hash.as_str())
    .bind(transfer.event_index)
    .bind(transfer.block_number)
    .bind(&transfer.block_hash)
    .bind(transfer.block_time)
    .bind(transfer.token_key.as_bytes())
    .bind(transfer.from_address.as_bytes())
    .bind(&transfer.from_address_text)
    .bind(transfer.to_address.as_bytes())
    .bind(&transfer.to_address_text)
    .bind(transfer.amount_raw.to_string())
    .bind(transfer.decimals)
    .bind(transfer.memo.as_ref().map(Memo::as_str))
    .bind(CANONICALIZATION_POLICY)
    .bind(verifier_version)
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;

    let (transfer_id, newly_created) = if let Some(id) = created {
        (id, true)
    } else {
        (find_transfer_id(transaction, transfer).await?, false)
    };

    if newly_created {
        open_transfer_lifecycle(transaction, transfer_id, verified, now).await?;
    }

    for (observation_id, role) in &verified.attestations {
        sqlx::query(
            r"
            INSERT INTO chain_transfer_attestations (
                id, transfer_id, observation_id, source_id, provider_group, source_kind,
                attestation_role, verifier_version, created_at
            )
            SELECT $1, $2, observation.id, observation.source_id, source.provider_group,
                   source.kind, $4, $5, $6
              FROM chain_observations AS observation
              JOIN chain_sources AS source ON source.id = observation.source_id
             WHERE observation.id = $3
            ON CONFLICT (observation_id) DO NOTHING
            ",
        )
        .bind(Uuid::now_v7())
        .bind(transfer_id)
        .bind(observation_id)
        .bind(role.as_str())
        .bind(verifier_version)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(unavailable)?;
    }

    advance_state(transaction, transfer_id, verified, now).await?;
    record_verdict(
        transaction,
        event,
        "verified",
        Some(verified.state.as_str()),
        u32::try_from(verified.attestations.len()).unwrap_or(u32::MAX),
        verified.independent_groups,
        u32::try_from(verified.discarded.len()).unwrap_or(u32::MAX),
        Some(transfer_id),
        verifier_version,
        now,
    )
    .await?;

    Ok(if newly_created {
        VerdictOutcome::TransferCreated(transfer_id)
    } else {
        VerdictOutcome::TransferAdvanced(transfer_id)
    })
}

async fn find_transfer_id(
    transaction: &mut Transaction<'_, Postgres>,
    transfer: &CanonicalTransfer,
) -> Result<Uuid, RepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r"
        SELECT id FROM chain_transfers
         WHERE chain = $1 AND network = $2 AND chain_environment = $3
           AND tx_hash = $4 AND event_index = $5
        ",
    )
    .bind(&transfer.chain)
    .bind(&transfer.network)
    .bind(transfer.chain_environment.as_str())
    .bind(transfer.tx_hash.as_str())
    .bind(transfer.event_index)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?
    .ok_or_else(|| corrupt("a canonical transfer vanished between insert and read"))
}

/// Opens the lifecycle of a freshly canonical transfer: its first state event,
/// its current state, and the mutable processing row the payment path owns.
async fn open_transfer_lifecycle(
    transaction: &mut Transaction<'_, Postgres>,
    transfer_id: Uuid,
    verified: &VerifiedTransfer,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    insert_state_event(
        transaction,
        transfer_id,
        None,
        TransferState::Canonical,
        1,
        verified,
        now,
    )
    .await?;
    sqlx::query(
        r"
        INSERT INTO chain_transfer_state_current (transfer_id, state, state_version, updated_at)
        VALUES ($1, 'canonical', 1, $2)
        ",
    )
    .bind(transfer_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    sqlx::query("INSERT INTO chain_transfer_processing (transfer_id, updated_at) VALUES ($1, $2)")
        .bind(transfer_id)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(unavailable)?;
    Ok(())
}

/// Walks the current state towards the verified state one allowed step at a
/// time, by compare-and-swap. A stale decision loses the swap instead of
/// pulling the state backwards.
async fn advance_state(
    transaction: &mut Transaction<'_, Postgres>,
    transfer_id: Uuid,
    verified: &VerifiedTransfer,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    loop {
        let current = sqlx::query_as::<_, (String, i64)>(
            "SELECT state, state_version FROM chain_transfer_state_current WHERE transfer_id = $1",
        )
        .bind(transfer_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| corrupt("a canonical transfer has no current state"))?;

        let state = current
            .0
            .parse::<TransferState>()
            .map_err(|error| corrupt(error.to_string()))?;
        let Some(next) = next_step(state, verified.state) else {
            return Ok(());
        };

        let swapped = sqlx::query(
            r"
            UPDATE chain_transfer_state_current
               SET state = $3, state_version = state_version + 1, updated_at = $4
             WHERE transfer_id = $1 AND state_version = $2 AND state = $5
            ",
        )
        .bind(transfer_id)
        .bind(current.1)
        .bind(next.as_str())
        .bind(now)
        .bind(state.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(unavailable)?;

        if swapped.rows_affected() != 1 {
            // Someone else moved this transfer first. Their decision stands.
            return Ok(());
        }
        insert_state_event(
            transaction,
            transfer_id,
            Some(state),
            next,
            current.1.saturating_add(1),
            verified,
            now,
        )
        .await?;
    }
}

const fn next_step(current: TransferState, target: TransferState) -> Option<TransferState> {
    match (current, target) {
        (TransferState::Canonical, TransferState::Confirmed | TransferState::Finalized) => {
            Some(TransferState::Confirmed)
        }
        (TransferState::Confirmed, TransferState::Finalized) => Some(TransferState::Finalized),
        _ => None,
    }
}

async fn insert_state_event(
    transaction: &mut Transaction<'_, Postgres>,
    transfer_id: Uuid,
    previous: Option<TransferState>,
    new_state: TransferState,
    state_version: i64,
    verified: &VerifiedTransfer,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO chain_transfer_state_events (
            id, transfer_id, previous_state, new_state, state_version, source_key,
            block_hash, head_height, reason, created_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ",
    )
    .bind(Uuid::now_v7())
    .bind(transfer_id)
    .bind(previous.map(TransferState::as_str))
    .bind(new_state.as_str())
    .bind(state_version)
    .bind("verifier")
    .bind(&verified.transfer.block_hash)
    .bind(
        verified
            .transfer
            .block_number
            .checked_add(verified.confirmations),
    )
    .bind(format!("policy {}", verified.policy_version))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

async fn commit_conflicts(
    transaction: &mut Transaction<'_, Postgres>,
    event: &ChainEventKey,
    conflicts: &[FieldConflict],
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    for conflict in conflicts {
        let conflict_id = sqlx::query_scalar::<_, Uuid>(
            r"
            INSERT INTO chain_observation_conflicts (
                id, chain, network, chain_environment, tx_hash, event_index, field, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (chain, network, chain_environment, tx_hash, event_index, field)
            DO UPDATE SET field = excluded.field
            RETURNING id
            ",
        )
        .bind(Uuid::now_v7())
        .bind(&event.chain)
        .bind(&event.network)
        .bind(event.chain_environment.as_str())
        .bind(event.tx_hash.as_str())
        .bind(event.event_index)
        .bind(conflict.field.as_str())
        .bind(now)
        .fetch_one(&mut **transaction)
        .await
        .map_err(unavailable)?;

        for (observation_id, value) in &conflict.items {
            sqlx::query(
                r"
                INSERT INTO chain_observation_conflict_items (conflict_id, observation_id, field_value)
                VALUES ($1, $2, $3)
                ON CONFLICT (conflict_id, observation_id) DO UPDATE SET field_value = excluded.field_value
                ",
            )
            .bind(conflict_id)
            .bind(observation_id)
            .bind(serde_json::json!({ "value": value }))
            .execute(&mut **transaction)
            .await
            .map_err(unavailable)?;
        }
    }
    Ok(())
}

fn conflict_fields(conflicts: &[FieldConflict]) -> String {
    conflicts
        .iter()
        .map(|conflict| conflict.field.as_str())
        .collect::<Vec<&str>>()
        .join(",")
}

#[allow(clippy::too_many_arguments)]
async fn record_verdict(
    transaction: &mut Transaction<'_, Postgres>,
    event: &ChainEventKey,
    verdict: &str,
    reason: Option<&str>,
    evidence_count: u32,
    independent_groups: u32,
    discarded_count: u32,
    transfer_id: Option<Uuid>,
    verifier_version: &str,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r"
        INSERT INTO chain_event_verdicts (
            chain, network, chain_environment, tx_hash, event_index, verdict, reason,
            evidence_count, independent_groups, discarded_count, transfer_id,
            verifier_version, attempts, decided_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 1, $13)
        ON CONFLICT (chain, network, chain_environment, tx_hash, event_index) DO UPDATE
           SET verdict = excluded.verdict,
               reason = excluded.reason,
               evidence_count = excluded.evidence_count,
               independent_groups = excluded.independent_groups,
               discarded_count = excluded.discarded_count,
               transfer_id = COALESCE(excluded.transfer_id, chain_event_verdicts.transfer_id),
               verifier_version = excluded.verifier_version,
               attempts = chain_event_verdicts.attempts + 1,
               decided_at = excluded.decided_at
        ",
    )
    .bind(&event.chain)
    .bind(&event.network)
    .bind(event.chain_environment.as_str())
    .bind(event.tx_hash.as_str())
    .bind(event.event_index)
    .bind(verdict)
    .bind(reason)
    .bind(i32::try_from(evidence_count).unwrap_or(i32::MAX))
    .bind(i32::try_from(independent_groups).unwrap_or(i32::MAX))
    .bind(i32::try_from(discarded_count).unwrap_or(i32::MAX))
    .bind(transfer_id)
    .bind(verifier_version)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(unavailable)?;
    Ok(())
}

#[cfg(test)]
mod tests;
