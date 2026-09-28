use std::{
    error::Error,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_application::{
    DeliveryResult, OperationsError, OperationsService, OperatorCredential, OperatorScope,
    OutboxRepository, OutboxService, RedeliveryError, SystemClock, WebhookEndpoint,
    WebhookRedelivery, WebhookSender,
};
use gateway_domain::SigningSecret;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

const MERCHANT: Uuid = Uuid::from_u128(19_001);
const OTHER_MERCHANT: Uuid = Uuid::from_u128(19_002);
const ENDPOINT: Uuid = Uuid::from_u128(19_011);
const SECOND_ENDPOINT: Uuid = Uuid::from_u128(19_012);
const FOREIGN_ENDPOINT: Uuid = Uuid::from_u128(19_013);
const DISABLED_ENDPOINT: Uuid = Uuid::from_u128(19_014);
const DEAD_EVENT: Uuid = Uuid::from_u128(19_021);
const QUEUED_EVENT: Uuid = Uuid::from_u128(19_022);
const OPERATOR_EVENT: Uuid = Uuid::from_u128(19_023);
const ADMIN_KEY: Uuid = Uuid::from_u128(19_031);
const READ_KEY: Uuid = Uuid::from_u128(19_032);
const MASTER_KEY: [u8; 32] = [7_u8; 32];

fn admin() -> OperatorCredential {
    OperatorCredential {
        key_id: ADMIN_KEY,
        label: "on-call".to_owned(),
        scopes: vec![OperatorScope::Read, OperatorScope::Admin],
    }
}

fn reader() -> OperatorCredential {
    OperatorCredential {
        key_id: READ_KEY,
        label: "dashboard".to_owned(),
        scopes: vec![OperatorScope::Read],
    }
}

fn payload() -> Value {
    json!({"payment_intent_id": "00000000-0000-0000-0000-000000000009", "note": "paid"})
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query("TRUNCATE merchants, operator_api_keys CASCADE")
        .execute(pool)
        .await?;
    for (id, name) in [
        (MERCHANT, "redeliver-one"),
        (OTHER_MERCHANT, "redeliver-two"),
    ] {
        sqlx::query(
            "INSERT INTO merchants(id,external_id,display_name,status,collector_policy) VALUES($1,$2,$2,'active','shared')",
        )
        .bind(id)
        .bind(name)
        .execute(pool)
        .await?;
    }
    for (id, merchant, status) in [
        (ENDPOINT, MERCHANT, "active"),
        (SECOND_ENDPOINT, MERCHANT, "active"),
        (FOREIGN_ENDPOINT, OTHER_MERCHANT, "active"),
        (DISABLED_ENDPOINT, MERCHANT, "disabled"),
    ] {
        let secret = SigningSecret::derive(&MASTER_KEY, 1, merchant, id)?;
        sqlx::query(
            r"INSERT INTO webhook_endpoints(id,merchant_id,url,secret_version,secret_fingerprint,status,created_at,disabled_at)
              VALUES($1,$2,$3,1,$4,$5,now(),CASE WHEN $5='disabled' THEN now() END)",
        )
        .bind(id)
        .bind(merchant)
        .bind(format!("https://{id}.example/hook"))
        .bind(secret.fingerprint().as_slice())
        .bind(status)
        .execute(pool)
        .await?;
    }
    for (id, scopes) in [(ADMIN_KEY, vec!["read", "admin"]), (READ_KEY, vec!["read"])] {
        sqlx::query(
            "INSERT INTO operator_api_keys(id,key_prefix,secret_hash,label,scopes) VALUES($1,'cg_test',$2,'redelivery',$3)",
        )
        .bind(id)
        .bind(id.as_bytes().repeat(2))
        .bind(scopes)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        r"INSERT INTO domain_events(id,merchant_id,channel,event_type,aggregate_type,aggregate_id,payload,
              available_at,attempts,last_error,dead_lettered_at,created_at)
          VALUES($1,$2,'webhook','payment_intent.paid','payment_intent',$3,$4,
                 now()-interval '1 day',3,'delivery_attempts_exhausted',now()-interval '1 hour',
                 now()-interval '1 day')",
    )
    .bind(DEAD_EVENT)
    .bind(MERCHANT)
    .bind(Uuid::from_u128(9))
    .bind(payload())
    .execute(pool)
    .await?;
    for attempt in 1..=3 {
        sqlx::query(
            "INSERT INTO webhook_deliveries(id,event_id,endpoint_id,attempt,response_status,error,delivered_at) VALUES($1,$2,$3,$4,503,'busy',now()-interval '2 hours')",
        )
        .bind(Uuid::now_v7())
        .bind(DEAD_EVENT)
        .bind(ENDPOINT)
        .bind(attempt)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        r"INSERT INTO domain_events(id,merchant_id,channel,event_type,aggregate_type,aggregate_id,payload,available_at,created_at)
          VALUES($1,$2,'webhook','payment_intent.paid','payment_intent',$3,'{}',now(),now()),
                ($4,NULL,'operator','UNMATCHED_INBOUND','chain_transfer',$3,'{}',now(),now())",
    )
    .bind(QUEUED_EVENT)
    .bind(MERCHANT)
    .bind(Uuid::from_u128(10))
    .bind(OPERATOR_EVENT)
    .execute(pool)
    .await?;
    Ok(())
}

