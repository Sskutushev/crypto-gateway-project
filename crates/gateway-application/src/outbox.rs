use std::{future::Future, pin::Pin, sync::Arc, task::Poll};

use async_trait::async_trait;
use gateway_domain::{SigningSecret, WebhookError, sign_event_with_all};
use serde_json::{Value, json};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Clock, RepositoryError, telemetry};

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
    /// Set by a redelivery aimed at one endpoint: only that endpoint is
    /// called. `None` is every active endpoint of the merchant.
    pub target_endpoint_id: Option<Uuid>,
    /// The attempt count at the last redelivery. The retry ceiling counts
    /// attempts above it, while attempt numbers keep growing so no earlier
    /// delivery record is overwritten.
    pub attempt_floor: i32,
}

/// Where a merchant is told, and under which key version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub url: String,
    pub secret_version: i32,
    pub secret_fingerprint: [u8; 32],
    /// The previous secret while its transition period lasts; `None` once it
    /// has ended. Loaded only while still valid.
    pub previous_secret: Option<PreviousSecret>,
}

/// A rotated-out secret that still signs deliveries until its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviousSecret {
    pub version: i32,
    pub fingerprint: [u8; 32],
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
    /// Takes ownership of due webhook events for a bounded visibility window,
    /// at most `per_merchant_limit` of them for any one merchant, so a merchant
    /// with a deep backlog cannot fill every batch.
    async fn claim_due_events(
        &self,
        holder: &str,
        limit: u32,
        per_merchant_limit: u32,
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

impl OutboxReport {
    fn absorb(&mut self, other: Self) {
        self.claimed = self.claimed.saturating_add(other.claimed);
        self.delivered = self.delivered.saturating_add(other.delivered);
        self.rescheduled = self.rescheduled.saturating_add(other.rescheduled);
        self.dead_lettered = self.dead_lettered.saturating_add(other.dead_lettered);
        self.endpoints_missing = self
            .endpoints_missing
            .saturating_add(other.endpoints_missing);
    }
}

/// The largest batch one delivery pass may claim.
const MAX_BATCH_LIMIT: u32 = 500;

/// Runs `futures` with at most `limit` in flight and returns their outputs in
/// input order. A finished future frees its slot for the next one at once, so
/// one slow future occupies one slot and never the whole batch.
async fn bounded_join<F: Future>(futures: Vec<F>, limit: usize) -> Vec<F::Output> {
    let limit = limit.max(1);
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    let mut waiting = futures.into_iter().enumerate();
    let mut running: Vec<(usize, Pin<Box<F>>)> = Vec::new();
    std::future::poll_fn(|context| {
        loop {
            while running.len() < limit {
                match waiting.next() {
                    Some((index, future)) => running.push((index, Box::pin(future))),
                    None => break,
                }
            }
            if running.is_empty() {
                return Poll::Ready(());
            }
            let mut finished_any = false;
            let mut position = 0;
            while position < running.len() {
                if let Poll::Ready(output) = running[position].1.as_mut().poll(context) {
                    let (index, _) = running.swap_remove(position);
                    outputs[index] = Some(output);
                    finished_any = true;
                } else {
                    position += 1;
                }
            }
            if !finished_any {
                return Poll::Pending;
            }
        }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

/// How one batch is shared between merchants.
///
/// Every event of a merchant goes to all of that merchant's endpoints, so the
/// merchant is the unit a slow endpoint can hold back. A batch takes at most
/// `max_events_per_merchant` events of one merchant and delivers up to
/// `concurrency` merchants at the same time; one merchant's events stay in
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryFairness {
    pub max_events_per_merchant: u32,
    pub concurrency: usize,
}

impl Default for DeliveryFairness {
    fn default() -> Self {
        Self {
            max_events_per_merchant: 20,
            concurrency: 8,
        }
    }
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
    fairness: DeliveryFairness,
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
            fairness: DeliveryFairness::default(),
        })
    }

    /// Replaces the default per-merchant bound and concurrency.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::InvalidFairness`] when either bound is zero or
    /// outside its ceiling.
    pub fn with_fairness(mut self, fairness: DeliveryFairness) -> Result<Self, OutboxError> {
        if fairness.max_events_per_merchant == 0
            || fairness.max_events_per_merchant > MAX_BATCH_LIMIT
            || fairness.concurrency == 0
            || fairness.concurrency > 64
        {
            return Err(OutboxError::InvalidFairness);
        }
        self.fairness = fairness;
        Ok(self)
    }

    /// Delivers a bounded batch of events.
    ///
    /// Merchants are delivered concurrently, up to the configured bound, so an
    /// endpoint that answers slowly holds back only its own merchant's events.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError`] when the batch limit is invalid, storage fails,
    /// or a signing secret cannot be derived. Every merchant's delivery runs to
    /// completion before the first error is returned, so an attempt that was
    /// made is never left unrecorded because a neighbour failed.
    pub async fn deliver_due(&self, limit: u32) -> Result<OutboxReport, OutboxError> {
        if limit == 0 || limit > MAX_BATCH_LIMIT {
            return Err(OutboxError::InvalidBatchLimit);
        }
        let now = self.clock.now();
        let events = self
            .repository
            .claim_due_events(
                &self.holder,
                limit,
                self.fairness.max_events_per_merchant,
                300,
                now,
            )
            .await?;
        let mut groups: Vec<(Option<Uuid>, Vec<OutboxEvent>)> = Vec::new();
        for event in events {
            match groups
                .iter_mut()
                .find(|(merchant, _)| *merchant == event.merchant_id)
            {
                Some((_, queue)) => queue.push(event),
                None => groups.push((event.merchant_id, vec![event])),
            }
        }
        let deliveries = groups
            .into_iter()
            .map(|(merchant, queue)| self.deliver_merchant(merchant, queue))
            .collect();
        let mut report = OutboxReport::default();
        let mut first_error = None;
        for outcome in bounded_join(deliveries, self.fairness.concurrency).await {
            match outcome {
                Ok(part) => report.absorb(part),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(report),
        }
    }

    /// Delivers one merchant's share of the batch, in order.
    async fn deliver_merchant(
        &self,
        merchant: Option<Uuid>,
        events: Vec<OutboxEvent>,
    ) -> Result<OutboxReport, OutboxError> {
        let mut report = OutboxReport::default();
        let Some(merchant_id) = merchant else {
            for event in events {
                report.claimed = report.claimed.saturating_add(1);
                // A webhook event without a merchant cannot be addressed; it
                // is a defect in whoever enqueued it, not a delivery problem.
                self.repository
                    .dead_letter(event.id, "webhook_event_without_merchant", self.clock.now())
                    .await?;
                report.dead_lettered = report.dead_lettered.saturating_add(1);
            }
            return Ok(report);
        };
        let endpoints = self.repository.active_endpoints(merchant_id).await?;
        for event in events {
            report.claimed = report.claimed.saturating_add(1);
            let targets: Vec<WebhookEndpoint> = match event.target_endpoint_id {
                Some(target) => endpoints
                    .iter()
                    .filter(|endpoint| endpoint.id == target)
                    .cloned()
                    .collect(),
                None => endpoints.clone(),
            };
            if targets.is_empty() {
                // Nobody is listening. Saying "delivered" would be a lie, so
                // the event is parked where an operator can see it.
                let reason = if event.target_endpoint_id.is_some() {
                    "redelivery_target_inactive"
                } else {
                    "no_active_endpoint"
                };
                report.endpoints_missing = report.endpoints_missing.saturating_add(1);
                report.dead_lettered = report.dead_lettered.saturating_add(1);
                self.repository
                    .dead_letter(event.id, reason, self.clock.now())
                    .await?;
                continue;
            }

            if event.attempts == 0 {
                // A negative delay is a clock behind the database's; it is
                // reported as no delay rather than dropped.
                let delay = std::time::Duration::try_from(self.clock.now() - event.created_at)
                    .unwrap_or_default();
                telemetry::record_first_attempt_delay(delay);
            }
            if self.deliver_to_all(&event, &targets).await? {
                self.repository
                    .mark_delivered(event.id, self.clock.now())
                    .await?;
                report.delivered = report.delivered.saturating_add(1);
            } else {
                let used = event
                    .attempts
                    .saturating_sub(event.attempt_floor)
                    .saturating_add(1);
                if used >= self.max_attempts {
                    self.repository
                        .dead_letter(event.id, "delivery_attempts_exhausted", self.clock.now())
                        .await?;
                    report.dead_lettered = report.dead_lettered.saturating_add(1);
                } else {
                    let available_at = self
                        .clock
                        .now()
                        .checked_add(backoff_for(used))
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

            // During a rotation the previous secret signs too, so a merchant
            // still verifying with it keeps accepting events. A previous
            // secret that no longer matches its fingerprint is left out, not
            // used: the current signature alone is still correct.
            let previous = match endpoint.previous_secret {
                Some(previous) => {
                    let secret = SigningSecret::derive(
                        &self.master_key,
                        previous.version,
                        endpoint.merchant_id,
                        endpoint.id,
                    )?;
                    (secret.fingerprint() == previous.fingerprint).then_some(secret)
                }
                None => None,
            };
            let timestamp = self.clock.now().unix_timestamp();
            let mut signing = vec![&secret];
            if let Some(previous) = previous.as_ref() {
                signing.push(previous);
            }
            let signature = sign_event_with_all(&signing, timestamp, &body);
            let result = self
                .sender
                .deliver(endpoint, event.id, &body, &signature)
                .await;
            let (status, error, duration_ms, accepted, outcome) = match result {
                DeliveryResult::Accepted {
                    status,
                    duration_ms,
                } => (Some(i32::from(status)), None, duration_ms, true, "accepted"),
                DeliveryResult::Refused {
                    status,
                    duration_ms,
                    detail,
                } => (
                    Some(i32::from(status)),
                    Some(detail),
                    duration_ms,
                    false,
                    "refused",
                ),
                DeliveryResult::Unreachable {
                    detail,
                    duration_ms,
                } => (None, Some(detail), duration_ms, false, "unreachable"),
            };
            telemetry::record_delivery(
                outcome,
                std::time::Duration::from_millis(u64::from(duration_ms)),
            );
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
    #[error("the per-merchant bound must be 1..=500 and the concurrency 1..=64")]
    InvalidFairness,
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
