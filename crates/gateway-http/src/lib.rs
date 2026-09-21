mod auth;
mod error;
mod handlers;

use std::{sync::Arc, time::Duration};

use axum::{Router, http::StatusCode, middleware, routing::get};
use gateway_application::{PaymentIntentService, QuoteService, SystemClock};
use gateway_storage::{PgPool, PostgresRepository};
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{catch_panic::CatchPanicLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::{auth::authenticate, handlers::health};

#[derive(Debug, Clone)]
pub struct AppState {
    pub repository: Arc<PostgresRepository>,
    pub payment_intents: Arc<PaymentIntentService<PostgresRepository, SystemClock>>,
    pub quotes: Arc<QuoteService<PostgresRepository, SystemClock>>,
    pub pool: PgPool,
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
        Self {
            repository,
            payment_intents,
            quotes,
            pool,
        }
    }
}

pub fn router(state: AppState) -> Router {
    let protected = handlers::payment_intent_routes()
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));

    Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        .merge(protected)
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
    // One linear scenario proves the cross-request idempotency, isolation, and
    // audit invariants against the same database state.
    #[allow(clippy::too_many_lines)]
    async fn payment_intent_api_contract_and_isolation() -> Result<(), Box<dyn Error>> {
        let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
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

    async fn reset_database(pool: &PgPool) -> Result<(), Box<dyn Error>> {
        migrate(pool).await?;
        sqlx::query("TRUNCATE chain_assets, merchants CASCADE")
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
                display_symbol, decimals, status
            ) VALUES ($1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active')
            ",
        )
        .bind(ASSET_ID)
        .bind([7_u8; 20].as_slice())
        .execute(pool)
        .await?;
        sqlx::query(
            r"
            INSERT INTO collector_addresses (
                id, asset_id, address_key, address_text, state, valid_from
            ) VALUES ($1, $2, $3, 'TContractCollector', 'active', now())
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