fn service(pool: &PgPool) -> OperationsService<PostgresRepository, SystemClock> {
    OperationsService::new(Arc::new(PostgresRepository::new(pool.clone())), SystemClock)
}

fn redelivery(endpoint: Option<Uuid>) -> WebhookRedelivery {
    WebhookRedelivery {
        event_id: DEAD_EVENT,
        endpoint_id: endpoint,
        reason: "merchant restored their endpoint after an outage".to_owned(),
    }
}

/// Remembers the body of every delivery and accepts it.
#[derive(Debug, Default)]
struct RecordingSender {
    bodies: Mutex<Vec<(Uuid, Vec<u8>)>>,
}

#[async_trait]
impl WebhookSender for RecordingSender {
    async fn deliver(
        &self,
        endpoint: &WebhookEndpoint,
        _event_id: Uuid,
        body: &[u8],
        _signature: &str,
    ) -> DeliveryResult {
        if let Ok(mut bodies) = self.bodies.lock() {
            bodies.push((endpoint.id, body.to_vec()));
        }
        DeliveryResult::Accepted {
            status: 200,
            duration_ms: 1,
        }
    }
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn a_dead_lettered_event_is_redelivered_as_the_same_event() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let service = service(&pool);
    let request = redelivery(Some(SECOND_ENDPOINT));

    assert!(matches!(
        service
            .redeliver_webhook(&reader(), "redeliver-dead-000001", &request)
            .await,
        Err(OperationsError::MissingScope(OperatorScope::Admin))
    ));

    let result = service
        .redeliver_webhook(&admin(), "redeliver-dead-000001", &request)
        .await?;
    assert!(!result.replayed);
    assert_eq!(result.event_id, DEAD_EVENT);
    assert_eq!(result.previous_state, "dead_lettered");
    assert_eq!(result.previous_attempts, 3);

