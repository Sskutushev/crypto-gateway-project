use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{SigningSecret, WebhookError, sign_event};
use serde_json::{Value, json};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Clock, RepositoryError};

/// One effect that must leave the system, written inside the transaction that
/// caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    pub id: Uuid,
    pub merchant_id: Option<Uuid>,
    pub event_type: String,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub payload: Value,
    pub attempts: i32,
    pub created_at: OffsetDateTime,
}

/// Where a merchant is told, and under which key version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub url: String,
    pub secret_version: i32,
    pub secret_fingerprint: [u8; 32],
}

/// What one delivery attempt did. Every variant is recorded; none of them is
/// silently treated as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryResult {
    /// The endpoint accepted the event.
    Accepted { status: u16, duration_ms: u32 },
    /// The endpoint answered, but refused the event.
    Refused {
        status: u16,
        duration_ms: u32,
        detail: String,
    },
    /// The endpoint could not be reached at all.
    Unreachable { detail: String, duration_ms: u32 },
}

#[async_trait]
pub trait WebhookSender: Send + Sync {
    /// Delivers one signed event to one endpoint.
    async fn deliver(
        &self,
        endpoint: &WebhookEndpoint,
        event_id: Uuid,
        body: &[u8],
        signature: &str,
    ) -> DeliveryResult;
}

/// One recorded delivery attempt against one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAttempt {
    pub event_id: Uuid,
    pub endpoint_id: Uuid,
    pub attempt: i32,
    /// The HTTP status, when the endpoint answered at all.
    pub status: Option<i32>,
    pub error: Option<String>,
    pub duration_ms: u32,
}

