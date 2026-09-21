use std::{
    error::Error,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_application::{
    ChainEventKey, ChainReader, ChainReaderError, ChainSource, Clock, ComponentLease,
    CreatePaymentIntent, IssueQuote, LeaseRepository, ObservationRepository, PaymentIntentService,
    ResolvedObservation, SettlementRepository, SettlementService, VerificationService,
};
use gateway_domain::{
    AddressKey, ChainEnvironment, ExecutionStatus, ObservationKind, ObservedTransfer, RawAmount,
    SourceFinality, TxHash,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

type TestResult = Result<(), Box<dyn Error>>;

const MERCHANT: Uuid = Uuid::from_u128(7_001);
const ACTOR_KEY: Uuid = Uuid::from_u128(7_002);
const ASSET_ID: Uuid = Uuid::from_u128(7_101);
const COLLECTOR_ID: Uuid = Uuid::from_u128(7_201);
const PRICE_SNAPSHOT: Uuid = Uuid::from_u128(7_301);
const QUOTE_POLICY: Uuid = Uuid::from_u128(7_302);
const RAIL_HEALTH: Uuid = Uuid::from_u128(7_303);
const FINALITY_POLICY: Uuid = Uuid::from_u128(7_304);
const SETTLEMENT_POLICY: Uuid = Uuid::from_u128(7_305);
const SOURCE_A: Uuid = Uuid::from_u128(7_401);
const SOURCE_B: Uuid = Uuid::from_u128(7_402);
const VERIFIER_SOURCE: Uuid = Uuid::from_u128(7_403);

#[derive(Debug, Clone)]
struct TestClock(Arc<Mutex<OffsetDateTime>>);

impl TestClock {
    fn at(moment: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(moment)))
    }

    fn advance(&self, by: Duration) {
        if let Ok(mut moment) = self.0.lock() {
            *moment += by;
        }
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        self.0
            .lock()
            .map(|moment| *moment)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

#[derive(Debug)]
struct ChainDouble(Mutex<ObservedTransfer>);

#[async_trait]
impl ChainReader for ChainDouble {
    async fn lookup(
        &self,
        _event: &ChainEventKey,
    ) -> Result<Option<ObservedTransfer>, ChainReaderError> {
        self.0
            .lock()
            .map(|transfer| Some(transfer.clone()))
            .map_err(|_| ChainReaderError::Unreachable("poisoned test lock".to_owned()))
    }
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn a_verified_payment_settles_once_and_only_once() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(OffsetDateTime::now_utc());

    // A merchant asks for money and the gateway reserves an exact amount.
    let intents = PaymentIntentService::new(Arc::clone(&repository), clock.clone());
    let intent = intents
        .create(
            MERCHANT,
            ACTOR_KEY,
            "settlement_intent_00000001",
            CreatePaymentIntent {
                amount_minor: "40000".to_owned(),
                currency: "USD".to_owned(),
                reference: "order-settlement-1".to_owned(),
                description: None,
                metadata: json!({}),
            },
        )
        .await?;
    let quotes = gateway_application::QuoteService::new(Arc::clone(&repository), clock.clone());
    let quote = quotes
        .issue(
            MERCHANT,
            ACTOR_KEY,
            "settlement_quote_000000001",
            IssueQuote {
                payment_intent_id: intent.intent.id,
                asset_id: ASSET_ID,
            },
        )
        .await?;
    let expected = quote.quote.amount_raw;

    // The chain shows exactly that amount arriving.
    let lease = repository
        .acquire_component_lease("payments:tron", "pod-payments:boot-1", 300, clock.now())
        .await?
        .ok_or("the payment worker could not take its lease")?;
    let paid = transfer(expected.to_string().as_str(), clock.now())?;
    record_evidence(&repository, &lease, &paid, clock.now()).await?;

    let verifier = VerificationService::new(
        Arc::clone(&repository),
        Arc::new(ChainDouble(Mutex::new(paid))),
        clock.clone(),
        source(VERIFIER_SOURCE, "verifier"),
        "verifier-test",
        "parser-test",
    );
    clock.advance(Duration::seconds(5));
    let canonicalized = verifier.verify_pending(&lease, 50).await?;
    assert_eq!(canonicalized.verified, 1);

    let settlement = SettlementService::new(Arc::clone(&repository), clock.clone());
    let report = settlement.settle_pending(&lease, 50).await?;

    assert_eq!(report.settled, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM payment_allocations").await?,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM chain_transfer_intent_claims").await?,
        1
    );
    let intent_status: String =
        sqlx::query_scalar("SELECT status FROM payment_intents WHERE id = $1")
            .bind(intent.intent.id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(intent_status, "paid");
    let fulfillment: String =
        sqlx::query_scalar("SELECT status FROM payment_fulfillments WHERE payment_intent_id = $1")
            .bind(intent.intent.id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(fulfillment, "claimed");
    let (outcome, groups): (String, i32) =
        sqlx::query_as("SELECT outcome, distinct_groups FROM payment_settlement_decisions LIMIT 1")
            .fetch_one(&pool)
            .await?;
    assert_eq!(outcome, "settled");
    // Two observers plus the verifier's own re-read: three independent groups.
    assert_eq!(groups, 3);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM domain_events WHERE event_type = 'payment_intent.paid'"
        )
        .await?,
        1
    );

    // Running again changes nothing: the transfer is no longer pending.
    clock.advance(Duration::seconds(5));
    let repeat = settlement.settle_pending(&lease, 50).await?;
    assert_eq!(repeat.examined, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM payment_allocations").await?,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM domain_events WHERE event_type = 'payment_intent.paid'"
        )
        .await?,
        1
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn one_transfer_can_never_pay_two_obligations() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(OffsetDateTime::now_utc());
    let (first, second) = two_intents(&repository, &clock).await?;

    let lease = repository
        .acquire_component_lease("payments:tron", "pod-payments:boot-1", 300, clock.now())
        .await?
        .ok_or("the payment worker could not take its lease")?;
    let paid = transfer(first.expected.to_string().as_str(), clock.now())?;
    record_evidence(&repository, &lease, &paid, clock.now()).await?;
    let verifier = VerificationService::new(
        Arc::clone(&repository),
        Arc::new(ChainDouble(Mutex::new(paid))),
        clock.clone(),
        source(VERIFIER_SOURCE, "verifier"),
        "verifier-test",
        "parser-test",
    );
    clock.advance(Duration::seconds(5));
    verifier.verify_pending(&lease, 50).await?;

    let settlement = SettlementService::new(Arc::clone(&repository), clock.clone());
    settlement.settle_pending(&lease, 50).await?;

    // A second run is forced to look at the same transfer again, as a crashed
    // worker between claim and bookkeeping would.
    sqlx::query("UPDATE chain_transfer_processing SET processing_state = 'pending'")
        .execute(&pool)
        .await?;
    let transfer_id: Uuid = sqlx::query_scalar("SELECT id FROM chain_transfers LIMIT 1")
        .fetch_one(&pool)
        .await?;
    let command = gateway_application::SettlementCommand {
        transfer_id,
        attempt_id: second.attempt_id,
        payment_intent_id: second.intent_id,
        merchant_id: MERCHANT,
        fiat_amount_minor: 40_000,
        match_strategy: gateway_domain::MatchStrategy::ExactAmount,
        outcome: gateway_domain::SettlementOutcome::Settle {
            allocate_raw: first.expected,
        },
        policy_version: "settlement-v1".to_owned(),
        independent_groups: 2,
        had_own_node: false,
        finality_state: gateway_domain::TransferState::Finalized,
        risk: gateway_domain::RiskDecision::Allow,
        risk_evaluation_id: None,
        attestation_ids: Vec::new(),
    };

    let record = repository.settle(&lease, &command).await?;

    assert_eq!(record, gateway_application::SettlementRecord::ForeignClaim);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM payment_allocations").await?,
        1
    );
    let second_status: String =
        sqlx::query_scalar("SELECT status FROM payment_intents WHERE id = $1")
            .bind(second.intent_id)
            .fetch_one(&pool)
            .await?;
    assert_ne!(second_status, "paid");
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn an_overpayment_keeps_the_remainder_for_a_person() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    seed(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let clock = TestClock::at(OffsetDateTime::now_utc());
    let (first, _second) = two_intents(&repository, &clock).await?;

    let lease = repository
        .acquire_component_lease("payments:tron", "pod-payments:boot-1", 300, clock.now())
        .await?
        .ok_or("the payment worker could not take its lease")?;
    let overpaid_amount = first.expected.checked_add_u32(500)?;
    let paid = transfer(overpaid_amount.to_string().as_str(), clock.now())?;
    record_evidence(&repository, &lease, &paid, clock.now()).await?;
    let verifier = VerificationService::new(
        Arc::clone(&repository),
        Arc::new(ChainDouble(Mutex::new(paid))),
        clock.clone(),
        source(VERIFIER_SOURCE, "verifier"),
        "verifier-test",
        "parser-test",
    );
    clock.advance(Duration::seconds(5));
    verifier.verify_pending(&lease, 50).await?;

    let settlement = SettlementService::new(Arc::clone(&repository), clock.clone());
    let report = settlement.settle_pending(&lease, 50).await?;

    // The amount does not match any reservation exactly, so it is money nobody
    // can attribute: recorded and queued, never absorbed.
    assert_eq!(report.unmatched, 1);
    let state: String =
        sqlx::query_scalar("SELECT processing_state FROM chain_transfer_processing LIMIT 1")
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, "unmatched");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM payment_events WHERE event_type = 'UNMATCHED_INBOUND'"
        )
        .await?,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM payment_allocations").await?,
        0
    );
    Ok(())
}