    // Pending again, as itself: same id, same payload, a fresh budget, and
    // aimed at the one endpoint that was named.
    let row: (Option<OffsetDateTime>, Option<OffsetDateTime>, Value, i32, i32, Option<Uuid>) =
        sqlx::query_as(
            "SELECT delivered_at, dead_lettered_at, payload, attempts, attempt_floor, target_endpoint_id FROM domain_events WHERE id=$1",
        )
        .bind(DEAD_EVENT)
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.0, None);
    assert_eq!(row.1, None);
    assert_eq!(row.2, payload());
    assert_eq!((row.3, row.4), (3, 3));
    assert_eq!(row.5, Some(SECOND_ENDPOINT));
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM domain_events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(events, 3, "a redelivery never creates a second event");
    let audit: (Option<Uuid>, String, Value) = sqlx::query_as(
        "SELECT actor_id, reason, payload FROM audit_events WHERE action='webhook_event.redeliver' AND resource_id=$1",
    )
    .bind(DEAD_EVENT)
    .fetch_one(&pool)
    .await?;
    assert_eq!(audit.0, Some(ADMIN_KEY));
    assert_eq!(audit.1, request.reason);
    assert_eq!(audit.2["previous_state"], "dead_lettered");

    // The same request again is the same answer; a different one under the
    // same key is refused.
    let replay = service
        .redeliver_webhook(&admin(), "redeliver-dead-000001", &request)
        .await?;
    assert!(replay.replayed);
    assert_eq!(replay.id, result.id);
    assert!(matches!(
        service
            .redeliver_webhook(&admin(), "redeliver-dead-000001", &redelivery(None))
            .await,
        Err(OperationsError::Redelivery(
            RedeliveryError::IdempotencyConflict
        ))
    ));
    // Still queued: a second redelivery under a new key is refused rather
    // than stacked.
    assert!(matches!(
        service
            .redeliver_webhook(&admin(), "redeliver-dead-000002", &request)
            .await,
        Err(OperationsError::Redelivery(RedeliveryError::StillQueued))
    ));

    // The worker sends the original envelope to the named endpoint only, and
    // numbers the attempt after the ones already on record.
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let sender = Arc::new(RecordingSender::default());
    let outbox = OutboxService::new(
        Arc::clone(&repository),
        Arc::clone(&sender),
        SystemClock,
        MASTER_KEY.to_vec(),
        "redelivery-test",
        2,
    )?;
    let report = outbox.deliver_due(10).await?;
    assert_eq!(report.delivered, 2, "{report:?}");
    let bodies = sender.bodies.lock().map_err(|_| "poisoned")?.clone();
    let (endpoint, body) = bodies
        .iter()
        .find(|(_, body)| {
            serde_json::from_slice::<Value>(body)
                .is_ok_and(|envelope| envelope["id"] == DEAD_EVENT.to_string())
        })
        .ok_or("the redelivered event was not sent")?;
    assert_eq!(*endpoint, SECOND_ENDPOINT);
    let envelope: Value = serde_json::from_slice(body)?;
    assert_eq!(envelope["data"]["attributes"], payload());
    let attempts: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT endpoint_id, attempt FROM webhook_deliveries WHERE event_id=$1 ORDER BY attempt",
    )
    .bind(DEAD_EVENT)
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        attempts,
        vec![
            (ENDPOINT, 1),
            (ENDPOINT, 2),
            (ENDPOINT, 3),
            (SECOND_ENDPOINT, 4)
        ]
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_redelivery_is_refused_for_anything_but_a_finished_webhook_of_its_own_merchant()
-> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let service = service(&pool);

    for (label, request, expected) in [
        (
            "another merchant's endpoint",
            redelivery(Some(FOREIGN_ENDPOINT)),
            "endpoint",
        ),
        (
            "a disabled endpoint",
            redelivery(Some(DISABLED_ENDPOINT)),
            "endpoint",
        ),
        (
            "an unknown event",
            WebhookRedelivery {
                event_id: Uuid::from_u128(1),
                ..redelivery(None)
            },
            "missing",
        ),
        (
            "an event still queued",
            WebhookRedelivery {
                event_id: QUEUED_EVENT,
                ..redelivery(None)
            },
            "queued",
        ),
        (
            "an operator event",
            WebhookRedelivery {
                event_id: OPERATOR_EVENT,
                ..redelivery(None)
            },
            "operator",
        ),
    ] {
        let result = service
            .redeliver_webhook(&admin(), "redeliver-refused-0001", &request)
            .await;
        let refused = matches!(
            (&result, expected),
            (
                Err(OperationsError::Redelivery(
                    RedeliveryError::EndpointNotFound
                )),
                "endpoint"
            ) | (
                Err(OperationsError::Redelivery(RedeliveryError::EventNotFound)),
                "missing"
            ) | (
                Err(OperationsError::Redelivery(RedeliveryError::StillQueued)),
                "queued"
            ) | (
                Err(OperationsError::Redelivery(RedeliveryError::NotAWebhook)),
                "operator"
            )
        );
        assert!(refused, "{label}: {result:?}");
    }
    let written: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM webhook_redeliveries) + (SELECT count(*) FROM audit_events)",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(written, 0, "a refused redelivery wrote a record");
    let dead: bool = sqlx::query_scalar(
        "SELECT dead_lettered_at IS NOT NULL AND target_endpoint_id IS NULL FROM domain_events WHERE id=$1",
    )
    .bind(DEAD_EVENT)
    .fetch_one(&pool)
    .await?;
    assert!(dead, "a refused redelivery moved the event");

    // The database refuses the same mistake from a path that skipped the check.
    let forged = sqlx::query("UPDATE domain_events SET target_endpoint_id=$2 WHERE id=$1")
        .bind(DEAD_EVENT)
        .bind(FOREIGN_ENDPOINT)
        .execute(&pool)
        .await;
    assert!(
        matches!(&forged, Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503")),
        "{forged:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_deep_backlog_of_one_merchant_does_not_take_the_whole_batch() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    sqlx::query("DELETE FROM domain_events WHERE id=$1")
        .bind(QUEUED_EVENT)
        .execute(&pool)
        .await?;
    // Thirty old events of one merchant, then one newer event of another.
    sqlx::query(
        r"INSERT INTO domain_events(id,merchant_id,channel,event_type,aggregate_type,aggregate_id,payload,available_at,created_at)
          SELECT gen_random_uuid(),$1,'webhook','payment_intent.paid','payment_intent',gen_random_uuid(),'{}',
                 now()-interval '1 hour'+n*interval '1 second',now()
            FROM generate_series(1,30) AS n",
    )
    .bind(MERCHANT)
    .execute(&pool)
    .await?;
    sqlx::query(
        r"INSERT INTO domain_events(id,merchant_id,channel,event_type,aggregate_type,aggregate_id,payload,available_at,created_at)
          VALUES(gen_random_uuid(),$1,'webhook','payment_intent.paid','payment_intent',gen_random_uuid(),'{}',now()-interval '1 minute',now())",
    )
    .bind(OTHER_MERCHANT)
    .execute(&pool)
    .await?;
    let repository = PostgresRepository::new(pool.clone());

    let claimed = repository
        .claim_due_events("fair-claim", 10, 5, 60, OffsetDateTime::now_utc())
        .await?;

    let mine = claimed
        .iter()
        .filter(|event| event.merchant_id == Some(MERCHANT))
        .count();
    let theirs = claimed
        .iter()
        .filter(|event| event.merchant_id == Some(OTHER_MERCHANT))
        .count();
    assert_eq!((mine, theirs), (5, 1));
    // Claimed rows are not claimed again while the visibility window lasts.
    let again = repository
        .claim_due_events("fair-claim-2", 10, 5, 60, OffsetDateTime::now_utc())
        .await?;
    assert_eq!(again.len(), 5);
    assert!(
        again
            .iter()
            .all(|event| claimed.iter().all(|first| first.id != event.id))
    );
    Ok(())
}
