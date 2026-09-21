use std::{
    error::Error,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_domain::SigningSecret;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    DeliveryAttempt, DeliveryResult, OutboxEvent, OutboxRepository, OutboxService, WebhookEndpoint,
    WebhookSender,
};
use crate::{Clock, RepositoryError};

type TestResult = Result<(), Box<dyn Error>>;

/// event id, HTTP status, error detail
type RecordedAttempt = (Uuid, Option<i32>, Option<String>);

const MERCHANT: Uuid = Uuid::from_u128(1);
const ENDPOINT: Uuid = Uuid::from_u128(2);
const EVENT: Uuid = Uuid::from_u128(3);

fn master_key() -> Vec<u8> {
    vec![5_u8; 32]
}

#[derive(Debug, Clone, Copy)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000)
    }
}

#[derive(Debug, Default)]
struct FakeRepository {
    events: Mutex<Vec<OutboxEvent>>,
    endpoints: Mutex<Vec<WebhookEndpoint>>,
    attempts: Mutex<Vec<RecordedAttempt>>,
    terminal: Mutex<Vec<(Uuid, String)>>,
}

impl FakeRepository {
    fn with(events: Vec<OutboxEvent>, endpoints: Vec<WebhookEndpoint>) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(events),
            endpoints: Mutex::new(endpoints),
            attempts: Mutex::new(Vec::new()),
            terminal: Mutex::new(Vec::new()),
        })
    }

    fn attempts(&self) -> Vec<RecordedAttempt> {
        self.attempts
            .lock()
            .map(|attempts| attempts.clone())
            .unwrap_or_default()
    }

    fn terminal(&self) -> Vec<(Uuid, String)> {
        self.terminal
            .lock()
            .map(|terminal| terminal.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl OutboxRepository for FakeRepository {
    async fn claim_due_events(
        &self,
        _holder: &str,
        _limit: u32,
        _visibility_seconds: i64,
        _now: OffsetDateTime,
    ) -> Result<Vec<OutboxEvent>, RepositoryError> {
        Ok(self
            .events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default())
    }

    async fn active_endpoints(
        &self,
        _merchant_id: Uuid,
    ) -> Result<Vec<WebhookEndpoint>, RepositoryError> {
        Ok(self
            .endpoints
            .lock()
            .map(|endpoints| endpoints.clone())
            .unwrap_or_default())
    }

    async fn record_attempt(
        &self,
        attempt: &DeliveryAttempt,
        _now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        self.attempts
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push((attempt.event_id, attempt.status, attempt.error.clone()));
        Ok(())
    }

    async fn mark_delivered(
        &self,
        event_id: Uuid,
        _now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        self.terminal
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push((event_id, "delivered".to_owned()));
        Ok(())
    }

    async fn reschedule(
        &self,
        event_id: Uuid,
        _available_at: OffsetDateTime,
        error: &str,
        _now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        self.terminal
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push((event_id, format!("rescheduled:{error}")));
        Ok(())
    }

    async fn dead_letter(
        &self,
        event_id: Uuid,
        error: &str,
        _now: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        self.terminal
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push((event_id, format!("dead_letter:{error}")));
        Ok(())
    }
}

#[derive(Debug)]
struct ScriptedSender {
    result: DeliveryResult,
    seen: Mutex<Vec<(String, Vec<u8>)>>,
}

impl ScriptedSender {
    fn new(result: DeliveryResult) -> Arc<Self> {
        Arc::new(Self {
            result,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<(String, Vec<u8>)> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl WebhookSender for ScriptedSender {
    async fn deliver(
        &self,
        _endpoint: &WebhookEndpoint,
        _event_id: Uuid,
        body: &[u8],
        signature: &str,
    ) -> DeliveryResult {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((signature.to_owned(), body.to_vec()));
        }
        self.result.clone()
    }
}

fn event() -> OutboxEvent {
    OutboxEvent {
        id: EVENT,
        merchant_id: Some(MERCHANT),
        event_type: "payment_intent.paid".to_owned(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: Uuid::from_u128(9),
        payload: json!({"transfer_id": "abc"}),
        attempts: 0,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn endpoint() -> Result<WebhookEndpoint, Box<dyn Error>> {
    let secret = SigningSecret::derive(&master_key(), 1, MERCHANT, ENDPOINT)?;
    Ok(WebhookEndpoint {
        id: ENDPOINT,
        merchant_id: MERCHANT,
        url: "https://merchant.example/hooks".to_owned(),
        secret_version: 1,
        secret_fingerprint: secret.fingerprint(),
    })
}

fn service(
    repository: Arc<FakeRepository>,
    sender: Arc<ScriptedSender>,
) -> Result<OutboxService<FakeRepository, ScriptedSender, FixedClock>, Box<dyn Error>> {
    Ok(OutboxService::new(
        repository,
        sender,
        FixedClock,
        master_key(),
        "pod:boot",
        5,
    )?)
}

#[tokio::test]
async fn an_accepted_event_is_signed_and_marked_delivered() -> TestResult {
    let repository = FakeRepository::with(vec![event()], vec![endpoint()?]);
    let sender = ScriptedSender::new(DeliveryResult::Accepted {
        status: 200,
        duration_ms: 12,
    });
    let service = service(Arc::clone(&repository), Arc::clone(&sender))?;

    let report = service.deliver_due(10).await?;

    assert_eq!(report.delivered, 1);
    assert_eq!(report.rescheduled, 0);
    assert_eq!(repository.terminal(), vec![(EVENT, "delivered".to_owned())]);
    let seen = sender.seen();
    let (signature, body) = seen.first().ok_or("nothing was sent")?;
    assert!(signature.starts_with("t="));
    assert!(signature.contains(",v1="));
    let envelope: Value = serde_json::from_slice(body)?;
    assert_eq!(envelope["type"], "payment_intent.paid");
    assert_eq!(envelope["id"], EVENT.to_string());
    assert_eq!(envelope["data"]["object"], "payment_intent");
    Ok(())
}

#[tokio::test]
async fn a_refused_delivery_is_retried_later() -> TestResult {
    let repository = FakeRepository::with(vec![event()], vec![endpoint()?]);
    let sender = ScriptedSender::new(DeliveryResult::Refused {
        status: 500,
        duration_ms: 30,
        detail: "internal error".to_owned(),
    });
    let service = service(Arc::clone(&repository), sender)?;

    let report = service.deliver_due(10).await?;

    assert_eq!(report.rescheduled, 1);
    assert_eq!(report.delivered, 0);
    let attempts = repository.attempts();
    assert_eq!(attempts.first().map(|attempt| attempt.1), Some(Some(500)));
    Ok(())
}

#[tokio::test]
async fn an_exhausted_event_is_dead_lettered_instead_of_retried_forever() -> TestResult {
    let mut exhausted = event();
    exhausted.attempts = 4;
    let repository = FakeRepository::with(vec![exhausted], vec![endpoint()?]);
    let sender = ScriptedSender::new(DeliveryResult::Unreachable {
        detail: "connection refused".to_owned(),
        duration_ms: 5,
    });
    let service = service(Arc::clone(&repository), sender)?;

    let report = service.deliver_due(10).await?;

    assert_eq!(report.dead_lettered, 1);
    assert_eq!(
        repository.terminal(),
        vec![(EVENT, "dead_letter:delivery_attempts_exhausted".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn an_event_nobody_listens_to_is_never_called_delivered() -> TestResult {
    let repository = FakeRepository::with(vec![event()], Vec::new());
    let sender = ScriptedSender::new(DeliveryResult::Accepted {
        status: 200,
        duration_ms: 1,
    });
    let service = service(Arc::clone(&repository), sender)?;

    let report = service.deliver_due(10).await?;

    assert_eq!(report.delivered, 0);
    assert_eq!(report.endpoints_missing, 1);
    assert_eq!(
        repository.terminal(),
        vec![(EVENT, "dead_letter:no_active_endpoint".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn a_wrong_master_key_never_produces_a_signature_the_merchant_cannot_verify() -> TestResult {
    let mut stale = endpoint()?;
    stale.secret_fingerprint = [0_u8; 32];
    let repository = FakeRepository::with(vec![event()], vec![stale]);
    let sender = ScriptedSender::new(DeliveryResult::Accepted {
        status: 200,
        duration_ms: 1,
    });
    let service = service(Arc::clone(&repository), Arc::clone(&sender))?;

    let report = service.deliver_due(10).await?;

    assert_eq!(report.delivered, 0);
    assert_eq!(report.rescheduled, 1);
    assert!(sender.seen().is_empty());
    let attempts = repository.attempts();
    assert_eq!(
        attempts.first().and_then(|attempt| attempt.2.clone()),
        Some("signing_key_mismatch".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn a_weak_master_key_is_refused_at_construction() -> TestResult {
    let repository = FakeRepository::with(Vec::new(), Vec::new());
    let sender = ScriptedSender::new(DeliveryResult::Accepted {
        status: 200,
        duration_ms: 1,
    });

    let refused = OutboxService::new(repository, sender, FixedClock, vec![1_u8; 8], "pod", 5);

    assert!(refused.is_err());
    Ok(())
}
