//! The privileges, proven from the other side.
//!
//! Every role SQL file is applied to the migrated schema (twice, because an
//! operator will), throwaway login roles are created in each group, and each
//! one is connected as and made to attempt the writes it must not be able to
//! make. A refusal is asserted by SQLSTATE: `42501` is `PostgreSQL` saying
//! "insufficient privilege", which is also how a row level security violation
//! is reported. An error of any other kind is a test defect, not a proof.

use std::{error::Error, fs};

use sqlx::{PgPool, postgres::PgPoolOptions};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    migrate,
    roles::{GRANTS_SQL, ROLES_SQL},
    test_support::{DATABASE, connect, database_url_as},
};

type TestResult = Result<(), Box<dyn Error>>;

const INSUFFICIENT_PRIVILEGE: &str = "42501";
const PASSWORD: &str = "roles_scenario_password";
const ASSET_ID: Uuid = Uuid::from_u128(11_101);
const COLLECTOR_ID: Uuid = Uuid::from_u128(11_201);
const SOURCE_A: Uuid = Uuid::from_u128(11_401);
const SOURCE_B: Uuid = Uuid::from_u128(11_402);

/// Login roles created for the scenario: (name, group). The observer role is
/// named as `SOURCE_A`'s `db_principal`; `SOURCE_B` belongs to a principal that
/// never logs in, which is what "another source" means to row level security.
const LOGINS: [(&str, &str); 6] = [
    ("roles_scenario_api", "gateway_api"),
    ("roles_scenario_observer_a", "gateway_observer"),
    ("roles_scenario_verifier", "gateway_verifier"),
    ("roles_scenario_payment", "gateway_payment"),
    ("roles_scenario_provisioner", "gateway_provisioner"),
    ("roles_scenario_retention", "gateway_retention"),
];

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn migrations_and_role_sql_apply_twice() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    migrate(&pool).await?;
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    let shipped = fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../db/migrations"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sql"))
        .count();
    assert_eq!(usize::try_from(applied)?, shipped);

    sqlx::raw_sql(ROLES_SQL).execute(&pool).await?;
    sqlx::raw_sql(GRANTS_SQL).execute(&pool).await?;
    sqlx::raw_sql(ROLES_SQL).execute(&pool).await?;
    sqlx::raw_sql(GRANTS_SQL).execute(&pool).await?;

    let owners: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT tableowner FROM pg_tables WHERE schemaname = 'public'")
            .fetch_all(&pool)
            .await?;
    assert_eq!(owners, vec!["gateway_migrator".to_owned()]);
    Ok(())
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn each_role_is_refused_the_writes_that_are_not_its_own() -> TestResult {
    let _fixture = DATABASE.lock().await;
    let pool = connect().await?;
    sqlx::raw_sql(ROLES_SQL).execute(&pool).await?;
    sqlx::raw_sql(GRANTS_SQL).execute(&pool).await?;
    seed(&pool).await?;
    create_logins(&pool).await?;

    // The roles are dropped whether or not the assertions hold, so a failed
    // run does not leave logins behind for the next one to trip over.
    let mut outcome = prove(&pool).await;
    if outcome.is_ok() {
        outcome = prove_provisioner(&pool).await;
    }
    if outcome.is_ok() {
        outcome = prove_dual_control_honor(&pool).await;
    }
    if outcome.is_ok() {
        outcome = prove_retention(&pool).await;
    }
    drop_logins(&pool).await?;
    outcome
}

