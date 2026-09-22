use async_trait::async_trait;
use gateway_application::{
    ChainSource, CollectorState, CollectorWatch, ComponentLease, CursorKind, CursorPosition,
    IntakeReport, LeaseRepository, ObservationRepository, RepositoryError, ResolvedObservation,
    SourceKind, SourceState,
};
use gateway_domain::{AddressKey, ChainEnvironment};
use sqlx::{FromRow, Postgres, Transaction};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::postgres::{PostgresRepository, corrupt, unavailable};

#[derive(Debug, FromRow)]
struct ChainSourceRow {
    id: Uuid,
    chain: String,
    network: String,
    chain_environment: String,
    source_key: String,
    provider_group: String,
    kind: String,
    db_principal: String,
    state: String,
}

impl TryFrom<ChainSourceRow> for ChainSource {
    type Error = RepositoryError;

    fn try_from(row: ChainSourceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            chain: row.chain,
            network: row.network,
            chain_environment: row
                .chain_environment
                .parse::<ChainEnvironment>()
                .map_err(|error| corrupt(error.to_string()))?,
            source_key: row.source_key,
            provider_group: row.provider_group,
            kind: SourceKind::parse(&row.kind).map_err(|error| corrupt(error.to_string()))?,
            db_principal: row.db_principal,
            state: SourceState::parse(&row.state).map_err(|error| corrupt(error.to_string()))?,
        })
    }
}

#[derive(Debug, FromRow)]
struct CollectorWatchRow {
    collector_address_id: Uuid,
    address_key: Vec<u8>,
    address_text: String,
    asset_id: Uuid,
    token_key: Vec<u8>,
    decimals: i16,
    token_display: String,
    chain: String,
    network: String,
    chain_environment: String,
    state: String,
}

impl TryFrom<CollectorWatchRow> for CollectorWatch {
    type Error = RepositoryError;

    fn try_from(row: CollectorWatchRow) -> Result<Self, Self::Error> {
        Ok(Self {
            collector_address_id: row.collector_address_id,
            address_key: AddressKey::new(row.address_key)
                .map_err(|error| corrupt(error.to_string()))?,
            address_text: row.address_text,
            asset_id: row.asset_id,
            token_key: AddressKey::new(row.token_key)
                .map_err(|error| corrupt(error.to_string()))?,
            decimals: row.decimals,
            token_display: row.token_display,
            chain: row.chain,
            network: row.network,
            chain_environment: row
                .chain_environment
                .parse::<ChainEnvironment>()
                .map_err(|error| corrupt(error.to_string()))?,
            state: CollectorState::parse(&row.state).map_err(|error| corrupt(error.to_string()))?,
        })
    }
}

#[derive(Debug, FromRow)]
struct CursorRow {
    cursor_kind: String,
    cursor_value: String,
    last_block_hash: Option<String>,
    fence_token: i64,
}

impl TryFrom<CursorRow> for CursorPosition {
    type Error = RepositoryError;

    fn try_from(row: CursorRow) -> Result<Self, Self::Error> {
        let kind = match row.cursor_kind.as_str() {
            "block" => CursorKind::Block,
            "logical_time" => CursorKind::LogicalTime,
            "event_position" => CursorKind::EventPosition,
            other => return Err(corrupt(format!("unknown cursor kind: {other}"))),
        };
        Self::new(kind, row.cursor_value, row.last_block_hash, row.fence_token)
            .map_err(|error| corrupt(error.to_string()))
    }
}

#[async_trait]
impl LeaseRepository for PostgresRepository {
    async fn acquire_component_lease(
        &self,
        component: &str,
        holder: &str,
        ttl_seconds: i64,
        now: OffsetDateTime,
    ) -> Result<Option<ComponentLease>, RepositoryError> {
        if ttl_seconds <= 0 {
            return Err(corrupt("a component lease needs a positive duration"));
        }
        let lease_until = now
            .checked_add(Duration::seconds(ttl_seconds))
            .ok_or_else(|| corrupt("component lease duration overflowed"))?;

        // Renewal keeps the token; a takeover increments it, so a frozen
        // predecessor's writes carry a token the database can refuse.
        let row = sqlx::query_as::<_, (String, String, i64, OffsetDateTime)>(
            r"
            INSERT INTO component_leases (component, holder, fence_token, lease_until, updated_at)
            VALUES ($1, $2, 1, $3, $4)
            ON CONFLICT (component) DO UPDATE
               SET holder = excluded.holder,
                   lease_until = excluded.lease_until,
                   updated_at = excluded.updated_at,
                   fence_token = CASE
                       WHEN component_leases.holder = excluded.holder
                       THEN component_leases.fence_token
                       ELSE component_leases.fence_token + 1
                   END
             WHERE component_leases.holder = excluded.holder
                OR component_leases.lease_until < excluded.updated_at
            RETURNING component, holder, fence_token, lease_until
            ",
        )
        .bind(component)
        .bind(holder)
        .bind(lease_until)
        .bind(now)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        Ok(row.map(
            |(component, holder, fence_token, lease_until)| ComponentLease {
                component,
                holder,
                fence_token,
                lease_until,
            },
        ))
    }
}

