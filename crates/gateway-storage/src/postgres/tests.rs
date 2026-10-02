use std::{env, error::Error, str::FromStr, sync::Arc};

use gateway_application::{
    Clock, IdempotentCreate, IdempotentQuote, IssueQuote, PaymentIntentRepository, QuoteRepository,
    QuoteService, QuoteServiceError, RepositoryError,
};
use gateway_domain::{
    CurrencyCode, FiatAmount, PriceSnapshot, QuotePlan, QuotePolicySnapshot, RailHealth,
    RailHealthSnapshot, RawAmount,
};
use gateway_scheduler::{BatchConfig, ExpiryScheduler, RetryPolicy};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::postgres::PostgresRepository;
use crate::{migrate, test_support::DATABASE};

const MERCHANT_ONE: Uuid = Uuid::from_u128(101);
const MERCHANT_TWO: Uuid = Uuid::from_u128(102);
const ACTOR_KEY: Uuid = Uuid::from_u128(201);
const ASSET_ID: Uuid = Uuid::from_u128(301);
const COLLECTOR_ID: Uuid = Uuid::from_u128(401);
const PRICE_SNAPSHOT_ID: Uuid = Uuid::from_u128(601);
const QUOTE_POLICY_ID: Uuid = Uuid::from_u128(602);
const RAIL_HEALTH_SNAPSHOT_ID: Uuid = Uuid::from_u128(603);
const INTENT_ONE: Uuid = Uuid::from_u128(501);
const INTENT_TWO: Uuid = Uuid::from_u128(502);