#[allow(clippy::too_many_lines)]
async fn prove(pool: &PgPool) -> TestResult {
    let api = connect_as("roles_scenario_api").await?;
    let observer = connect_as("roles_scenario_observer_a").await?;
    let verifier = connect_as("roles_scenario_verifier").await?;
    let payment = connect_as("roles_scenario_payment").await?;

    // Observer: it may say what it saw, under its own name, for its own source.
    let recorded: String = sqlx::query_scalar(&observation_insert(None))
        .bind(SOURCE_A)
        .fetch_one(&observer)
        .await?;
    assert_eq!(recorded, "roles_scenario_observer_a");
    sqlx::query(
        "INSERT INTO chain_cursors (source_id, observation_kind, collector_address_id, \
                                    cursor_kind, cursor_value, fence_token, updated_at) \
         VALUES ($1, 'fast_detect', $2, 'block', '10', 1, now())",
    )
    .bind(SOURCE_A)
    .bind(COLLECTOR_ID)
    .execute(&observer)
    .await?;

    // ... and nothing more. A forged principal, another source's cursor, and
    // every table on the money path are refused.
    denied(
        sqlx::query(&observation_insert(Some("someone_else")))
            .bind(SOURCE_A)
            .execute(&observer)
            .await,
        "observer forging a source principal",
    )?;
    denied(
        sqlx::query(
            "INSERT INTO chain_cursors (source_id, observation_kind, collector_address_id, \
                                        cursor_kind, cursor_value, fence_token, updated_at) \
             VALUES ($1, 'fast_detect', $2, 'block', '10', 1, now())",
        )
        .bind(SOURCE_B)
        .bind(COLLECTOR_ID)
        .execute(&observer)
        .await,
        "observer creating another source's cursor",
    )?;
    // An UPDATE that the row policy hides touches nothing rather than failing:
    // the proof is that the other source's cursor did not move.
    let moved = sqlx::query("UPDATE chain_cursors SET cursor_value = '999' WHERE source_id = $1")
        .bind(SOURCE_B)
        .execute(&observer)
        .await?;
    assert_eq!(moved.rows_affected(), 0);
    let untouched: String =
        sqlx::query_scalar("SELECT cursor_value FROM chain_cursors WHERE source_id = $1")
            .bind(SOURCE_B)
            .fetch_one(pool)
            .await?;
    assert_eq!(untouched, "1");
    denied(
        sqlx::query(&transfer_insert()).execute(&observer).await,
        "observer writing a canonical transfer",
    )?;
    denied(
        sqlx::query("INSERT INTO payment_allocations DEFAULT VALUES")
            .execute(&observer)
            .await,
        "observer writing an allocation",
    )?;
    denied(
        sqlx::query("INSERT INTO domain_events DEFAULT VALUES")
            .execute(&observer)
            .await,
        "observer writing to the outbox",
    )?;
    denied(
        sqlx::query("SELECT count(*) FROM merchant_api_keys")
            .execute(&observer)
            .await,
        "observer reading merchant credentials",
    )?;

    // Verifier: writes facts, never money.
    let fact_id: Uuid = sqlx::query_scalar(&transfer_insert())
        .fetch_one(&verifier)
        .await?;
    assert!(!fact_id.is_nil());
    denied(
        sqlx::query("INSERT INTO payment_allocations DEFAULT VALUES")
            .execute(&verifier)
            .await,
        "verifier writing an allocation",
    )?;
    denied(
        sqlx::query("INSERT INTO domain_events DEFAULT VALUES")
            .execute(&verifier)
            .await,
        "verifier writing to the outbox",
    )?;

    // API: takes orders and reads evidence, never writes a chain fact.
    let merchants: i64 = sqlx::query_scalar("SELECT count(*) FROM merchants")
        .fetch_one(&api)
        .await?;
    assert_eq!(merchants, 0);
    denied(
        sqlx::query(&transfer_insert()).execute(&api).await,
        "api writing a canonical transfer",
    )?;
    denied(
        sqlx::query("INSERT INTO chain_observations DEFAULT VALUES")
            .execute(&api)
            .await,
        "api writing an observation",
    )?;
    denied(
        sqlx::query("DELETE FROM amount_leases").execute(&api).await,
        "api deleting a lease",
    )?;

    // Payment: moves money against facts it can only read.
    let facts: i64 = sqlx::query_scalar("SELECT count(*) FROM chain_transfers")
        .fetch_one(&payment)
        .await?;
    assert_eq!(facts, 1);
    denied(
        sqlx::query(&observation_insert(None))
            .bind(SOURCE_A)
            .execute(&payment)
            .await,
        "payment writing an observation",
    )?;
    denied(
        sqlx::query(&transfer_insert()).execute(&payment).await,
        "payment writing a canonical transfer",
    )?;
    denied(
        sqlx::query("UPDATE rail_stops SET cleared_at = now()")
            .execute(&payment)
            .await,
        "payment clearing a rail stop",
    )?;
    Ok(())
}

/// Asserts that `PostgreSQL` refused the statement for lack of privilege.
fn denied<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, what: &str) -> TestResult {
    match result {
        Err(sqlx::Error::Database(error)) => {
            let code = error.code().map(std::borrow::Cow::into_owned);
            assert_eq!(
                code.as_deref(),
                Some(INSUFFICIENT_PRIVILEGE),
                "{what}: refused, but not for privilege: {error}"
            );
            Ok(())
        }
        Err(other) => {
            Err(format!("{what}: failed for a reason other than privilege: {other}").into())
        }
        Ok(value) => Err(format!("{what}: was allowed: {value:?}").into()),
    }
}