#[async_trait]
pub trait OutboxRepository: Send + Sync {
    /// Takes ownership of due webhook events for a bounded visibility window.
    async fn claim_due_events(
        &self,
        holder: &str,
        limit: u32,
        visibility_seconds: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<OutboxEvent>, RepositoryError>;

    async fn active_endpoints(
        &self,
        merchant_id: Uuid,
    ) -> Result<Vec<WebhookEndpoint>, RepositoryError>;

    async fn record_attempt(
        &self,
        attempt: &DeliveryAttempt,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError>;

    async fn mark_delivered(
        &self,
        event_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError>;

    async fn reschedule(
        &self,
        event_id: Uuid,
        available_at: OffsetDateTime,
        error: &str,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError>;

    async fn dead_letter(
        &self,
        event_id: Uuid,
        error: &str,
        now: OffsetDateTime,
    ) -> Result<(), RepositoryError>;
}

/// Counters for one delivery pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutboxReport {
    pub claimed: u32,
    pub delivered: u32,
    pub rescheduled: u32,
    pub dead_lettered: u32,
    pub endpoints_missing: u32,
}

/// Delivers what the money path promised, after the money path committed.
#[derive(Debug)]
pub struct OutboxService<R, S, C> {
    repository: Arc<R>,
    sender: Arc<S>,
    clock: C,
    master_key: Vec<u8>,
    holder: String,
    max_attempts: i32,
}

impl<R, S, C> OutboxService<R, S, C>
where
    R: OutboxRepository,
    S: WebhookSender,
    C: Clock,
{
    /// Builds the delivery service.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::WeakMasterKey`] when the deployment key is too
    /// short to sign with, because unverifiable signatures are worse than no
    /// delivery at all.
    pub fn new(
        repository: Arc<R>,
        sender: Arc<S>,
        clock: C,
        master_key: Vec<u8>,
        holder: impl Into<String>,
        max_attempts: i32,
    ) -> Result<Self, OutboxError> {
        if master_key.len() < 32 {
            return Err(OutboxError::Webhook(WebhookError::WeakMasterKey));
        }
        if max_attempts < 1 {
            return Err(OutboxError::InvalidRetryCeiling);
        }
        Ok(Self {
            repository,
            sender,
            clock,
            master_key,
            holder: holder.into(),
            max_attempts,
        })
    }

    /// Delivers a bounded batch of events.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError`] when the batch limit is invalid, storage fails,
    /// or a signing secret cannot be derived.
    pub async fn deliver_due(&self, limit: u32) -> Result<OutboxReport, OutboxError> {
        if limit == 0 || limit > 500 {
            return Err(OutboxError::InvalidBatchLimit);
        }
        let now = self.clock.now();
        let events = self
            .repository
            .claim_due_events(&self.holder, limit, 300, now)
            .await?;
        let mut report = OutboxReport::default();

        for event in events {
            report.claimed = report.claimed.saturating_add(1);
            let Some(merchant_id) = event.merchant_id else {
                // A webhook event without a merchant cannot be addressed; it
                // is a defect in whoever enqueued it, not a delivery problem.
                self.repository
                    .dead_letter(event.id, "webhook_event_without_merchant", self.clock.now())
                    .await?;
                report.dead_lettered = report.dead_lettered.saturating_add(1);
                continue;
            };
            let endpoints = self.repository.active_endpoints(merchant_id).await?;
            if endpoints.is_empty() {
                // Nobody is listening. Saying "delivered" would be a lie, so
                // the event is parked where an operator can see it.
                report.endpoints_missing = report.endpoints_missing.saturating_add(1);
                report.dead_lettered = report.dead_lettered.saturating_add(1);
                self.repository
                    .dead_letter(event.id, "no_active_endpoint", self.clock.now())
                    .await?;
                continue;
            }

            if self.deliver_to_all(&event, &endpoints).await? {
                self.repository
                    .mark_delivered(event.id, self.clock.now())
                    .await?;
                report.delivered = report.delivered.saturating_add(1);
            } else {
                let attempt = event.attempts.saturating_add(1);
                if attempt >= self.max_attempts {
                    self.repository
                        .dead_letter(event.id, "delivery_attempts_exhausted", self.clock.now())
                        .await?;
                    report.dead_lettered = report.dead_lettered.saturating_add(1);
                } else {
                    let available_at = self
                        .clock
                        .now()
                        .checked_add(backoff_for(attempt))
                        .ok_or(OutboxError::InvalidRetryCeiling)?;
                    self.repository
                        .reschedule(event.id, available_at, "delivery_failed", self.clock.now())
                        .await?;
                    report.rescheduled = report.rescheduled.saturating_add(1);
                }
            }
        }
        Ok(report)
    }

    /// Returns whether every endpoint accepted the event.
    async fn deliver_to_all(
        &self,
        event: &OutboxEvent,
        endpoints: &[WebhookEndpoint],
    ) -> Result<bool, OutboxError> {
        let body = serde_json::to_vec(&envelope(event))
            .map_err(|error| OutboxError::Serialization(error.to_string()))?;
        let mut all_accepted = true;

        for endpoint in endpoints {
            let secret = SigningSecret::derive(
                &self.master_key,
                endpoint.secret_version,
                endpoint.merchant_id,
                endpoint.id,
            )?;
            // A fingerprint mismatch means this deployment is holding a
            // different master key than the one the endpoint was issued under.
            // Signing anyway would produce a signature the merchant rejects.
            if secret.fingerprint() != endpoint.secret_fingerprint {
                self.repository
                    .record_attempt(
                        &DeliveryAttempt {
                            event_id: event.id,
                            endpoint_id: endpoint.id,
                            attempt: event.attempts.saturating_add(1),
                            status: None,
                            error: Some("signing_key_mismatch".to_owned()),
                            duration_ms: 0,
                        },
                        self.clock.now(),
                    )
                    .await?;
                all_accepted = false;
                continue;
            }

            let timestamp = self.clock.now().unix_timestamp();
            let signature = sign_event(&secret, timestamp, &body);
            let result = self
                .sender
                .deliver(endpoint, event.id, &body, &signature)
                .await;
            let (status, error, duration_ms, accepted) = match result {
                DeliveryResult::Accepted {
                    status,
                    duration_ms,
                } => (Some(i32::from(status)), None, duration_ms, true),
                DeliveryResult::Refused {
                    status,
                    duration_ms,
                    detail,
                } => (Some(i32::from(status)), Some(detail), duration_ms, false),
                DeliveryResult::Unreachable {
                    detail,
                    duration_ms,
                } => (None, Some(detail), duration_ms, false),
            };
            self.repository
                .record_attempt(
                    &DeliveryAttempt {
                        event_id: event.id,
                        endpoint_id: endpoint.id,
                        attempt: event.attempts.saturating_add(1),
                        status,
                        error,
                        duration_ms,
                    },
                    self.clock.now(),
                )
                .await?;
            if !accepted {
                all_accepted = false;
            }
        }
        Ok(all_accepted)
    }
}

/// The body a merchant receives. The event id is stable across retries, so a
/// merchant can deduplicate by it.
fn envelope(event: &OutboxEvent) -> Value {
    json!({
        "id": event.id,
        "type": event.event_type,
        "created_at": event.created_at.unix_timestamp(),
        "data": {
            "object": event.aggregate_type,
            "id": event.aggregate_id,
            "attributes": event.payload,
        },
    })
}

/// Exponential backoff between delivery attempts, capped so a dead endpoint is
/// retried on a human timescale instead of never.
fn backoff_for(attempt: i32) -> Duration {
    let doublings = attempt.clamp(1, 8).saturating_sub(1);
    let seconds = 60_i64.saturating_mul(1_i64 << doublings);
    Duration::seconds(seconds.min(6 * 60 * 60))
}

#[derive(Debug, Error)]
pub enum OutboxError {
    #[error("delivery batch limit must be between 1 and 500")]
    InvalidBatchLimit,
    #[error("the delivery retry ceiling must be at least one attempt")]
    InvalidRetryCeiling,
    #[error("an outbox event could not be serialized: {0}")]
    Serialization(String),
    #[error(transparent)]
    Webhook(#[from] WebhookError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl OutboxError {
    /// Reports whether a retry can plausibly succeed without operator action.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Repository(error) => error.is_transient(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
