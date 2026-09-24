mod auth;
mod error;
mod handlers;
mod metrics;

use std::{sync::Arc, time::Duration};

use axum::{Router, http::StatusCode, middleware, routing::get};
use gateway_application::{
    OperationsService, OperatorReadService, PaymentIntentService, QuoteService, SelfCheckConfig,
    SelfCheckReport, SelfCheckService, SystemClock,
};
use gateway_scheduler::RunMetrics;
use gateway_storage::{PgPool, PostgresRepository};
use tokio::sync::Mutex;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{catch_panic::CatchPanicLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::{
    auth::{authenticate, authenticate_operator},
    handlers::health,
};

#[derive(Debug, Clone)]
pub struct AppState {
    pub repository: Arc<PostgresRepository>,
    pub payment_intents: Arc<PaymentIntentService<PostgresRepository, SystemClock>>,
    pub quotes: Arc<QuoteService<PostgresRepository, SystemClock>>,
    pub operations: Arc<OperationsService<PostgresRepository, SystemClock>>,
    pub operator_reads: Arc<OperatorReadService<PostgresRepository>>,
    pub expiry_metrics: Option<Arc<RunMetrics>>,
    pub pool: PgPool,
    pub self_check: Option<Arc<SelfCheckService<PostgresRepository, SystemClock>>>,
    pub self_check_cache: Arc<Mutex<Option<SelfCheckReport>>>,
}

impl AppState {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        let repository = Arc::new(PostgresRepository::new(pool.clone()));
        let payment_intents = Arc::new(PaymentIntentService::new(
            Arc::clone(&repository),
            SystemClock,
        ));
        let quotes = Arc::new(QuoteService::new(Arc::clone(&repository), SystemClock));
        let operations = Arc::new(OperationsService::new(Arc::clone(&repository), SystemClock));
        let operator_reads = Arc::new(OperatorReadService::new(Arc::clone(&repository)));
        Self {
            repository,
            payment_intents,
            quotes,
            operations,
            operator_reads,
            expiry_metrics: None,
            pool,
            self_check: None,
            self_check_cache: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn with_expiry_metrics(mut self, metrics: Arc<RunMetrics>) -> Self {
        self.expiry_metrics = Some(metrics);
        self
    }

    #[must_use]
    pub fn with_self_check(mut self, config: SelfCheckConfig) -> Self {
        self.self_check = Some(Arc::new(SelfCheckService::new(
            Arc::clone(&self.repository),
            SystemClock,
            config,
        )));
        self
    }
}

pub fn router(state: AppState) -> Router {
    let protected = handlers::payment_intent_routes()
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    let operator = handlers::operator_routes().route_layer(middleware::from_fn_with_state(
        state.clone(),
        authenticate_operator,
    ));

    Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        .merge(protected)
        .merge(operator)
        .with_state(state)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(15),
        ))
        .layer(ConcurrencyLimitLayer::new(128))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, env, error::Error};

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use gateway_application::{ExpectedAsset, SelfCheckConfig};
    use gateway_domain::{AddressKey, ChainEnvironment};
    use gateway_storage::{PgPool, PgPoolOptions, migrate};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{AppState, router};

    const MERCHANT_ONE: Uuid = Uuid::from_u128(1);
    const MERCHANT_TWO: Uuid = Uuid::from_u128(2);
    const KEY_ONE: Uuid = Uuid::from_u128(11);
    const KEY_TWO: Uuid = Uuid::from_u128(12);
    const ASSET_ID: Uuid = Uuid::from_u128(21);
    const COLLECTOR_ID: Uuid = Uuid::from_u128(22);
    const PRICE_SNAPSHOT_ID: Uuid = Uuid::from_u128(23);
    const QUOTE_POLICY_ID: Uuid = Uuid::from_u128(24);
    const RAIL_HEALTH_SNAPSHOT_ID: Uuid = Uuid::from_u128(25);
    const SECRET_ONE: &str = "cg_test_merchant_one_0000000000000001";
    const SECRET_TWO: &str = "cg_test_merchant_two_0000000000000002";
    const IDEMPOTENCY_KEY: &str = "checkout_01JABCDEFGHJKMNPQRSTVWXYZ";

    #[tokio::test]
    #[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
    async fn readiness_returns_the_failed_self_check_report() -> Result<(), Box<dyn Error>> {
        let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await?;
        let mut scenario_lock = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock(20260923)")
            .execute(&mut *scenario_lock)
            .await?;
        reset_database(&pool).await?;
        sqlx::query("TRUNCATE chain_finality_policies CASCADE")
            .execute(&pool)
            .await?;
        let config = SelfCheckConfig {
            collectors: vec![AddressKey::new(vec![8_u8; 21])?],
            assets: vec![ExpectedAsset {
                chain: "tron".to_owned(),
                network: "nile".to_owned(),
                contract: AddressKey::new(vec![7_u8; 20])?,
            }],
            environment: ChainEnvironment::Testnet,
            max_clock_skew_seconds: 5,
        };
        let response = router(AppState::new(pool).with_self_check(config))
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = json_body(response).await?;
        assert_eq!(body["ready"], false);
        assert!(
            body["evaluated_at"]
                .as_str()
                .is_some_and(|stamp| stamp.ends_with('Z'))
        );
        assert!(body["checks"].as_array().is_some_and(|checks| {
            checks
                .iter()
                .any(|check| check["name"] == "finality_policy_present" && check["passed"] == false)
        }));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
    // One linear scenario proves the cross-request idempotency, isolation, and
    // audit invariants against the same database state.
    #[allow(clippy::too_many_lines)]
    async fn payment_intent_api_contract_and_isolation() -> Result<(), Box<dyn Error>> {
        let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await?;
        let mut scenario_lock = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock(20260923)")
            .execute(&mut *scenario_lock)
            .await?;
        reset_database(&pool).await?;
        let app = router(AppState::new(pool.clone()));

        let unauthorized = app
            .clone()
            .oneshot(request(
                "GET",
                "/v1/payment-intents/00000000-0000-0000-0000-000000000001",
                None,
                None,
                None,
            )?)
            .await?;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let create_body = json!({
            "amount_minor": "12345",
            "currency": "USD",
            "reference": "order-123",
            "description": "API integration test",
            "metadata": {"suite": "api"}
        });
        let created = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/payment-intents",
                Some(SECRET_ONE),
                Some(IDEMPOTENCY_KEY),
                Some(create_body.clone()),
            )?)
            .await?;
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()["idempotent-replayed"], "false");
        let created = json_body(created).await?;
        assert_eq!(created["amount"]["minor_units"], "12345");
        assert_eq!(
            object_keys(&created),
            BTreeSet::from([
                "amount",
                "created_at",
                "description",
                "id",
                "merchant_id",
                "metadata",
                "reference",
                "status",
                "updated_at",
            ])
        );
        assert_eq!(
            object_keys(&created["amount"]),
            BTreeSet::from(["currency", "minor_units"])
        );
        let intent_id = created["id"].as_str().ok_or("missing intent id")?;

        let replay = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/payment-intents",
                Some(SECRET_ONE),
                Some(IDEMPOTENCY_KEY),
                Some(create_body.clone()),
            )?)
            .await?;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(replay.headers()["idempotent-replayed"], "true");
        assert_eq!(json_body(replay).await?["id"], intent_id);

        let quote_body = json!({"asset_id": ASSET_ID});
        let quote = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JABCDEFGHJKMNPQRSTVWXYZ"),
                Some(quote_body.clone()),
            )?)
            .await?;
        assert_eq!(quote.status(), StatusCode::CREATED);
        assert_eq!(quote.headers()["idempotent-replayed"], "false");
        let quote = json_body(quote).await?;
        assert_eq!(quote["amount_raw"], "1235");
        assert_eq!(quote["collector_address"], "TContractCollector");
        assert_eq!(quote["price_snapshot_id"], PRICE_SNAPSHOT_ID.to_string());
        assert_eq!(
            object_keys(&quote),
            BTreeSet::from([
                "amount_raw",
                "asset_id",
                "attempt_id",
                "collector_address",
                "collector_address_id",
                "created_at",
                "expires_at",
                "fiat_amount",
                "id",
                "late_payment_until",
                "payment_intent_id",
                "policy_version",
                "price_observed_at",
                "price_snapshot_id",
                "price_sources",
                "quote_policy_id",
                "rail_health_observed_at",
                "rail_health_snapshot_id",
                "rate_denominator",
                "rate_numerator",
            ])
        );

        let quote_replay = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JABCDEFGHJKMNPQRSTVWXYZ"),
                Some(quote_body.clone()),
            )?)
            .await?;
        assert_eq!(quote_replay.status(), StatusCode::OK);
        assert_eq!(quote_replay.headers()["idempotent-replayed"], "true");
        assert_eq!(json_body(quote_replay).await?["id"], quote["id"]);

        let quote_key_conflict = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JABCDEFGHJKMNPQRSTVWXYZ"),
                Some(json!({"asset_id": Uuid::from_u128(9_999)})),
            )?)
            .await?;
        assert_error(
            quote_key_conflict,
            StatusCode::CONFLICT,
            "idempotency_conflict",
        )
        .await?;

        let quote_again = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JDIFFERENT000000000001"),
                Some(quote_body.clone()),
            )?)
            .await?;
        assert_error(
            quote_again,
            StatusCode::CONFLICT,
            "payment_intent_not_quotable",
        )
        .await?;

        let foreign_quote = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_TWO),
                Some("quote_01JFOREIGN00000000000001"),
                Some(quote_body.clone()),
            )?)
            .await?;
        assert_error(
            foreign_quote,
            StatusCode::NOT_FOUND,
            "payment_intent_not_found",
        )
        .await?;

        let injected_quote_fact = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JINJECTED0000000000001"),
                Some(json!({
                    "asset_id": ASSET_ID,
                    "amount_raw": "1"
                })),
            )?)
            .await?;
        assert_error(
            injected_quote_fact,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        )
        .await?;

        let conflict = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/payment-intents",
                Some(SECRET_ONE),
                Some(IDEMPOTENCY_KEY),
                Some(json!({
                    "amount_minor": "54321",
                    "currency": "USD",
                    "reference": "order-456"
                })),
            )?)
            .await?;
        assert_error(conflict, StatusCode::CONFLICT, "idempotency_conflict").await?;

        let duplicate_reference = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/payment-intents",
                Some(SECRET_ONE),
                Some("checkout_01JDIFFERENTKEY000000001"),
                Some(create_body),
            )?)
            .await?;
        assert_error(
            duplicate_reference,
            StatusCode::CONFLICT,
            "payment_intent_reference_conflict",
        )
        .await?;

        let foreign_read = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/v1/payment-intents/{intent_id}"),
                Some(SECRET_TWO),
                None,
                None,
            )?)
            .await?;
        assert_error(
            foreign_read,
            StatusCode::NOT_FOUND,
            "payment_intent_not_found",
        )
        .await?;

        let numeric_money = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/payment-intents",
                Some(SECRET_ONE),
                Some("checkout_01JNUMERICMONEY0000000001"),
                Some(json!({
                    "amount_minor": 12345,
                    "currency": "USD",
                    "reference": "numeric-money"
                })),
            )?)
            .await?;
        assert_error(numeric_money, StatusCode::BAD_REQUEST, "invalid_request").await?;

        let concurrent_body = json!({
            "amount_minor": "777",
            "currency": "USD",
            "reference": "concurrent-order"
        });
        let first = app.clone().oneshot(request(
            "POST",
            "/v1/payment-intents",
            Some(SECRET_ONE),
            Some("checkout_01JCONCURRENT00000000001"),
            Some(concurrent_body.clone()),
        )?);
        let second = app.clone().oneshot(request(
            "POST",
            "/v1/payment-intents",
            Some(SECRET_ONE),
            Some("checkout_01JCONCURRENT00000000001"),
            Some(concurrent_body),
        )?);
        let (first, second) = tokio::join!(first, second);
        let first = first?;
        let second = second?;
        assert!(
            (first.status() == StatusCode::CREATED && second.status() == StatusCode::OK)
                || (first.status() == StatusCode::OK && second.status() == StatusCode::CREATED)
        );
        let first = json_body(first).await?;
        let second = json_body(second).await?;
        assert_eq!(first["id"], second["id"]);
        let concurrent_intent_id = first["id"].as_str().ok_or("missing concurrent intent id")?;

        sqlx::query(
            "INSERT INTO rail_health_snapshots (id, asset_id, health, observed_at) \
             VALUES ($1, $2, 'unavailable', now())",
        )
        .bind(Uuid::now_v7())
        .bind(ASSET_ID)
        .execute(&pool)
        .await?;
        let unavailable_quote = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{concurrent_intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JUNAVAILABLE00000000001"),
                Some(json!({"asset_id": ASSET_ID})),
            )?)
            .await?;
        assert_error(
            unavailable_quote,
            StatusCode::SERVICE_UNAVAILABLE,
            "quote_unavailable",
        )
        .await?;

        let replay_during_outage = app
            .oneshot(request(
                "POST",
                &format!("/v1/payment-intents/{intent_id}/quotes"),
                Some(SECRET_ONE),
                Some("quote_01JABCDEFGHJKMNPQRSTVWXYZ"),
                Some(quote_body),
            )?)
            .await?;
        assert_eq!(replay_during_outage.status(), StatusCode::OK);
        assert_eq!(
            replay_during_outage.headers()["idempotent-replayed"],
            "true"
        );

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action = 'payment_intent.created'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(audit_count, 2);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
    #[allow(clippy::too_many_lines)]
    async fn operator_reads_are_scoped_paginated_and_scrapeable() -> Result<(), Box<dyn Error>> {
        const READ_SECRET: &str = "cg_operator_read_test_secret_000001";
        const INGEST_SECRET: &str = "cg_operator_ingest_test_secret_0001";
        let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await?;
        let mut scenario_lock = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock(20260923)")
            .execute(&mut *scenario_lock)
            .await?;
        reset_database(&pool).await?;
        for (id, secret, scopes) in [
            (Uuid::from_u128(801), READ_SECRET, vec!["read"]),
            (Uuid::from_u128(802), INGEST_SECRET, vec!["ingest"]),
        ] {
            let hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
            sqlx::query("INSERT INTO operator_api_keys(id,key_prefix,secret_hash,label,scopes) VALUES($1,'cg_test',$2,'operator read test',$3)").bind(id).bind(hash.as_slice()).bind(scopes).execute(&pool).await?;
        }
        sqlx::query("INSERT INTO component_health(component,state,detail,since,updated_at) VALUES('verifier','degraded','test degradation',now(),now())").execute(&pool).await?;
        sqlx::query("INSERT INTO rail_stops(id,asset_id,reason_code,opened_by,opened_at) VALUES($1,$2,'test_stop','test',now())").bind(Uuid::now_v7()).bind(ASSET_ID).execute(&pool).await?;
        for kind in ["incremental", "daily"] {
            sqlx::query("INSERT INTO reconciliation_runs(id,kind,window_start,window_end,status,started_at,finished_at) VALUES($1,$2,now()-interval '1 hour',now(),'drift',now(),now())").bind(Uuid::now_v7()).bind(kind).execute(&pool).await?;
        }
        let run_id: Uuid =
            sqlx::query_scalar("SELECT id FROM reconciliation_runs ORDER BY id DESC LIMIT 1")
                .fetch_one(&pool)
                .await?;
        let discrepancy_ids = [Uuid::now_v7(), Uuid::now_v7()];
        for (id, kind, money_affected) in [
            (discrepancy_ids[0], "observer_behind", false),
            (discrepancy_ids[1], "allocation_exceeds_transfer", true),
        ] {
            sqlx::query(
                "INSERT INTO reconciliation_discrepancies(id,run_id,kind,money_affected,detail,created_at) VALUES($1,$2,$3,$4,'{}',now())",
            )
            .bind(id)
            .bind(run_id)
            .bind(kind)
            .bind(money_affected)
            .execute(&pool)
            .await?;
        }
        let intent_id = Uuid::now_v7();
        let quote_id = Uuid::now_v7();
        let attempt_id = Uuid::now_v7();
        sqlx::query("INSERT INTO payment_intents(id,merchant_id,amount_minor,currency,status,reference,created_at,updated_at) VALUES($1,$2,9007199254740993,'USD','risk_hold','operator-evidence',now(),now())").bind(intent_id).bind(MERCHANT_ONE).execute(&pool).await?;
        sqlx::query("INSERT INTO payment_quotes(id,merchant_id,payment_intent_id,asset_id,collector_address_id,fiat_currency,fiat_amount_minor,base_amount_raw,amount_raw,rate_numerator,rate_denominator,price_sources,price_observed_at,policy_version,rail_health_observed_at,created_at,expires_at,late_payment_until,price_snapshot_id,quote_policy_id,rail_health_snapshot_id) VALUES($1,$2,$3,$4,$5,'USD',9007199254740993,9007199254740993,9007199254740994,1,10,'[{\"provider_group\":\"test\"}]',now(),'policy-v1',now(),now(),now()+interval '1 hour',now()+interval '2 hours',$6,$7,$8)").bind(quote_id).bind(MERCHANT_ONE).bind(intent_id).bind(ASSET_ID).bind(COLLECTOR_ID).bind(PRICE_SNAPSHOT_ID).bind(QUOTE_POLICY_ID).bind(RAIL_HEALTH_SNAPSHOT_ID).execute(&pool).await?;
        sqlx::query("INSERT INTO payment_attempts(id,merchant_id,payment_intent_id,quote_id,collector_address_id,expected_amount_raw,status,quote_expires_at,late_payment_until,created_at,updated_at) VALUES($1,$2,$3,$4,$5,9007199254740994,'awaiting_payment',now()+interval '1 hour',now()+interval '2 hours',now(),now())").bind(attempt_id).bind(MERCHANT_ONE).bind(intent_id).bind(quote_id).bind(COLLECTOR_ID).execute(&pool).await?;
        let transfer_id = Uuid::now_v7();
        sqlx::query("INSERT INTO chain_transfers(id,asset_id,collector_address_id,chain,network,chain_environment,tx_hash,event_index,block_number,block_hash,block_time,token_key,from_address_key,from_address_text,to_address_key,to_address_text,amount_raw,decimals,canonicalization_policy,verifier_version,canonicalized_at) VALUES($1,$2,$3,'tron','nile','testnet','operator-tx',0,100,'block-100',now(),$4,$5,'TFrom',$6,'TContractCollector',9007199254740994,6,'test-policy','test-verifier',now())").bind(transfer_id).bind(ASSET_ID).bind(COLLECTOR_ID).bind([7_u8;20].as_slice()).bind([9_u8;21].as_slice()).bind([8_u8;21].as_slice()).execute(&pool).await?;
        sqlx::query("INSERT INTO chain_transfer_state_current(transfer_id,state,state_version,updated_at) VALUES($1,'finalized',1,now())").bind(transfer_id).execute(&pool).await?;
        sqlx::query("INSERT INTO chain_transfer_processing(transfer_id,processing_state,updated_at) VALUES($1,'unmatched',now())").bind(transfer_id).execute(&pool).await?;
        sqlx::query("INSERT INTO chain_transfer_intent_claims(transfer_id,payment_intent_id,attempt_id,merchant_id,match_strategy,claimed_at) VALUES($1,$2,$3,$4,'manual',now())").bind(transfer_id).bind(intent_id).bind(attempt_id).bind(MERCHANT_ONE).execute(&pool).await?;
        sqlx::query("INSERT INTO payment_settlement_decisions(id,payment_intent_id,attempt_id,transfer_id,merchant_id,fiat_amount_minor,required_policy,distinct_groups,had_own_node,finality_state,risk_decision,attestation_ids,match_strategy,allocated_raw,remainder_raw,outcome,decided_by,decided_at) VALUES($1,$2,$3,$4,$5,9007199254740993,'test-policy',2,false,'finalized','review','{}','manual',0,9007199254740994,'held','test',now())").bind(Uuid::now_v7()).bind(intent_id).bind(attempt_id).bind(transfer_id).bind(MERCHANT_ONE).execute(&pool).await?;
        let event_id = Uuid::now_v7();
        let endpoint_id = Uuid::now_v7();
        sqlx::query("INSERT INTO webhook_endpoints(id,merchant_id,url,secret_fingerprint,description,status,created_at) VALUES($1,$2,'https://example.invalid/hook',$3,'test','active',now())").bind(endpoint_id).bind(MERCHANT_ONE).bind([1_u8;32].as_slice()).execute(&pool).await?;
        sqlx::query("INSERT INTO domain_events(id,merchant_id,event_type,aggregate_type,aggregate_id,payload,available_at,attempts,last_error,dead_lettered_at,created_at) VALUES($1,$2,'payment.held','payment_intent',$3,'{}',now(),2,'test failure',now(),now())").bind(event_id).bind(MERCHANT_ONE).bind(intent_id).execute(&pool).await?;
        for attempt in 1..=2 {
            sqlx::query("INSERT INTO webhook_deliveries(id,event_id,endpoint_id,attempt,error,delivered_at) VALUES($1,$2,$3,$4,'test failure',now())").bind(Uuid::now_v7()).bind(event_id).bind(endpoint_id).bind(attempt).execute(&pool).await?;
        }
        let app = router(AppState::new(pool));
        for path in [
            "/v1/operator/overview",
            "/v1/operator/conflicts?limit=1",
            "/v1/operator/unmatched-transfers?limit=1",
            "/v1/operator/held-payments?limit=1",
            "/v1/operator/dead-letters?limit=1",
            "/v1/operator/reconciliation/runs?limit=1",
            "/v1/operator/reconciliation/discrepancies?limit=1",
        ] {
            let response = app
                .clone()
                .oneshot(request("GET", path, Some(READ_SECRET), None, None)?)
                .await?;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body = json_body(response).await?;
            if path != "/v1/operator/overview" {
                assert_eq!(object_keys(&body), BTreeSet::from(["items", "next_before"]));
            }
        }
        let first = json_body(
            app.clone()
                .oneshot(request(
                    "GET",
                    "/v1/operator/reconciliation/runs?limit=1",
                    Some(READ_SECRET),
                    None,
                    None,
                )?)
                .await?,
        )
        .await?;
        let before = first["next_before"]
            .as_str()
            .ok_or("first page lacks cursor")?;
        let second = json_body(
            app.clone()
                .oneshot(request(
                    "GET",
                    &format!("/v1/operator/reconciliation/runs?limit=1&before={before}"),
                    Some(READ_SECRET),
                    None,
                    None,
                )?)
                .await?,
        )
        .await?;
        assert_eq!(second["items"].as_array().map(Vec::len), Some(1));
        let first_discrepancy = json_body(
            app.clone()
                .oneshot(request(
                    "GET",
                    "/v1/operator/reconciliation/discrepancies?limit=1",
                    Some(READ_SECRET),
                    None,
                    None,
                )?)
                .await?,
        )
        .await?;
        assert_eq!(
            object_keys(&first_discrepancy["items"][0]),
            BTreeSet::from([
                "asset_id",
                "created_at",
                "detail",
                "id",
                "kind",
                "money_affected",
                "payment_intent_id",
                "resolution",
                "resolved_at",
                "resolved_by",
                "run_id",
                "transfer_id"
            ])
        );
        let discrepancy_cursor = first_discrepancy["next_before"]
            .as_str()
            .ok_or("first discrepancy page lacks cursor")?;
        let last_discrepancy = json_body(app.clone().oneshot(request("GET", &format!("/v1/operator/reconciliation/discrepancies?limit=1&before={discrepancy_cursor}"), Some(READ_SECRET), None, None)?).await?).await?;
        assert_eq!(last_discrepancy["next_before"], Value::Null);
        let evidence = json_body(
            app.clone()
                .oneshot(request(
                    "GET",
                    &format!("/v1/operator/payment-intents/{intent_id}"),
                    Some(READ_SECRET),
                    None,
                    None,
                )?)
                .await?,
        )
        .await?;
        assert_eq!(
            object_keys(&evidence),
            BTreeSet::from([
                "allocations",
                "attempts",
                "fulfillment",
                "intent",
                "payment_events",
                "settlement_decisions",
                "transfers"
            ]),
            "{evidence:?}"
        );
        assert_eq!(evidence["intent"]["amount_minor"], "9007199254740993");
        assert_eq!(
            evidence["attempts"][0]["expected_amount_raw"],
            "9007199254740994"
        );
        let forbidden = app
            .clone()
            .oneshot(request(
                "GET",
                "/v1/operator/overview",
                Some(INGEST_SECRET),
                None,
                None,
            )?)
            .await?;
        assert_error(forbidden, StatusCode::FORBIDDEN, "missing_scope").await?;
        let unauthorized = app
            .clone()
            .oneshot(request("GET", "/v1/operator/overview", None, None, None)?)
            .await?;
        assert_error(
            unauthorized,
            StatusCode::UNAUTHORIZED,
            "authentication_failed",
        )
        .await?;
        let metrics = app
            .oneshot(request("GET", "/metrics", Some(READ_SECRET), None, None)?)
            .await?;
        assert_eq!(metrics.status(), StatusCode::OK);
        assert_eq!(
            metrics.headers()[header::CONTENT_TYPE],
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let text = String::from_utf8(to_bytes(metrics.into_body(), 1024 * 1024).await?.to_vec())?;
        assert!(
            text.contains("gateway_component_state{component=\"verifier\",state=\"degraded\"} 1")
        );
        assert!(text.contains("gateway_rail_stops_open"));
        assert!(text.contains("gateway_reconciliation_open_discrepancies{kind=\"allocation_exceeds_transfer\",money_affected=\"true\"} 1"));
        assert!(text.contains("gateway_transfers_processing{state=\"unmatched\"} 1"));
        assert!(text.contains("gateway_payment_intents{status=\"risk_hold\"} 1"));
        assert!(text.contains("gateway_outbox_events_dead_lettered 1"));
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn reset_database(pool: &PgPool) -> Result<(), Box<dyn Error>> {
        migrate(pool).await?;
        sqlx::query("TRUNCATE component_health, reconciliation_runs, operator_api_keys, chain_assets, merchants CASCADE")
            .execute(pool)
            .await?;
        for (merchant_id, key_id, external_id, secret) in [
            (MERCHANT_ONE, KEY_ONE, "merchant-one", SECRET_ONE),
            (MERCHANT_TWO, KEY_TWO, "merchant-two", SECRET_TWO),
        ] {
            sqlx::query(
                "INSERT INTO merchants (id, external_id, display_name, status) \
                 VALUES ($1, $2, $2, 'active')",
            )
            .bind(merchant_id)
            .bind(external_id)
            .execute(pool)
            .await?;
            let secret_hash: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
            sqlx::query(
                "INSERT INTO merchant_api_keys \
                 (id, merchant_id, key_prefix, secret_hash, label) \
                 VALUES ($1, $2, 'cg_test', $3, 'integration test')",
            )
            .bind(key_id)
            .bind(merchant_id)
            .bind(secret_hash.as_slice())
            .execute(pool)
            .await?;
        }
        sqlx::query(
            r"
            INSERT INTO chain_assets (
                id, chain, network, chain_environment, contract_address_key,
                display_symbol, decimals, status, pinned_sha256, approved_by
            ) VALUES (
                $1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active',
                encode(sha256($2), 'hex'), 'test-fixture'
            )
            ",
        )
        .bind(ASSET_ID)
        .bind([7_u8; 20].as_slice())
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO collector_addresses (
                id, asset_id, address_key, address_text, state, valid_from,
                pinned_sha256, approved_by
            ) VALUES (
                $1, $2, $3, 'TContractCollector', 'active', now(),
                encode(sha256($3), 'hex'), 'test-fixture'
            )
            ",
        )
        .bind(COLLECTOR_ID)
        .bind(ASSET_ID)
        .bind([8_u8; 21].as_slice())
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO price_snapshots (
                id, asset_id, fiat_currency, rate_numerator, rate_denominator,
                sources, observed_at
            ) VALUES ($1, $2, 'USD', 1, 10, $3, now())
            ",
        )
        .bind(PRICE_SNAPSHOT_ID)
        .bind(ASSET_ID)
        .bind(json!([
            {"provider_group":"pricing-a","observed_at":"current"},
            {"provider_group":"pricing-b","observed_at":"current"}
        ]))
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO quote_policies (
                id, asset_id, fiat_currency, version, status,
                quote_ttl_seconds, late_payment_window_seconds,
                amount_slot_count, max_price_age_seconds,
                max_policy_age_seconds, max_rail_health_age_seconds, observed_at
            ) VALUES ($1, $2, 'USD', 'policy-v1', 'active', 900, 2592000,
                      10000, 300, 300, 300, now())
            ",
        )
        .bind(QUOTE_POLICY_ID)
        .bind(ASSET_ID)
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO rail_health_snapshots (id, asset_id, health, observed_at)
            VALUES ($1, $2, 'healthy', now())
            ",
        )
        .bind(RAIL_HEALTH_SNAPSHOT_ID)
        .bind(ASSET_ID)
        .execute(pool)
        .await?;
        Ok(())
    }

    fn request(
        method: &str,
        uri: &str,
        secret: Option<&str>,
        idempotency_key: Option<&str>,
        body: Option<Value>,
    ) -> Result<Request<Body>, http::Error> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(secret) = secret {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {secret}"));
        }
        if let Some(idempotency_key) = idempotency_key {
            builder = builder.header("idempotency-key", idempotency_key);
        }
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        builder.body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
    }

    async fn json_body(response: axum::response::Response) -> Result<Value, Box<dyn Error>> {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn assert_error(
        response: axum::response::Response,
        status: StatusCode,
        code: &str,
    ) -> Result<(), Box<dyn Error>> {
        assert_eq!(response.status(), status);
        assert_eq!(json_body(response).await?["error"]["code"], code);
        Ok(())
    }

    fn object_keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .into_iter()
            .flat_map(serde_json::Map::keys)
            .map(String::as_str)
            .collect()
    }
}