async fn connect_as(role: &str) -> Result<PgPool, Box<dyn Error>> {
    Ok(PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url_as(role, PASSWORD)?)
        .await?)
}

fn observation_insert(forged_principal: Option<&str>) -> String {
    let principal = forged_principal.map_or(String::new(), |name| format!("'{name}', "));
    let principal_column = if forged_principal.is_some() {
        "source_principal, "
    } else {
        ""
    };
    format!(
        "INSERT INTO chain_observations (\
            id, source_id, {principal_column}chain, network, chain_environment, \
            observation_kind, tx_hash, event_index, token_key, token_display, \
            from_address_key, from_address_text, to_address_key, to_address_text, \
            amount_raw, decimals, execution_status, source_finality, evidence_sha256, \
            observer_version, parser_version, fence_token, semantic_hash, observed_at\
         ) VALUES (\
            gen_random_uuid(), $1, {principal}'tron', 'nile', 'testnet', 'cursor_scan', \
            'tx-roles', 0, '\\x07', 'USDT', '\\x09', 'TFrom', '\\x03', 'TCollector', \
            1000, 6, 'success', 'seen', repeat('a', 64), 'test', 'test', 1, \
            sha256(gen_random_uuid()::TEXT::BYTEA), now()\
         ) RETURNING source_principal"
    )
}

fn transfer_insert() -> String {
    format!(
        "INSERT INTO chain_transfers (\
            id, asset_id, collector_address_id, chain, network, chain_environment, \
            tx_hash, event_index, block_number, block_hash, block_time, token_key, \
            from_address_key, from_address_text, to_address_key, to_address_text, \
            amount_raw, decimals, canonicalization_policy, verifier_version, canonicalized_at\
         ) VALUES (\
            gen_random_uuid(), '{ASSET_ID}', '{COLLECTOR_ID}', 'tron', 'nile', 'testnet', \
            'tx-roles-' || gen_random_uuid()::TEXT, 0, 1, 'block-1', now(), '\\x07', \
            '\\x09', 'TFrom', '\\x03', 'TCollector', 1000, 6, 'test', 'test', now()\
         ) RETURNING id"
    )
}

async fn seed(pool: &PgPool) -> TestResult {
    sqlx::query("TRUNCATE chain_sources, chain_assets, merchants CASCADE")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO chain_assets (id, chain, network, chain_environment, contract_address_key, \
            display_symbol, decimals, status, pinned_sha256, approved_by) \
         VALUES ($1, 'tron', 'nile', 'testnet', $2, 'USDT', 6, 'active', \
            encode(sha256($2), 'hex'), 'test')",
    )
    .bind(ASSET_ID)
    .bind([7_u8; 20].as_slice())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO collector_addresses (id, asset_id, address_key, address_text, state, \
            valid_from, pinned_sha256, approved_by) \
         VALUES ($1, $2, $3, 'TCollector', 'active', $4, encode(sha256($3), 'hex'), 'test')",
    )
    .bind(COLLECTOR_ID)
    .bind(ASSET_ID)
    .bind([3_u8; 21].as_slice())
    .bind(OffsetDateTime::UNIX_EPOCH)
    .execute(pool)
    .await?;
    for (id, key, principal) in [
        (SOURCE_A, "roles-source-a", "roles_scenario_observer_a"),
        (SOURCE_B, "roles-source-b", "roles_scenario_observer_b"),
    ] {
        sqlx::query(
            "INSERT INTO chain_sources (id, chain, network, chain_environment, source_key, \
                provider_group, kind, db_principal, requires_dedicated_principal, state, valid_from) \
             VALUES ($1, 'tron', 'nile', 'testnet', $2, $2, 'indexed_api', $3, TRUE, 'active', $4)",
        )
        .bind(id)
        .bind(key)
        .bind(principal)
        .bind(OffsetDateTime::UNIX_EPOCH)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO chain_cursors (source_id, observation_kind, collector_address_id, \
                                    cursor_kind, cursor_value, fence_token, updated_at) \
         VALUES ($1, 'cursor_scan', $2, 'block', '1', 1, now())",
    )
    .bind(SOURCE_B)
    .bind(COLLECTOR_ID)
    .execute(pool)
    .await?;
    Ok(())
}