struct SeededIntent {
    intent_id: Uuid,
    attempt_id: Uuid,
    expected: RawAmount,
}

async fn two_intents(
    repository: &Arc<PostgresRepository>,
    clock: &TestClock,
) -> Result<(SeededIntent, SeededIntent), Box<dyn Error>> {
    let intents = PaymentIntentService::new(Arc::clone(repository), clock.clone());
    let quotes = gateway_application::QuoteService::new(Arc::clone(repository), clock.clone());
    let mut seeded = Vec::new();
    for index in 0..2 {
        let intent = intents
            .create(
                MERCHANT,
                ACTOR_KEY,
                &format!("settlement_intent_0000000{index}"),
                CreatePaymentIntent {
                    amount_minor: "40000".to_owned(),
                    currency: "USD".to_owned(),
                    reference: format!("order-settlement-{index}"),
                    description: None,
                    metadata: json!({}),
                },
            )
            .await?;
        let quote = quotes
            .issue(
                MERCHANT,
                ACTOR_KEY,
                &format!("settlement_quote_00000000{index}"),
                IssueQuote {
                    payment_intent_id: intent.intent.id,
                    asset_id: ASSET_ID,
                },
            )
            .await?;
        seeded.push(SeededIntent {
            intent_id: intent.intent.id,
            attempt_id: quote.quote.attempt_id,
            expected: quote.quote.amount_raw,
        });
    }
    let mut drained = seeded.into_iter();
    let first = drained.next().ok_or("the first intent was not created")?;
    let second = drained.next().ok_or("the second intent was not created")?;
    Ok((first, second))
}