#[async_trait]
impl ObservationRepository for PostgresRepository {
    async fn find_source(&self, source_key: &str) -> Result<Option<ChainSource>, RepositoryError> {
        let rows = sqlx::query_as::<_, ChainSourceRow>(
            r"
            SELECT id, chain, network, chain_environment, source_key, provider_group,
                   kind, db_principal, state
              FROM chain_sources
             WHERE source_key = $1
             ORDER BY created_at
             LIMIT 2
            ",
        )
        .bind(source_key)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        if rows.len() > 1 {
            return Err(corrupt(format!(
                "source key {source_key} is not unique across chains"
            )));
        }
        rows.into_iter().next().map(TryInto::try_into).transpose()
    }

    async fn watched_collectors(
        &self,
        chain: &str,
        network: &str,
        environment: ChainEnvironment,
    ) -> Result<Vec<CollectorWatch>, RepositoryError> {
        let rows = sqlx::query_as::<_, CollectorWatchRow>(
            r"
            SELECT collector.id AS collector_address_id,
                   collector.address_key,
                   collector.address_text,
                   asset.id AS asset_id,
                   asset.contract_address_key AS token_key,
                   asset.decimals,
                   asset.display_symbol AS token_display,
                   asset.chain,
                   asset.network,
                   asset.chain_environment,
                   collector.state
              FROM collector_addresses AS collector
              JOIN chain_assets AS asset ON asset.id = collector.asset_id
             WHERE asset.chain = $1
               AND asset.network = $2
               AND asset.chain_environment = $3
               AND asset.status = 'active'
               AND collector.state IN ('active', 'receiving_only')
             ORDER BY collector.valid_from, collector.id
            ",
        )
        .bind(chain)
        .bind(network)
        .bind(environment.as_str())
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn find_cursor(
        &self,
        source_id: Uuid,
        kind: gateway_domain::ObservationKind,
        collector_address_id: Uuid,
    ) -> Result<Option<CursorPosition>, RepositoryError> {
        let row = sqlx::query_as::<_, CursorRow>(
            r"
            SELECT cursor_kind, cursor_value, last_block_hash, fence_token
              FROM chain_cursors
             WHERE source_id = $1 AND observation_kind = $2 AND collector_address_id = $3
            ",
        )
        .bind(source_id)
        .bind(kind.as_str())
        .bind(collector_address_id)
        .fetch_optional(self.pool())
        .await
        .map_err(unavailable)?;

        row.map(TryInto::try_into).transpose()
    }

    async fn record_observations(
        &self,
        source: &ChainSource,
        lease: &ComponentLease,
        observations: &[ResolvedObservation],
        cursor: Option<(Uuid, gateway_domain::ObservationKind, CursorPosition)>,
    ) -> Result<IntakeReport, RepositoryError> {
        let mut transaction = self.pool().begin().await.map_err(unavailable)?;
        hold_lease(&mut transaction, lease).await?;

        let mut recorded = 0_u32;
        let mut duplicates = 0_u32;
        for observation in observations {
            if insert_observation(&mut transaction, source, observation).await? {
                recorded = recorded.saturating_add(1);
            } else {
                duplicates = duplicates.saturating_add(1);
            }
        }

        let cursor_advanced = match cursor {
            Some((collector_address_id, kind, position)) => {
                advance_cursor(
                    &mut transaction,
                    source.id,
                    kind,
                    collector_address_id,
                    &position,
                )
                .await?
            }
            None => false,
        };

        transaction.commit().await.map_err(unavailable)?;
        Ok(IntakeReport {
            recorded,
            duplicates,
            refused: 0,
            cursor_advanced,
        })
    }
}

/// Reads the lease inside the writing transaction.
///
/// Under a concurrent takeover this read conflicts with the new holder's
/// write, so a frozen predecessor fails instead of writing silently.
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

async fn insert_observation(
    transaction: &mut Transaction<'_, Postgres>,
    source: &ChainSource,
    observation: &ResolvedObservation,
) -> Result<bool, RepositoryError> {
    let transfer = &observation.transfer;
    // `source_principal` is deliberately absent: the column default records the
    // authenticated database principal, which the writer cannot forge.
    let inserted = sqlx::query_scalar::<_, Uuid>(
        r"
        INSERT INTO chain_observations (
            id, source_id, asset_id, collector_address_id, chain, network,
            chain_environment, observation_kind, tx_hash, event_index,
            block_number, block_hash, parent_hash, block_time, token_key,
            token_display, from_address_key, from_address_text, to_address_key,
            to_address_text, amount_raw, decimals, memo, execution_status,
            source_finality, source_head, evidence_sha256, evidence_uri,
            observer_version, parser_version, fence_token, semantic_hash,
            observed_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
            $16, $17, $18, $19, $20, CAST($21 AS NUMERIC), $22, $23, $24, $25,
            $26, $27, $28, $29, $30, $31, $32, $33
        )
        ON CONFLICT (source_id, semantic_hash) DO NOTHING
        RETURNING id
        ",
    )
    .bind(Uuid::now_v7())
    .bind(source.id)
    .bind(observation.asset_id)
    .bind(observation.collector_address_id)
    .bind(&transfer.chain)
    .bind(&transfer.network)
    .bind(transfer.chain_environment.as_str())
    .bind(observation.kind.as_str())
    .bind(transfer.tx_hash.as_str())
    .bind(transfer.event_index)
    .bind(transfer.block_number)
    .bind(transfer.block_hash.as_deref())
    .bind(transfer.parent_hash.as_deref())
    .bind(transfer.block_time)
    .bind(transfer.token_key.as_bytes())
    .bind(&transfer.token_display)
    .bind(transfer.from_address.as_bytes())
    .bind(&transfer.from_address_text)
    .bind(transfer.to_address.as_bytes())
    .bind(&transfer.to_address_text)
    .bind(transfer.amount_raw.to_string())
    .bind(transfer.decimals)
    .bind(transfer.memo.as_ref().map(gateway_domain::Memo::as_str))
    .bind(transfer.execution_status.as_str())
    .bind(transfer.source_finality.as_str())
    .bind(transfer.source_head)
    .bind(&transfer.evidence_sha256)
    .bind(transfer.evidence_uri.as_deref())
    .bind(&observation.observer_version)
    .bind(&observation.parser_version)
    .bind(observation.fence_token)
    .bind(observation.semantic_hash.as_slice())
    .bind(observation.observed_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;

    Ok(inserted.is_some())
}

async fn advance_cursor(
    transaction: &mut Transaction<'_, Postgres>,
    source_id: Uuid,
    kind: gateway_domain::ObservationKind,
    collector_address_id: Uuid,
    position: &CursorPosition,
) -> Result<bool, RepositoryError> {
    let advanced = sqlx::query_scalar::<_, i32>(
        r"
        INSERT INTO chain_cursors (
            source_id, observation_kind, collector_address_id, cursor_kind,
            cursor_value, last_block_hash, fence_token, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now())
        ON CONFLICT (source_id, observation_kind, collector_address_id) DO UPDATE
           SET cursor_kind = excluded.cursor_kind,
               cursor_value = excluded.cursor_value,
               last_block_hash = excluded.last_block_hash,
               fence_token = excluded.fence_token,
               updated_at = excluded.updated_at
         WHERE chain_cursors.fence_token <= excluded.fence_token
           AND (
                excluded.cursor_kind NOT IN ('block', 'logical_time')
                OR chain_cursors.cursor_value::NUMERIC <= excluded.cursor_value::NUMERIC
               )
        RETURNING 1
        ",
    )
    .bind(source_id)
    .bind(kind.as_str())
    .bind(collector_address_id)
    .bind(position.kind.as_str())
    .bind(&position.value)
    .bind(position.last_block_hash.as_deref())
    .bind(position.fence_token)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;

    if advanced.is_some() {
        return Ok(true);
    }

    // The upsert refused. A newer fence token means another holder owns this
    // lane; anything else is a cursor that simply did not move forward.
    let holder_token = sqlx::query_scalar::<_, i64>(
        r"
        SELECT fence_token
          FROM chain_cursors
         WHERE source_id = $1 AND observation_kind = $2 AND collector_address_id = $3
        ",
    )
    .bind(source_id)
    .bind(kind.as_str())
    .bind(collector_address_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(unavailable)?;

    if holder_token.is_some_and(|token| token > position.fence_token) {
        return Err(RepositoryError::LeaseLost);
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
