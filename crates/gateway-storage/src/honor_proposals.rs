//! Storage for two-operator honors. A proposal is checked against locked rows
//! and stored without money; an approval re-checks the same rows and runs the
//! honor in the transaction that closes the proposal.

use async_trait::async_trait;
use gateway_application::{
    HonorProposal, HonorProposalRepository, ManualResolution, ManualResolutionResult,
    OperationsError, OperatorCredential, PageRequest, ProposalStatus, RepositoryError,
};
use gateway_domain::{ManualResolutionAction, RawAmount};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    operations::{honor_transfer, load_resolution, parse_raw, plan_honor},
    postgres::{PostgresRepository, corrupt, unavailable},
};

const PROPOSAL_COLUMNS: &str = "id, transfer_id, payment_intent_id, attempt_id, merchant_id, \
     allocate_raw::text AS allocate_raw, threshold_raw::text AS threshold_raw, reason, evidence, \
     proposer_key_id, proposer_label, request_hash, status, created_at, \
     expires_at, decided_by_key_id, decided_by_label, decision_idempotency_key, \
     decision_reason, decided_at, resolution_id";

#[derive(Debug, FromRow)]
struct ProposalRow {
    id: Uuid,
    transfer_id: Uuid,
    payment_intent_id: Uuid,
    attempt_id: Uuid,
    merchant_id: Uuid,
    allocate_raw: String,
    threshold_raw: String,
    reason: String,
    evidence: Value,
    proposer_key_id: Uuid,
    proposer_label: String,
    request_hash: Vec<u8>,
    status: String,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    decided_by_key_id: Option<Uuid>,
    decided_by_label: Option<String>,
    decision_idempotency_key: Option<String>,
    decision_reason: Option<String>,
    decided_at: Option<OffsetDateTime>,
    resolution_id: Option<Uuid>,
}

impl ProposalRow {
    fn into_proposal(self, replayed: bool) -> Result<HonorProposal, OperationsError> {
        Ok(HonorProposal {
            id: self.id,
            transfer_id: self.transfer_id,
            payment_intent_id: self.payment_intent_id,
            attempt_id: self.attempt_id,
            merchant_id: self.merchant_id,
            allocate_raw: parse_raw(&self.allocate_raw)?,
            threshold_raw: if self.threshold_raw == "0" {
                RawAmount::ZERO
            } else {
                parse_raw(&self.threshold_raw)?
            },
            reason: self.reason,
            evidence: self.evidence,
            proposed_by_key_id: self.proposer_key_id,
            proposed_by_label: self.proposer_label,
            status: ProposalStatus::parse(&self.status)?,
            created_at: self.created_at,
            expires_at: self.expires_at,
            decided_by_key_id: self.decided_by_key_id,
            decided_by_label: self.decided_by_label,
            decision_reason: self.decision_reason,
            decided_at: self.decided_at,
            resolution_id: self.resolution_id,
            replayed,
        })
    }

    fn request_hash(&self) -> Result<[u8; 32], OperationsError> {
        self.request_hash
            .as_slice()
            .try_into()
            .map_err(|_| OperationsError::Repository(corrupt("a proposal hash is not 32 bytes")))
    }
}

fn storage(error: sqlx::Error) -> OperationsError {
    OperationsError::Repository(unavailable(error))
}

async fn lock_scope(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
) -> Result<(), OperationsError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(scope)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    Ok(())
}