async fn record_evidence(
    repository: &Arc<PostgresRepository>,
    lease: &ComponentLease,
    paid: &ObservedTransfer,
    observed_at: OffsetDateTime,
) -> TestResult {
    for (source_id, group) in [(SOURCE_A, "group-a"), (SOURCE_B, "group-b")] {
        let source = source(source_id, group);
        let semantic_hash = paid.semantic_hash(source.id, ObservationKind::CursorScan);
        repository
            .record_observations(
                &source,
                lease,
                &[ResolvedObservation {
                    transfer: paid.clone(),
                    kind: ObservationKind::CursorScan,
                    collector_address_id: COLLECTOR_ID,
                    asset_id: Some(ASSET_ID),
                    semantic_hash,
                    observer_version: "observer-test".to_owned(),
                    parser_version: "parser-test".to_owned(),
                    fence_token: lease.fence_token,
                    observed_at,
                }],
                None,
            )
            .await?;
    }
    Ok(())
}

fn source(id: Uuid, group: &str) -> ChainSource {
    ChainSource {
        id,
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        source_key: format!("source-{id}"),
        provider_group: group.to_owned(),
        kind: gateway_application::SourceKind::IndexedApi,
        db_principal: "gateway".to_owned(),
        state: gateway_application::SourceState::Active,
    }
}

fn transfer(amount: &str, block_time: OffsetDateTime) -> Result<ObservedTransfer, Box<dyn Error>> {
    Ok(ObservedTransfer {
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        tx_hash: TxHash::new("settlementtx01")?,
        event_index: 0,
        block_number: Some(500),
        block_hash: Some("block-500".to_owned()),
        parent_hash: Some("block-499".to_owned()),
        block_time: Some(block_time),
        token_key: AddressKey::new([7_u8; 20])?,
        token_display: "USDT".to_owned(),
        from_address: AddressKey::new([9_u8; 21])?,
        from_address_text: "TFrom".to_owned(),
        to_address: AddressKey::new([3_u8; 21])?,
        to_address_text: "TCollector".to_owned(),
        amount_raw: RawAmount::from_str(amount)?,
        decimals: 6,
        memo: None,
        execution_status: ExecutionStatus::Success,
        source_finality: SourceFinality::Finalized,
        source_head: Some(600),
        evidence_sha256: "c".repeat(64),
        evidence_uri: None,
    })
}