async fn create_logins(pool: &PgPool) -> TestResult {
    drop_logins(pool).await?;
    for (name, group) in LOGINS {
        sqlx::query(&format!(
            "CREATE ROLE {name} LOGIN PASSWORD '{PASSWORD}' IN ROLE {group}"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn drop_logins(pool: &PgPool) -> TestResult {
    for (name, _) in LOGINS {
        sqlx::query(&format!(
            "DO $$ BEGIN \
                IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{name}') THEN \
                    EXECUTE 'DROP OWNED BY {name}'; \
                    EXECUTE 'DROP ROLE {name}'; \
                END IF; \
             END $$"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
}

struct FixedRandom;

impl gateway_application::RandomBytes for FixedRandom {
    fn fill(&self, buffer: &mut [u8]) -> Result<(), gateway_application::ProvisioningError> {
        buffer.fill(0x5a);
        Ok(())
    }
}

/// The provisioner onboards through the service, under its own login, and is
/// refused the money path and the columns it must not rewrite.
#[allow(clippy::too_many_lines)]
async fn prove_provisioner(owner: &PgPool) -> TestResult {
    let provisioner = connect_as("roles_scenario_provisioner").await?;
    let service = gateway_application::ProvisioningService::new(
        std::sync::Arc::new(crate::PostgresRepository::new(provisioner.clone())),
        gateway_application::SystemClock,
        FixedRandom,
        Some(vec![3_u8; 32]),
    );
    let merchant = service
        .create_merchant(
            "roles-scenario",
            "roles-scenario-merchant",
            "Roles Scenario",
            gateway_application::CollectorPolicy::Own,
        )
        .await?;
    let key = service
        .issue_api_key("roles-scenario", merchant.id, "server")
        .await?;
    service
        .revoke_api_key("roles-scenario", key.key_id, "scenario")
        .await?;
    let endpoint = service
        .add_webhook_endpoint(
            "roles-scenario",
            merchant.id,
            "https://roles.example/hook",
            None,
        )
        .await?;
    service
        .rotate_webhook_secret(
            "roles-scenario",
            endpoint.endpoint_id,
            time::Duration::hours(2),
            "scenario",
        )
        .await?;
    let event_id = service
        .send_test_event("roles-scenario", endpoint.endpoint_id)
        .await?;

    // Redelivery and the read-only lists run under the same login.
    sqlx::query("UPDATE domain_events SET dead_lettered_at = now(), attempts = 2 WHERE id = $1")
        .bind(event_id)
        .execute(owner)
        .await?;
    let request = gateway_application::WebhookRedelivery {
        event_id,
        endpoint_id: Some(endpoint.endpoint_id),
        reason: "roles scenario".to_owned(),
    };
    let redelivered = service
        .redeliver_webhook("roles-scenario", "roles-redeliver-00001", &request)
        .await?;
    assert_eq!(redelivered.previous_state, "dead_lettered");
    let page = gateway_application::ListRequest::new(Some(10), None)?;
    assert_eq!(service.list_merchants(page).await?.items.len(), 1);
    assert_eq!(
        service.list_api_keys(merchant.id, page).await?.items.len(),
        1
    );
    assert_eq!(
        service
            .list_webhook_endpoints(merchant.id, page)
            .await?
            .items
            .len(),
        1
    );
    service.list_collectors(None, page).await?;

    // The API redelivers under its own login too.
    sqlx::query("UPDATE domain_events SET delivered_at = now(), attempts = 3 WHERE id = $1")
        .bind(event_id)
        .execute(owner)
        .await?;
    let api = connect_as("roles_scenario_api").await?;
    let api_repository = crate::PostgresRepository::new(api.clone());
    let hash = gateway_application::validate_redelivery("roles-redeliver-api-01", &request)?;
    gateway_application::RedeliveryRepository::redeliver_webhook_event(
        &api_repository,
        &gateway_application::RedeliveryActor::Admin {
            name: "roles-scenario".to_owned(),
        },
        "roles-redeliver-api-01",
        &hash,
        &request,
        time::OffsetDateTime::now_utc(),
    )
    .await?;
    denied(
        sqlx::query("UPDATE domain_events SET payload = '{}'::jsonb")
            .execute(&api)
            .await,
        "api rewriting an event payload",
    )?;

    denied(
        sqlx::query("INSERT INTO payment_allocations DEFAULT VALUES")
            .execute(&provisioner)
            .await,
        "provisioner writing an allocation",
    )?;
    denied(
        sqlx::query("UPDATE merchant_api_keys SET secret_hash = secret_hash")
            .execute(&provisioner)
            .await,
        "provisioner rewriting a key hash",
    )?;
    denied(
        sqlx::query("UPDATE merchants SET collector_policy = 'shared'")
            .execute(&provisioner)
            .await,
        "provisioner moving a merchant between collector policies",
    )?;
    denied(
        sqlx::query("SELECT count(*) FROM payment_intents")
            .execute(&provisioner)
            .await,
        "provisioner reading payments",
    )?;
    denied(
        sqlx::query("SELECT secret_hash FROM merchant_api_keys")
            .execute(&provisioner)
            .await,
        "provisioner reading a key hash",
    )?;
    denied(
        sqlx::query("SELECT payload FROM domain_events")
            .execute(&provisioner)
            .await,
        "provisioner reading an event payload",
    )?;
    denied(
        sqlx::query("UPDATE domain_events SET event_type = 'payment_intent.paid'")
            .execute(&provisioner)
            .await,
        "provisioner rewriting an event",
    )?;
    Ok(())
}

/// The two-operator honor runs end to end under the API's own login: the
/// proposal, the approval and the money it writes need no privilege the API
/// group does not hold.
#[allow(clippy::too_many_lines)]
async fn prove_dual_control_honor(owner: &PgPool) -> TestResult {
    let merchant = Uuid::now_v7();
    let (price, policy, health, intent, quote, attempt, transfer) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query("INSERT INTO merchants(id,external_id,display_name,status,collector_policy) VALUES($1,'roles-honor','Roles Honor','active','shared')")
        .bind(merchant).execute(owner).await?;
    sqlx::query("INSERT INTO price_snapshots(id,asset_id,fiat_currency,rate_numerator,rate_denominator,sources,observed_at) VALUES($1,$2,'USD',1,1,'[{},{}]'::jsonb,now())")
        .bind(price).bind(ASSET_ID).execute(owner).await?;
    sqlx::query("INSERT INTO quote_policies(id,asset_id,fiat_currency,version,status,quote_ttl_seconds,late_payment_window_seconds,amount_slot_count,max_price_age_seconds,max_policy_age_seconds,max_rail_health_age_seconds,observed_at) VALUES($1,$2,'USD','roles-v1','active',900,3600,10000,300,300,300,now())")
        .bind(policy).bind(ASSET_ID).execute(owner).await?;
    sqlx::query("INSERT INTO rail_health_snapshots(id,asset_id,health,observed_at) VALUES($1,$2,'healthy',now())")
        .bind(health).bind(ASSET_ID).execute(owner).await?;
    sqlx::query("INSERT INTO payment_intents(id,merchant_id,amount_minor,currency,status,reference,created_at,updated_at) VALUES($1,$2,100,'USD','awaiting_payment','roles-honor',now(),now())")
        .bind(intent).bind(merchant).execute(owner).await?;
    sqlx::query("INSERT INTO payment_quotes(id,merchant_id,payment_intent_id,asset_id,collector_address_id,fiat_currency,fiat_amount_minor,base_amount_raw,amount_raw,rate_numerator,rate_denominator,price_sources,price_observed_at,policy_version,rail_health_observed_at,created_at,expires_at,late_payment_until,price_snapshot_id,quote_policy_id,rail_health_snapshot_id) VALUES($1,$2,$3,$4,$5,'USD',100,5000,5000,1,1,'[{}]'::jsonb,now(),'roles-v1',now(),now(),now()+interval '1 hour',now()+interval '2 hours',$6,$7,$8)")
        .bind(quote).bind(merchant).bind(intent).bind(ASSET_ID).bind(COLLECTOR_ID).bind(price).bind(policy).bind(health).execute(owner).await?;
    sqlx::query("INSERT INTO payment_attempts(id,merchant_id,payment_intent_id,quote_id,collector_address_id,expected_amount_raw,status,quote_expires_at,late_payment_until,created_at,updated_at) VALUES($1,$2,$3,$4,$5,5000,'awaiting_payment',now()+interval '1 hour',now()+interval '2 hours',now(),now())")
        .bind(attempt).bind(merchant).bind(intent).bind(quote).bind(COLLECTOR_ID).execute(owner).await?;
    sqlx::query("INSERT INTO chain_transfers(id,asset_id,collector_address_id,chain,network,chain_environment,tx_hash,event_index,block_number,block_hash,block_time,token_key,from_address_key,from_address_text,to_address_key,to_address_text,amount_raw,decimals,canonicalization_policy,verifier_version,canonicalized_at) VALUES($1,$2,$3,'tron','nile','testnet','roles-honor-tx',0,5,'roles-block',now(),'\x07','\x09','TFrom','\x03','TCollector',5000,6,'test','test',now())")
        .bind(transfer).bind(ASSET_ID).bind(COLLECTOR_ID).execute(owner).await?;
    sqlx::query("INSERT INTO chain_transfer_state_current(transfer_id,state,state_version,updated_at) VALUES($1,'finalized',1,now())")
        .bind(transfer).execute(owner).await?;
    sqlx::query("INSERT INTO chain_transfer_processing(transfer_id,allocated_raw,processing_state,updated_at) VALUES($1,0,'unmatched',now())")
        .bind(transfer).execute(owner).await?;
    let mut keys = Vec::new();
    for label in ["roles-proposer", "roles-approver"] {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO operator_api_keys(id,key_prefix,secret_hash,label,scopes) VALUES($1,'cg_roles',$2,$3,$4)")
            .bind(id)
            .bind(id.as_bytes().repeat(2))
            .bind(label)
            .bind(vec!["read", "admin"])
            .execute(owner)
            .await?;
        keys.push(gateway_application::OperatorCredential {
            key_id: id,
            label: label.to_owned(),
            scopes: vec![
                gateway_application::OperatorScope::Read,
                gateway_application::OperatorScope::Admin,
            ],
        });
    }

    let api = connect_as("roles_scenario_api").await?;
    let service = gateway_application::OperationsService::new(
        std::sync::Arc::new(crate::PostgresRepository::new(api)),
        gateway_application::SystemClock,
    );
    let honor = gateway_application::ManualResolution {
        action: gateway_domain::ManualResolutionAction::Honor,
        transfer_id: transfer,
        payment_intent_id: Some(intent),
        attempt_id: Some(attempt),
        allocate_raw: Some("5000".parse()?),
        remainder_raw: None,
        disposition: None,
        external_reference: None,
        reason: "roles scenario".to_owned(),
    };
    let proposal = service
        .propose_honor(&keys[0], "roles-honor-proposal-1", &honor)
        .await?;
    let paid = service
        .approve_honor(
            &keys[1],
            proposal.id,
            "roles-honor-approval-1",
            "second pair of eyes",
        )
        .await?;
    assert_eq!(
        paid.allocated_raw.map(|raw| raw.to_string()).as_deref(),
        Some("5000")
    );
    let status: String = sqlx::query_scalar("SELECT status FROM payment_intents WHERE id = $1")
        .bind(intent)
        .fetch_one(owner)
        .await?;
    assert_eq!(status, "paid");
    Ok(())
}

/// Retention runs under its own login, deletes nothing it is not allowed to,
/// and cannot delete an observation that is not a reading of a final
/// transfer even with a statement of its own.
async fn prove_retention(owner: &PgPool) -> TestResult {
    let retention = connect_as("roles_scenario_retention").await?;
    let service = gateway_application::RetentionService::new(
        std::sync::Arc::new(crate::PostgresRepository::new(retention.clone())),
        gateway_application::SystemClock,
        gateway_application::RetentionPolicy::from_days(Some(30), 3, Some(30), Some(30))?,
    )?;
    service.purge_batch(100).await?;

    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM chain_observations")
        .fetch_one(owner)
        .await?;
    let hidden = sqlx::query("DELETE FROM chain_observations")
        .execute(&retention)
        .await?;
    assert_eq!(hidden.rows_affected(), 0);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM chain_observations")
        .fetch_one(owner)
        .await?;
    assert_eq!(before, after);

    for (statement, what) in [
        (
            "DELETE FROM payment_allocations",
            "retention deleting an allocation",
        ),
        (
            "DELETE FROM audit_events",
            "retention deleting the audit trail",
        ),
        (
            "DELETE FROM domain_events",
            "retention deleting an outbox event",
        ),
        (
            "DELETE FROM chain_transfers",
            "retention deleting a canonical transfer",
        ),
        (
            "UPDATE chain_observations SET amount_raw = 1",
            "retention rewriting an observation",
        ),
        (
            "SELECT count(*) FROM merchant_api_keys",
            "retention reading credentials",
        ),
        (
            "SELECT payload FROM domain_events",
            "retention reading an event payload",
        ),
    ] {
        denied(sqlx::query(statement).execute(&retention).await, what)?;
    }
    Ok(())
}