async fn locked_proposal(
    tx: &mut Transaction<'_, Postgres>,
    proposal_id: Uuid,
) -> Result<ProposalRow, OperationsError> {
    sqlx::query_as::<_, ProposalRow>(&format!(
        "SELECT {PROPOSAL_COLUMNS} FROM manual_honor_proposals WHERE id = $1 FOR UPDATE"
    ))
    .bind(proposal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or(OperationsError::HonorProposalNotFound)
}

#[allow(clippy::too_many_arguments)]
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    operator: &OperatorCredential,
    merchant_id: Uuid,
    action: &str,
    proposal_id: Uuid,
    reason: &str,
    payload: Value,
    now: OffsetDateTime,
) -> Result<(), OperationsError> {
    sqlx::query(
        r"INSERT INTO audit_events (id, merchant_id, actor_type, actor_id, action, resource_type,
              resource_id, reason, payload, created_at)
          VALUES ($1, $2, 'operator', $3, $4, 'manual_honor_proposal', $5, $6, $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(merchant_id)
    .bind(operator.key_id)
    .bind(action)
    .bind(proposal_id)
    .bind(reason)
    .bind(payload)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

#[async_trait]
impl HonorProposalRepository for PostgresRepository {
    // One transaction, read top to bottom: replay, check, store, audit.
    #[allow(clippy::too_many_lines)]
    async fn propose_honor(
        &self,
        proposer: &OperatorCredential,
        idempotency_key: &str,
        request_hash: &[u8; 32],
        resolution: &ManualResolution,
        threshold_raw: RawAmount,
        now: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> Result<HonorProposal, OperationsError> {
        let (ManualResolutionAction::Honor, Some(intent_id), Some(attempt_id), Some(allocate)) = (
            resolution.action,
            resolution.payment_intent_id,
            resolution.attempt_id,
            resolution.allocate_raw,
        ) else {
            return Err(OperationsError::InvalidManualResolution);
        };
        let mut tx = self.begin().await.map_err(storage)?;
        lock_scope(
            &mut tx,
            &format!("honor_proposal:{}:{idempotency_key}", proposer.key_id),
        )
        .await?;
        let existing = sqlx::query_as::<_, ProposalRow>(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM manual_honor_proposals \
              WHERE proposer_key_id = $1 AND idempotency_key = $2"
        ))
        .bind(proposer.key_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        if let Some(existing) = existing {
            if existing.request_hash.as_slice() != request_hash {
                return Err(OperationsError::Repository(
                    RepositoryError::IdempotencyConflict,
                ));
            }
            tx.commit().await.map_err(storage)?;
            return existing.into_proposal(true);
        }

        // Every check an honor makes, made now: an honor that would be refused
        // is not put in front of a second person.
        let plan = plan_honor(&mut tx, resolution).await?;
        // A proposal nobody approved in time no longer blocks a new one.
        sqlx::query(
            r"UPDATE manual_honor_proposals SET status = 'expired', decided_at = $2
               WHERE transfer_id = $1 AND status = 'pending' AND expires_at <= $2",
        )
        .bind(resolution.transfer_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let id = Uuid::now_v7();
        let inserted = sqlx::query_as::<_, ProposalRow>(&format!(
            r"INSERT INTO manual_honor_proposals (
                  id, proposer_key_id, proposer_label, idempotency_key, request_hash,
                  transfer_id, payment_intent_id, attempt_id, merchant_id, allocate_raw,
                  threshold_raw, reason, evidence, status, created_at, expires_at)
              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CAST($10 AS NUMERIC),
                      CAST($11 AS NUMERIC), $12, $13, 'pending', $14, $15)
              RETURNING {PROPOSAL_COLUMNS}"
        ))
        .bind(id)
        .bind(proposer.key_id)
        .bind(&proposer.label)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .bind(resolution.transfer_id)
        .bind(intent_id)
        .bind(attempt_id)
        .bind(plan.row.merchant_id)
        .bind(allocate.to_string())
        .bind(threshold_raw.to_string())
        .bind(&resolution.reason)
        .bind(plan.evidence())
        .bind(now)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
            {
                // Another proposal for the same transfer is still waiting.
                OperationsError::ManualResolutionConflict
            } else {
                storage(error)
            }
        })?;
        audit(
            &mut tx,
            proposer,
            plan.row.merchant_id,
            "honor.propose",
            id,
            &resolution.reason,
            json!({
                "transfer_id": resolution.transfer_id,
                "payment_intent_id": intent_id,
                "attempt_id": attempt_id,
                "allocate_raw": allocate.to_string(),
                "threshold_raw": threshold_raw.to_string(),
                "expires_at": expires_at,
            }),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        inserted.into_proposal(false)
    }

    async fn approve_honor(
        &self,
        approver: &OperatorCredential,
        proposal_id: Uuid,
        idempotency_key: &str,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<ManualResolutionResult, OperationsError> {
        let mut tx = self.begin().await.map_err(storage)?;
        // The same scope as a direct manual resolution under this key, so the
        // key cannot be spent twice on two different decisions at once.
        lock_scope(&mut tx, &format!("{}:{idempotency_key}", approver.key_id)).await?;
        let spent = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM manual_resolution_requests WHERE operator_key_id = $1 AND idempotency_key = $2",
        )
        .bind(approver.key_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let proposal = locked_proposal(&mut tx, proposal_id).await?;
        let hash = proposal.request_hash()?;
        if let Some(resolution_id) = spent {
            if proposal.resolution_id != Some(resolution_id) {
                return Err(OperationsError::Repository(
                    RepositoryError::IdempotencyConflict,
                ));
            }
            let replay =
                load_resolution(&mut tx, approver.key_id, idempotency_key, &hash, true).await?;
            tx.commit().await.map_err(storage)?;
            return Ok(replay);
        }
        if approver.key_id == proposal.proposer_key_id {
            return Err(OperationsError::SameOperator);
        }
        if ProposalStatus::parse(&proposal.status)? != ProposalStatus::Pending {
            return Err(OperationsError::HonorProposalNotPending);
        }
        if now >= proposal.expires_at {
            return Err(OperationsError::HonorProposalExpired);
        }
        let resolution = ManualResolution {
            action: ManualResolutionAction::Honor,
            transfer_id: proposal.transfer_id,
            payment_intent_id: Some(proposal.payment_intent_id),
            attempt_id: Some(proposal.attempt_id),
            allocate_raw: Some(parse_raw(&proposal.allocate_raw)?),
            remainder_raw: None,
            disposition: None,
            external_reference: None,
            reason: proposal.reason.clone(),
        };
        // The honor itself locks and re-reads the transfer, attempt and intent
        // and refuses anything that changed since the proposal.
        let result =
            honor_transfer(&mut tx, approver, idempotency_key, &hash, &resolution, now).await?;
        sqlx::query(
            r"UPDATE manual_honor_proposals
                 SET status = 'approved', decided_by_key_id = $2, decided_by_label = $3,
                     decision_idempotency_key = $4, decision_reason = $5, decided_at = $6,
                     resolution_id = $7
               WHERE id = $1",
        )
        .bind(proposal_id)
        .bind(approver.key_id)
        .bind(&approver.label)
        .bind(idempotency_key)
        .bind(reason)
        .bind(now)
        .bind(result.id)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        audit(
            &mut tx,
            approver,
            proposal.merchant_id,
            "honor.approve",
            proposal_id,
            reason,
            json!({
                "transfer_id": proposal.transfer_id,
                "proposed_by": proposal.proposer_key_id,
                "resolution_id": result.id,
                "allocated_raw": proposal.allocate_raw,
            }),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }

    async fn reject_honor(
        &self,
        operator: &OperatorCredential,
        proposal_id: Uuid,
        idempotency_key: &str,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<HonorProposal, OperationsError> {
        let mut tx = self.begin().await.map_err(storage)?;
        let proposal = locked_proposal(&mut tx, proposal_id).await?;
        let status = ProposalStatus::parse(&proposal.status)?;
        if status == ProposalStatus::Rejected
            && proposal.decided_by_key_id == Some(operator.key_id)
            && proposal.decision_idempotency_key.as_deref() == Some(idempotency_key)
        {
            if proposal.decision_reason.as_deref() != Some(reason) {
                return Err(OperationsError::Repository(
                    RepositoryError::IdempotencyConflict,
                ));
            }
            tx.commit().await.map_err(storage)?;
            return proposal.into_proposal(true);
        }
        if status != ProposalStatus::Pending {
            return Err(OperationsError::HonorProposalNotPending);
        }
        let rejected = sqlx::query_as::<_, ProposalRow>(&format!(
            r"UPDATE manual_honor_proposals
                 SET status = 'rejected', decided_by_key_id = $2, decided_by_label = $3,
                     decision_idempotency_key = $4, decision_reason = $5, decided_at = $6
               WHERE id = $1
              RETURNING {PROPOSAL_COLUMNS}"
        ))
        .bind(proposal_id)
        .bind(operator.key_id)
        .bind(&operator.label)
        .bind(idempotency_key)
        .bind(reason)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        audit(
            &mut tx,
            operator,
            proposal.merchant_id,
            "honor.reject",
            proposal_id,
            reason,
            json!({
                "transfer_id": proposal.transfer_id,
                "proposed_by": proposal.proposer_key_id,
            }),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        rejected.into_proposal(false)
    }

    async fn pending_honor_proposals(
        &self,
        page: PageRequest,
        now: OffsetDateTime,
    ) -> Result<Vec<HonorProposal>, RepositoryError> {
        let rows = sqlx::query_as::<_, ProposalRow>(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM manual_honor_proposals \
              WHERE status = 'pending' AND expires_at > $1 \
                AND ($2::UUID IS NULL OR id < $2) \
              ORDER BY id DESC LIMIT $3"
        ))
        .bind(now)
        .bind(page.before)
        .bind(i64::from(page.limit) + 1)
        .fetch_all(self.pool())
        .await
        .map_err(unavailable)?;
        rows.into_iter()
            .map(|row| {
                row.into_proposal(false).map_err(|error| match error {
                    OperationsError::Repository(inner) => inner,
                    other => corrupt(other.to_string()),
                })
            })
            .collect()
    }
}