#[allow(clippy::too_many_lines)]
async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query(
        "TRUNCATE chain_sources, chain_assets, merchants, component_leases,
                  chain_finality_policies, payment_settlement_policies CASCADE",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO merchants (id, external_id, display_name, status) \
         VALUES ($1, 'settlement-merchant', 'Settlement Merchant', 'active')",
    )
    .bind(MERCHANT)
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO chain_assets (
            id, chain, network, chain_environment, contract_address_key,
            display_symbol, decimals, status, pinned_sha256, approved_by
        ) VALUES (
            $1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active',
            encode(sha256($2), 'hex'), 'test'
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
        ) VALUES ($1, $2, $3, 'TCollector', 'active', $4, encode(sha256($3), 'hex'), 'test')
        ",
    )
    .bind(COLLECTOR_ID)
    .bind(ASSET_ID)
    .bind([3_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(pool)
    .await?;
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        r"
        INSERT INTO price_snapshots (
            id, asset_id, fiat_currency, rate_numerator, rate_denominator, sources, observed_at
        ) VALUES ($1, $2, 'USD', 1, 10, $3, $4)
        ",
    )
    .bind(PRICE_SNAPSHOT)
    .bind(ASSET_ID)
    .bind(json!([
        {"provider_group": "source-a", "observed_at": now},
        {"provider_group": "source-b", "observed_at": now}
    ]))
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO quote_policies (
            id, asset_id, fiat_currency, version, status, quote_ttl_seconds,
            late_payment_window_seconds, amount_slot_count, max_price_age_seconds,
            max_policy_age_seconds, max_rail_health_age_seconds, observed_at
        ) VALUES ($1, $2, 'USD', 'quote-v1', 'active', 900, 2592000, 100, 3600, 3600, 3600, $3)
        ",
    )
    .bind(QUOTE_POLICY)
    .bind(ASSET_ID)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO rail_health_snapshots (id, asset_id, health, observed_at) \
         VALUES ($1, $2, 'healthy', $3)",
    )
    .bind(RAIL_HEALTH)
    .bind(ASSET_ID)
    .bind(now)
    .execute(pool)
    .await?;
    for (id, group) in [
        (SOURCE_A, "group-a"),
        (SOURCE_B, "group-b"),
        (VERIFIER_SOURCE, "verifier"),
    ] {
        sqlx::query(
            r"
            INSERT INTO chain_sources (
                id, chain, network, chain_environment, source_key, provider_group,
                kind, db_principal, requires_dedicated_principal, state, valid_from
            ) VALUES ($1, 'tron', 'nile', 'testnet', $2, $3, 'indexed_api',
                      session_user, FALSE, 'active', $4)
            ",
        )
        .bind(id)
        .bind(format!("source-{id}"))
        .bind(group)
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        r"
        INSERT INTO chain_finality_policies (
            id, chain, network, chain_environment, version, status, min_confirmations,
            required_source_finality, min_independent_groups, max_evidence_age_seconds, observed_at
        ) VALUES ($1, 'tron', 'nile', 'testnet', 'finality-v1', 'active', 19,
                  'finalized', 2, 3600, $2)
        ",
    )
    .bind(FINALITY_POLICY)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO payment_settlement_policies (
            id, fiat_currency, version, status, approved_by, observed_at
        ) VALUES ($1, 'USD', 'settlement-v1', 'active', 'test', $2)
        ",
    )
    .bind(SETTLEMENT_POLICY)
    .bind(now)
    .execute(pool)
    .await?;
    for (max_fiat_minor, groups, own_node, risk_allow, auto) in [
        (50_000_i64, 2_i32, false, false, true),
        (600_000_i64, 2_i32, true, true, false),
    ] {
        sqlx::query(
            r"
            INSERT INTO payment_settlement_policy_tiers (
                policy_id, max_fiat_minor, min_independent_groups, require_own_node,
                require_risk_allow, auto_settle
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ",
        )
        .bind(SETTLEMENT_POLICY)
        .bind(max_fiat_minor)
        .bind(groups)
        .bind(own_node)
        .bind(risk_allow)
        .bind(auto)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn count(pool: &PgPool, query: &str) -> Result<i64, Box<dyn Error>> {
    Ok(sqlx::query_scalar(query).fetch_one(pool).await?)
}