#[derive(Debug, Clone, Copy)]
struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn quote_leases_are_concurrent_replayable_isolated_and_archived_exactly_once()
-> Result<(), Box<dyn Error>> {
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    let repository = PostgresRepository::new(pool.clone());
    let first_plan = plan(MERCHANT_ONE, INTENT_ONE, now)?;
    let first_quote_id = first_plan.quote_id();
    let second_plan = plan(MERCHANT_ONE, INTENT_TWO, now)?;
    let first_repository = repository.clone();
    let second_repository = repository.clone();

    let first = first_repository.issue_quote_idempotently(
        first_plan,
        ACTOR_KEY,
        "POST /v1/payment-intents/:id/quotes",
        "quote_first_000000000001",
        &[1; 32],
    );
    let second = second_repository.issue_quote_idempotently(
        second_plan,
        ACTOR_KEY,
        "POST /v1/payment-intents/:id/quotes",
        "quote_second_00000000001",
        &[2; 32],
    );
    let (first, second) = tokio::join!(first, second);
    let first = match first? {
        IdempotentQuote::Issued(quote) => quote,
        IdempotentQuote::Replayed(_) => return Err("first issue unexpectedly replayed".into()),
    };
    let second = match second? {
        IdempotentQuote::Issued(quote) => quote,
        IdempotentQuote::Replayed(_) => return Err("second issue unexpectedly replayed".into()),
    };
    let mut amounts = [first.amount_raw.to_string(), second.amount_raw.to_string()];
    amounts.sort();
    assert_eq!(amounts, ["100".to_owned(), "101".to_owned()]);

    let replay_plan = plan(MERCHANT_ONE, INTENT_ONE, now)?;
    let replay = repository
        .issue_quote_idempotently(
            replay_plan,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "quote_first_000000000001",
            &[1; 32],
        )
        .await?;
    match replay {
        IdempotentQuote::Replayed(quote) => assert_eq!(quote.id, first_quote_id),
        IdempotentQuote::Issued(_) => return Err("replay unexpectedly issued a quote".into()),
    }

    let foreign = repository
        .issue_quote_idempotently(
            plan(MERCHANT_TWO, INTENT_ONE, now)?,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "quote_foreign_00000000001",
            &[3; 32],
        )
        .await;
    assert!(matches!(
        foreign,
        Err(RepositoryError::PaymentIntentNotQuotable)
    ));

    // Two sweeps run at once, as an overlapping scheduler tick or a second
    // replica would. Each attempt must expire exactly once.
    let quote_expiry = now + Duration::minutes(15);
    let (first_sweep, second_sweep) = tokio::join!(
        repository.expire_quotes_and_archive_leases(quote_expiry, 100),
        repository.expire_quotes_and_archive_leases(quote_expiry, 100)
    );
    let first_sweep = first_sweep?;
    let second_sweep = second_sweep?;
    assert_eq!(first_sweep.quotes_expired + second_sweep.quotes_expired, 2);
    assert_eq!(
        first_sweep.leases_archived + second_sweep.leases_archived,
        0
    );
    assert_eq!(count(&pool, "amount_leases").await?, 2);
    assert_eq!(count(&pool, "amount_lease_history").await?, 0);

    let before_late_window = repository
        .expire_quotes_and_archive_leases(quote_expiry + Duration::days(29), 100)
        .await?;
    assert_eq!(before_late_window.quotes_expired, 0);
    assert_eq!(before_late_window.leases_archived, 0);
    assert_eq!(count(&pool, "amount_leases").await?, 2);

    let late_deadline = quote_expiry + Duration::days(30);
    let (first_archive, second_archive) = tokio::join!(
        repository.expire_quotes_and_archive_leases(late_deadline, 100),
        repository.expire_quotes_and_archive_leases(late_deadline, 100)
    );
    let archived_leases = first_archive?.leases_archived + second_archive?.leases_archived;
    assert_eq!(archived_leases, 2);
    assert_eq!(count(&pool, "amount_leases").await?, 0);
    assert_eq!(count(&pool, "amount_lease_history").await?, 2);

    let issued_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'payment_quote.issued'",
    )
    .fetch_one(&pool)
    .await?;
    let archived_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'amount_lease.archived'",
    )
    .fetch_one(&pool)
    .await?;
    let attempt_expiry_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'payment_attempt.expired'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(issued_audits, 2);
    assert_eq!(archived_audits, 2);
    assert_eq!(attempt_expiry_audits, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn quotes_spread_over_the_address_pool_and_stop_at_its_limits() -> Result<(), Box<dyn Error>>
{
    const SECOND_COLLECTOR: Uuid = Uuid::from_u128(403);
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    sqlx::query(
        r"INSERT INTO collector_addresses (id, asset_id, address_key, address_text, state,
              valid_from, pinned_sha256, approved_by)
          VALUES ($1, $2, $3, 'TSecondCollector', 'active', $4, encode(sha256($3), 'hex'),
              'test-fixture')",
    )
    .bind(SECOND_COLLECTOR)
    .bind(ASSET_ID)
    .bind([10_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(&pool)
    .await?;
    // Five orders at one price. The seeded policy allows two exact amounts
    // per price on an address, so two addresses hold four of them.
    let intents: Vec<Uuid> = (0..5_u128).map(|n| Uuid::from_u128(510 + n)).collect();
    for (position, intent) in intents.iter().enumerate() {
        sqlx::query(
            r"INSERT INTO payment_intents (id, merchant_id, amount_minor, currency, status,
                  reference, created_at, updated_at)
              VALUES ($1, $2, 1000, 'USD', 'requires_quote', $3, $4, $4)",
        )
        .bind(intent)
        .bind(MERCHANT_ONE)
        .bind(format!("pool-order-{position}"))
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(&pool)
        .await?;
    }
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let quotes = QuoteService::new(Arc::clone(&repository), FixedClock(now));
    let mut collectors = Vec::new();
    for (position, intent) in intents.iter().take(4).enumerate() {
        let issued = quotes
            .issue(
                MERCHANT_ONE,
                ACTOR_KEY,
                &format!("pool_quote_{position:012}"),
                IssueQuote {
                    payment_intent_id: *intent,
                    asset_id: ASSET_ID,
                },
            )
            .await?;
        collectors.push(issued.quote.collector_address_id);
    }
    // Least loaded first, the older address on a tie.
    assert_eq!(
        collectors,
        vec![
            COLLECTOR_ID,
            SECOND_COLLECTOR,
            COLLECTOR_ID,
            SECOND_COLLECTOR
        ]
    );

    // The fifth order finds both amounts taken on the first address, tries
    // the second, and is refused there too; nothing is written for it.
    let fifth = quotes
        .issue(
            MERCHANT_ONE,
            ACTOR_KEY,
            "pool_quote_000000000004",
            IssueQuote {
                payment_intent_id: intents[4],
                asset_id: ASSET_ID,
            },
        )
        .await;
    assert!(
        matches!(
            fifth,
            Err(QuoteServiceError::Repository(
                RepositoryError::AmountSlotsExhausted
            ))
        ),
        "{fifth:?}"
    );
    let snapshot = quotes.metrics().snapshot();
    assert_eq!(snapshot.spillovers, 1);
    assert_eq!(count(&pool, "amount_leases").await?, 4);
    let idempotency_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_idempotency_records WHERE idempotency_key = 'pool_quote_000000000004'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(idempotency_rows, 0);

    // With a cap of two reservations per address, both addresses are full
    // and the refusal says so instead of trying them.
    let capped = QuoteService::new(Arc::clone(&repository), FixedClock(now))
        .with_max_open_leases_per_collector(2)
        .issue(
            MERCHANT_ONE,
            ACTOR_KEY,
            "pool_quote_capped_000001",
            IssueQuote {
                payment_intent_id: intents[4],
                asset_id: ASSET_ID,
            },
        )
        .await;
    assert!(
        matches!(capped, Err(QuoteServiceError::CapacityExhausted)),
        "{capped:?}"
    );
    let occupancy = repository.collector_occupancy().await?;
    assert_eq!(
        occupancy
            .iter()
            .map(|row| (row.collector_id, row.open_leases, row.live_leases))
            .collect::<Vec<_>>(),
        vec![(COLLECTOR_ID, 2, 2), (SECOND_COLLECTOR, 2, 2)]
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn the_scheduler_expires_and_archives_through_the_application_stack()
-> Result<(), Box<dyn Error>> {
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let issued_at = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, issued_at).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    QuoteService::new(Arc::clone(&repository), FixedClock(issued_at))
        .issue(
            MERCHANT_ONE,
            ACTOR_KEY,
            "quote_scheduler_00000001",
            IssueQuote {
                payment_intent_id: INTENT_ONE,
                asset_id: ASSET_ID,
            },
        )
        .await?;

    // The scheduler drives the real service, which drives the real
    // transaction: nothing in this path is a test double.
    let after_late_window = issued_at + Duration::days(31);
    let scheduler = ExpiryScheduler::new(
        Arc::new(QuoteService::new(
            Arc::clone(&repository),
            FixedClock(after_late_window),
        )),
        BatchConfig {
            interval: std::time::Duration::from_secs(1),
            batch_limit: 100,
            max_batches_per_tick: 4,
            lease_seconds: 30,
            retry: RetryPolicy::default(),
        },
    )?;

    let report = scheduler.run_once().await?;

    assert_eq!(report.quotes_expired, 1);
    assert_eq!(report.leases_archived, 1);
    assert_eq!(report.retries, 0);
    assert!(report.drained);
    assert_eq!(count(&pool, "amount_leases").await?, 0);
    assert_eq!(count(&pool, "amount_lease_history").await?, 1);
    let intent_status: String =
        sqlx::query_scalar("SELECT status FROM payment_intents WHERE id = $1")
            .bind(INTENT_ONE)
            .fetch_one(&pool)
            .await?;
    assert_eq!(intent_status, "expired");
    let metrics = scheduler.metrics().snapshot();
    assert_eq!(metrics.runs_succeeded, 1);
    assert_eq!(metrics.runs_failed, 0);
    assert_eq!(metrics.batches_executed, 1);
    assert_eq!(metrics.backlog_left, 0);

    // A second sweep has nothing left to do and must not re-archive.
    let repeat = scheduler.run_once().await?;
    assert_eq!(repeat.quotes_expired, 0);
    assert_eq!(repeat.leases_archived, 0);
    assert_eq!(count(&pool, "amount_lease_history").await?, 1);
    Ok(())
}

fn plan(
    merchant_id: Uuid,
    payment_intent_id: Uuid,
    now: OffsetDateTime,
) -> Result<QuotePlan, Box<dyn Error>> {
    Ok(QuotePlan::build(
        merchant_id,
        payment_intent_id,
        ASSET_ID,
        COLLECTOR_ID,
        "TTestCollector".to_owned(),
        FiatAmount::positive(CurrencyCode::new("USD")?, 1_000)?,
        Some(PriceSnapshot {
            id: PRICE_SNAPSHOT_ID,
            rate_numerator: RawAmount::from_str("1")?,
            rate_denominator: RawAmount::from_str("10")?,
            sources: json!([
                {"provider_group":"source-a","observed_at":now},
                {"provider_group":"source-b","observed_at":now}
            ]),
            observed_at: now,
        }),
        Some(QuotePolicySnapshot {
            id: QUOTE_POLICY_ID,
            version: "policy-v1".to_owned(),
            quote_ttl_seconds: 900,
            late_payment_window_seconds: 2_592_000,
            amount_slot_count: 2,
            max_price_age_seconds: 60,
            max_policy_age_seconds: 60,
            max_rail_health_age_seconds: 60,
            observed_at: now,
        }),
        Some(RailHealthSnapshot {
            id: RAIL_HEALTH_SNAPSHOT_ID,
            health: RailHealth::Healthy,
            observed_at: now,
        }),
        now,
    )?)
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn a_merchant_on_its_own_policy_is_quoted_only_on_its_own_address()
-> Result<(), Box<dyn Error>> {
    const COLLECTOR_TWO: Uuid = Uuid::from_u128(402);
    const MERCHANT_THREE: Uuid = Uuid::from_u128(103);
    const INTENT_THREE: Uuid = Uuid::from_u128(503);
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;

    // Two merchants that keep their own keys, each with its own address,
    // and a third that has not registered one yet.
    sqlx::query("UPDATE merchants SET collector_policy = 'own'")
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO merchants (id, external_id, display_name, status) \
         VALUES ($1, 'quote-merchant-three', 'quote-merchant-three', 'active')",
    )
    .bind(MERCHANT_THREE)
    .execute(&pool)
    .await?;
    sqlx::query("UPDATE collector_addresses SET merchant_id = $2 WHERE id = $1")
        .bind(COLLECTOR_ID)
        .bind(MERCHANT_ONE)
        .execute(&pool)
        .await?;
    sqlx::query(
        r"INSERT INTO collector_addresses (id, asset_id, address_key, address_text, state,
              valid_from, pinned_sha256, approved_by, merchant_id)
          VALUES ($1, $2, $3, 'TMerchantTwo', 'active', $4, encode(sha256($3), 'hex'),
              'test-fixture', $5)",
    )
    .bind(COLLECTOR_TWO)
    .bind(ASSET_ID)
    .bind([9_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .bind(MERCHANT_TWO)
    .execute(&pool)
    .await?;
    sqlx::query(
        r"INSERT INTO payment_intents (id, merchant_id, amount_minor, currency, status,
              reference, created_at, updated_at)
          VALUES ($1, $2, 1000, 'USD', 'requires_quote', 'order-three', $3, $3)",
    )
    .bind(INTENT_THREE)
    .bind(MERCHANT_TWO)
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(&pool)
    .await?;
    let repository = PostgresRepository::new(pool.clone());
    let usd = CurrencyCode::new("USD")?;

    // Each merchant is offered only its own address.
    assert_eq!(
        repository
            .load_quote_context(MERCHANT_ONE, ASSET_ID, &usd)
            .await?
            .candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        vec![COLLECTOR_ID]
    );
    assert_eq!(
        repository
            .load_quote_context(MERCHANT_TWO, ASSET_ID, &usd)
            .await?
            .candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>(),
        vec![COLLECTOR_TWO]
    );
    // No address of its own is no quote, never someone else's address.
    assert!(matches!(
        repository
            .load_quote_context(MERCHANT_THREE, ASSET_ID, &usd)
            .await,
        Err(RepositoryError::CollectorUnavailable)
    ));

    // A plan that names another merchant's address is refused when issued.
    let foreign = repository
        .issue_quote_idempotently(
            plan(MERCHANT_TWO, INTENT_THREE, now)?,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "quote_foreign_collector_01",
            &[3; 32],
        )
        .await;
    assert!(
        matches!(foreign, Err(RepositoryError::CollectorUnavailable)),
        "{foreign:?}"
    );
    let issued = repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, now)?,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "quote_own_collector_00001",
            &[4; 32],
        )
        .await?;
    assert!(matches!(issued, IdempotentQuote::Issued(_)));

    // And the database refuses the same quote written by hand.
    let written = sqlx::query(
        r"INSERT INTO payment_quotes(
             id,merchant_id,payment_intent_id,asset_id,collector_address_id,fiat_currency,
             fiat_amount_minor,base_amount_raw,amount_raw,rate_numerator,rate_denominator,
             price_sources,price_observed_at,policy_version,rail_health_observed_at,
             created_at,expires_at,late_payment_until,price_snapshot_id,quote_policy_id,
             rail_health_snapshot_id
           ) VALUES($1,$2,$3,$4,$5,'USD',1000,10000,10000,1,10,'[{}]'::jsonb,$6,'policy-v1',$6,
                    $6,$7,$8,$9,$10,$11)",
    )
    .bind(Uuid::from_u128(701))
    .bind(MERCHANT_TWO)
    .bind(INTENT_THREE)
    .bind(ASSET_ID)
    .bind(COLLECTOR_ID)
    .bind(now)
    .bind(now + Duration::minutes(15))
    .bind(now + Duration::days(30))
    .bind(PRICE_SNAPSHOT_ID)
    .bind(QUOTE_POLICY_ID)
    .bind(RAIL_HEALTH_SNAPSHOT_ID)
    .execute(&pool)
    .await;
    let refused = matches!(&written, Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23514"));
    assert!(
        refused,
        "expected the tenancy trigger to refuse, got {written:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_partially_paid_intent_never_stops_the_expiry_of_the_others() -> Result<(), Box<dyn Error>>
{
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    let repository = PostgresRepository::new(pool.clone());
    for (intent, key, hash) in [
        (INTENT_ONE, "expiry_partial_quote_0001", [5_u8; 32]),
        (INTENT_TWO, "expiry_partial_quote_0002", [6_u8; 32]),
    ] {
        repository
            .issue_quote_idempotently(
                plan(MERCHANT_ONE, intent, now)?,
                ACTOR_KEY,
                "POST /v1/payment-intents/:id/quotes",
                key,
                &hash,
            )
            .await?;
    }
    // Money arrived short on the first order before its quote ran out.
    sqlx::query("UPDATE payment_intents SET status = 'partially_paid' WHERE id = $1")
        .bind(INTENT_ONE)
        .execute(&pool)
        .await?;

    repository
        .expire_quotes_and_archive_leases(now + Duration::hours(1), 50)
        .await?;

    let states: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT i.id, i.status, a.status FROM payment_intents i              JOIN payment_attempts a ON a.payment_intent_id = i.id ORDER BY i.id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        states,
        vec![
            (
                INTENT_ONE,
                "partially_paid".to_owned(),
                "expired".to_owned()
            ),
            (INTENT_TWO, "expired".to_owned(), "expired".to_owned()),
        ]
    );
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'payment_intent.quote_window_closed'              AND resource_id = $1",
    )
    .bind(INTENT_ONE)
    .fetch_one(&pool)
    .await?;
    assert_eq!(kept, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn an_order_whose_quote_ran_out_unpaid_is_quoted_again_and_only_then()
-> Result<(), Box<dyn Error>> {
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    let repository = PostgresRepository::new(pool.clone());
    let route = "POST /v1/payment-intents/:id/quotes";
    repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, now)?,
            ACTOR_KEY,
            route,
            "requote_first_quote_0001",
            &[7; 32],
        )
        .await?;
    // While the first quote is live, the order cannot be quoted again.
    let second_live = repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, now)?,
            ACTOR_KEY,
            route,
            "requote_while_live_00001",
            &[8; 32],
        )
        .await;
    assert!(
        matches!(second_live, Err(RepositoryError::PaymentIntentNotQuotable)),
        "{second_live:?}"
    );

    let later = now + Duration::hours(2);
    repository
        .expire_quotes_and_archive_leases(later, 50)
        .await?;
    refresh_evidence(&pool, later).await?;
    let requoted = repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, later)?,
            ACTOR_KEY,
            route,
            "requote_after_expiry_001",
            &[9; 32],
        )
        .await?;
    assert!(matches!(requoted, IdempotentQuote::Issued(_)));
    let attempts: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM payment_attempts WHERE payment_intent_id = $1 ORDER BY created_at",
    )
    .bind(INTENT_ONE)
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        attempts,
        vec!["expired".to_owned(), "awaiting_payment".to_owned()]
    );
    // The first attempt still holds its amount for late money.
    let leases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM amount_leases AS lease JOIN payment_attempts AS attempt              ON attempt.id = lease.attempt_id WHERE attempt.payment_intent_id = $1",
    )
    .bind(INTENT_ONE)
    .fetch_one(&pool)
    .await?;
    assert_eq!(leases, 2);

    // Once money is claimed for the order, it is never quoted again.
    let latest = later + Duration::hours(2);
    repository
        .expire_quotes_and_archive_leases(latest, 50)
        .await?;
    let transfer = Uuid::from_u128(801);
    sqlx::query(
        r"INSERT INTO chain_transfers(
             id,asset_id,collector_address_id,chain,network,chain_environment,tx_hash,
             event_index,block_number,block_hash,block_time,token_key,from_address_key,
             from_address_text,to_address_key,to_address_text,amount_raw,decimals,
             canonicalization_policy,verifier_version,canonicalized_at
           ) VALUES($1,$2,$3,'tron','nile','testnet','requote-tx',0,1,'b',$4,$5,$6,'TFrom',$7,'TTestCollector',1,6,'t','t',$4)",
    )
    .bind(transfer)
    .bind(ASSET_ID)
    .bind(COLLECTOR_ID)
    .bind(latest)
    .bind([7_u8; 20].as_slice())
    .bind([1_u8; 21].as_slice())
    .bind([8_u8; 21].as_slice())
    .execute(&pool)
    .await?;
    sqlx::query(
        r"INSERT INTO chain_transfer_intent_claims (transfer_id, payment_intent_id, attempt_id,
              merchant_id, match_strategy, claimed_at, collector_address_id)
          SELECT $1, attempt.payment_intent_id, attempt.id, attempt.merchant_id, 'manual', $2,
                 attempt.collector_address_id
            FROM payment_attempts AS attempt
           WHERE attempt.payment_intent_id = $3
           ORDER BY attempt.created_at LIMIT 1",
    )
    .bind(transfer)
    .bind(latest)
    .bind(INTENT_ONE)
    .execute(&pool)
    .await?;
    refresh_evidence(&pool, latest).await?;
    let with_money = repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, latest)?,
            ACTOR_KEY,
            route,
            "requote_with_money_00001",
            &[10; 32],
        )
        .await;
    assert!(
        matches!(with_money, Err(RepositoryError::PaymentIntentNotQuotable)),
        "{with_money:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn an_order_without_money_is_cancelled_once_and_one_with_money_never()
-> Result<(), Box<dyn Error>> {
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    let repository = PostgresRepository::new(pool.clone());
    repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, now)?,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/quotes",
            "cancel_scenario_quote_001",
            &[11; 32],
        )
        .await?;
    let route = "POST /v1/payment-intents/:id/cancel";

    let cancelled = repository
        .cancel_idempotently(
            MERCHANT_ONE,
            INTENT_ONE,
            ACTOR_KEY,
            route,
            "cancel_scenario_key_0001",
            &[1; 32],
            Some("customer left"),
        )
        .await?;
    assert!(
        matches!(cancelled, Some(IdempotentCreate::Created(ref intent)) if intent.status.as_str() == "cancelled")
    );
    let attempt: String =
        sqlx::query_scalar("SELECT status FROM payment_attempts WHERE payment_intent_id = $1")
            .bind(INTENT_ONE)
            .fetch_one(&pool)
            .await?;
    assert_eq!(attempt, "cancelled");
    let leases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM amount_leases AS lease JOIN payment_attempts AS attempt              ON attempt.id = lease.attempt_id WHERE attempt.payment_intent_id = $1",
    )
    .bind(INTENT_ONE)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        leases, 1,
        "the amount stays reserved for money that still arrives"
    );
    let webhooks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM domain_events WHERE event_type = 'payment_intent.cancelled' AND aggregate_id = $1",
    )
    .bind(INTENT_ONE)
    .fetch_one(&pool)
    .await?;
    assert_eq!(webhooks, 1);

    // The same request again is the same answer; the same key with a
    // different body is a conflict.
    let replay = repository
        .cancel_idempotently(
            MERCHANT_ONE,
            INTENT_ONE,
            ACTOR_KEY,
            route,
            "cancel_scenario_key_0001",
            &[1; 32],
            Some("customer left"),
        )
        .await?;
    assert!(matches!(replay, Some(IdempotentCreate::Replayed(_))));
    assert!(matches!(
        repository
            .cancel_idempotently(
                MERCHANT_ONE,
                INTENT_ONE,
                ACTOR_KEY,
                route,
                "cancel_scenario_key_0001",
                &[2; 32],
                None
            )
            .await,
        Err(RepositoryError::IdempotencyConflict)
    ));

    // An order with money on it is never cancelled.
    sqlx::query("UPDATE payment_intents SET status = 'partially_paid' WHERE id = $1")
        .bind(INTENT_TWO)
        .execute(&pool)
        .await?;
    assert!(matches!(
        repository
            .cancel_idempotently(
                MERCHANT_ONE,
                INTENT_TWO,
                ACTOR_KEY,
                route,
                "cancel_scenario_key_0002",
                &[3; 32],
                None
            )
            .await,
        Err(RepositoryError::PaymentIntentNotCancellable)
    ));
    // Another merchant's order does not exist for this merchant.
    assert!(
        repository
            .cancel_idempotently(
                MERCHANT_TWO,
                INTENT_TWO,
                ACTOR_KEY,
                route,
                "cancel_scenario_key_0003",
                &[4; 32],
                None
            )
            .await?
            .is_none()
    );
    let twice: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM domain_events WHERE event_type = 'payment_intent.cancelled'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(twice, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn the_payment_page_shows_the_attempt_and_never_calls_seen_money_paid()
-> Result<(), Box<dyn Error>> {
    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let route = "POST /v1/payment-intents/:id/quotes";
    let mut tokens = Vec::new();
    for (intent, key, hash) in [
        (INTENT_ONE, "checkout_scenario_quote1", [21_u8; 32]),
        (INTENT_TWO, "checkout_scenario_quote2", [22_u8; 32]),
    ] {
        let IdempotentQuote::Issued(quote) = repository
            .issue_quote_idempotently(
                plan(MERCHANT_ONE, intent, now)?,
                ACTOR_KEY,
                route,
                key,
                &hash,
            )
            .await?
        else {
            return Err("the quote was not issued".into());
        };
        tokens.push(quote.checkout_token);
    }
    let checkout = gateway_application::CheckoutService::new(Arc::clone(&repository));

    let waiting = checkout
        .view(&tokens[0])
        .await?
        .ok_or("no page for a live quote")?;
    assert_eq!(waiting.status, gateway_application::CheckoutStatus::Waiting);
    assert_eq!(waiting.amount, waiting.amount_raw.to_decimal_string(6));
    assert!(waiting.asset.contract_address.starts_with('T'));
    assert_eq!(waiting.collector_address, "TTestCollector");
    assert_eq!(waiting.received, "0");

    repository
        .cancel_idempotently(
            MERCHANT_ONE,
            INTENT_ONE,
            ACTOR_KEY,
            "POST /v1/payment-intents/:id/cancel",
            "checkout_scenario_cancel",
            &[1; 32],
            None,
        )
        .await?;
    let cancelled = checkout
        .view(&tokens[0])
        .await?
        .ok_or("no page after cancel")?;
    assert_eq!(
        cancelled.status,
        gateway_application::CheckoutStatus::Cancelled
    );

    sqlx::query("UPDATE payment_intents SET status = 'paid' WHERE id = $1")
        .bind(INTENT_TWO)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE payment_attempts SET status = 'settled' WHERE payment_intent_id = $1")
        .bind(INTENT_TWO)
        .execute(&pool)
        .await?;
    let paid = checkout
        .view(&tokens[1])
        .await?
        .ok_or("no page after payment")?;
    assert_eq!(paid.status, gateway_application::CheckoutStatus::Paid);

    // Unknown and malformed tokens are indistinguishable.
    assert!(checkout.view(&"0".repeat(64)).await?.is_none());
    assert!(checkout.view("not-a-token").await?.is_none());
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn a_collector_holding_a_reservation_stops_quoting_but_is_not_retired()
-> Result<(), Box<dyn Error>> {
    use gateway_application::{ProvisioningError, ProvisioningRepository};

    let _fixture = DATABASE.lock().await;
    let database_url = env::var("GATEWAY_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
    reset_database(&pool, now).await?;
    let repository = PostgresRepository::new(pool.clone());
    let route = "POST /v1/payment-intents/:id/quotes";
    repository
        .issue_quote_idempotently(
            plan(MERCHANT_ONE, INTENT_ONE, now)?,
            ACTOR_KEY,
            route,
            "retire_reserved_quote_01",
            &[11; 32],
        )
        .await?;

    repository
        .stop_quoting_collector("alice", COLLECTOR_ID, "moving to a new address")
        .await?;
    let refused_quote = repository
        .issue_quote_idempotently(
            plan(MERCHANT_TWO, INTENT_TWO, now)?,
            ACTOR_KEY,
            route,
            "retire_reserved_quote_02",
            &[12; 32],
        )
        .await;
    assert!(refused_quote.is_err(), "{refused_quote:?}");

    // The first order's amount is still reserved for late money.
    let refused = repository
        .retire_collector("alice", COLLECTOR_ID, "moved", false)
        .await;
    assert!(
        matches!(refused, Err(ProvisioningError::CollectorStillReserved(1))),
        "{refused:?}"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM collector_addresses WHERE id = $1")
        .bind(COLLECTOR_ID)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "receiving_only");

    repository
        .retire_collector("alice", COLLECTOR_ID, "key leaked", true)
        .await?;
    let audited: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM audit_events WHERE action = 'collector.retire' AND resource_id = $1",
    )
    .bind(COLLECTOR_ID)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        audited,
        json!({ "actor": "alice", "compromised": true, "open_reservations": 1 })
    );
    Ok(())
}

/// Moves the seeded price, policy and rail evidence to `at`, exactly as a
/// plan built at `at` describes it, so a later quote passes the issue-time
/// comparison with the stored evidence.
async fn refresh_evidence(pool: &PgPool, at: OffsetDateTime) -> Result<(), Box<dyn Error>> {
    sqlx::query("UPDATE price_snapshots SET observed_at = $1, sources = $2")
        .bind(at)
        .bind(json!([
            {"provider_group":"source-a","observed_at":at},
            {"provider_group":"source-b","observed_at":at}
        ]))
        .execute(pool)
        .await?;
    for table in ["quote_policies", "rail_health_snapshots"] {
        sqlx::query(&format!("UPDATE {table} SET observed_at = $1"))
            .bind(at)
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn reset_database(pool: &PgPool, now: OffsetDateTime) -> Result<(), Box<dyn Error>> {
    migrate(pool).await?;
    sqlx::query("TRUNCATE chain_assets, merchants CASCADE")
        .execute(pool)
        .await?;
    for (merchant_id, external_id) in [
        (MERCHANT_ONE, "quote-merchant-one"),
        (MERCHANT_TWO, "quote-merchant-two"),
    ] {
        sqlx::query(
            "INSERT INTO merchants (id, external_id, display_name, status, collector_policy) \
             VALUES ($1, $2, $2, 'active', 'shared')",
        )
        .bind(merchant_id)
        .bind(external_id)
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
            $1, $2, $3, 'TTestCollector', 'active', $4,
            encode(sha256($3), 'hex'), 'test-fixture'
        )
        ",
    )
    .bind(COLLECTOR_ID)
    .bind(ASSET_ID)
    .bind([8_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO price_snapshots (
            id, asset_id, fiat_currency, rate_numerator, rate_denominator,
            sources, observed_at
        ) VALUES ($1, $2, 'USD', 1, 10, $3, $4)
        ",
    )
    .bind(PRICE_SNAPSHOT_ID)
    .bind(ASSET_ID)
    .bind(json!([
        {"provider_group":"source-a","observed_at":now},
        {"provider_group":"source-b","observed_at":now}
    ]))
    .bind(now)
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
                  2, 60, 60, 60, $3)
        ",
    )
    .bind(QUOTE_POLICY_ID)
    .bind(ASSET_ID)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO rail_health_snapshots (id, asset_id, health, observed_at)
        VALUES ($1, $2, 'healthy', $3)
        ",
    )
    .bind(RAIL_HEALTH_SNAPSHOT_ID)
    .bind(ASSET_ID)
    .bind(now)
    .execute(pool)
    .await?;
    for (intent_id, reference) in [(INTENT_ONE, "order-one"), (INTENT_TWO, "order-two")] {
        sqlx::query(
            r"
            INSERT INTO payment_intents (
                id, merchant_id, amount_minor, currency, status, reference,
                created_at, updated_at
            ) VALUES ($1, $2, 1000, 'USD', 'requires_quote', $3, $4, $4)
            ",
        )
        .bind(intent_id)
        .bind(MERCHANT_ONE)
        .bind(reference)
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn count(pool: &PgPool, table: &str) -> Result<i64, Box<dyn Error>> {
    let query = match table {
        "amount_leases" => "SELECT count(*) FROM amount_leases",
        "amount_lease_history" => "SELECT count(*) FROM amount_lease_history",
        _ => return Err("unsupported test table".into()),
    };
    Ok(sqlx::query_scalar(query).fetch_one(pool).await?)
}
