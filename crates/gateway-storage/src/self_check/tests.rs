use std::{error::Error, sync::Arc};

use gateway_application::{
    ExpectedAsset, SelfCheckConfig, SelfCheckRepository, SelfCheckService, SystemClock,
};
use gateway_domain::{AddressKey, ChainEnvironment};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    PostgresRepository,
    test_support::{DATABASE, connect},
};

const ASSET: Uuid = Uuid::from_u128(9101);
const COLLECTOR: Uuid = Uuid::from_u128(9102);
const SOURCE: Uuid = Uuid::from_u128(9103);

fn key(last: u8) -> Result<AddressKey, Box<dyn Error>> {
    let mut bytes = vec![0x41; 21];
    bytes[20] = last;
    Ok(AddressKey::new(bytes)?)
}

fn config() -> Result<SelfCheckConfig, Box<dyn Error>> {
    Ok(SelfCheckConfig {
        collectors: vec![key(2)?],
        assets: vec![ExpectedAsset {
            chain: "tron".to_owned(),
            network: "nile".to_owned(),
            contract: key(1)?,
        }],
        environment: ChainEnvironment::Testnet,
        max_clock_skew_seconds: 5,
    })
}

#[tokio::test]
#[ignore = "requires GATEWAY_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn startup_self_check_names_each_broken_invariant() -> Result<(), Box<dyn Error>> {
    let _guard = DATABASE.lock().await;
    let pool = connect().await?;
    sqlx::query("TRUNCATE chain_sources, chain_assets, chain_finality_policies CASCADE")
        .execute(&pool)
        .await?;
    let asset_key = key(1)?;
    let collector_key = key(2)?;
    let asset_pin = format!("{:x}", Sha256::digest(asset_key.as_bytes()));
    let collector_pin = format!("{:x}", Sha256::digest(collector_key.as_bytes()));
    sqlx::query("INSERT INTO chain_assets(id,chain,network,chain_environment,contract_address_key,display_symbol,decimals,status,pinned_sha256,approved_by) VALUES($1,'tron','nile','testnet',$2,'USDT',6,'active',$3,'test')")
        .bind(ASSET).bind(asset_key.as_bytes()).bind(asset_pin).execute(&pool).await?;
    sqlx::query("INSERT INTO collector_addresses(id,asset_id,address_key,address_text,state,valid_from,pinned_sha256,approved_by) VALUES($1,$2,$3,'collector','active',now(),$4,'test')")
        .bind(COLLECTOR).bind(ASSET).bind(collector_key.as_bytes()).bind(collector_pin).execute(&pool).await?;
    sqlx::query("INSERT INTO chain_sources(id,chain,network,chain_environment,source_key,provider_group,kind,db_principal,requires_dedicated_principal,state,valid_from) VALUES($1,'tron','nile','testnet','source','group','hosted_rpc','gateway',false,'active',now())")
        .bind(SOURCE).execute(&pool).await?;
    sqlx::query("INSERT INTO chain_finality_policies(id,chain,network,chain_environment,version,status,min_confirmations,required_source_finality,min_independent_groups,max_evidence_age_seconds,observed_at) VALUES($1,'tron','nile','testnet','v1','active',1,'confirmed',2,60,now())")
        .bind(Uuid::from_u128(9104)).execute(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    assert!(
        SelfCheckService::new(Arc::clone(&repository), SystemClock, config()?)
            .run()
            .await?
            .passed
    );

    // Each mutation is restored immediately so a failure identifies one
    // invariant rather than inheriting an earlier test defect.
    for (name, break_sql, restore_sql) in [
        (
            "pinned_collectors",
            "UPDATE collector_addresses SET state='retired',retired_at=now()",
            "UPDATE collector_addresses SET state='active',retired_at=NULL",
        ),
        (
            "pinned_assets",
            "UPDATE chain_assets SET pinned_sha256=repeat('0',64)",
            "UPDATE chain_assets SET pinned_sha256=encode(sha256(contract_address_key),'hex')",
        ),
        (
            "environment_agreement",
            "UPDATE chain_sources SET chain_environment='mainnet'",
            "UPDATE chain_sources SET chain_environment='testnet'",
        ),
        (
            "finality_policy_present",
            "UPDATE chain_finality_policies SET status='disabled'",
            "UPDATE chain_finality_policies SET status='active'",
        ),
    ] {
        sqlx::query(break_sql).execute(&pool).await?;
        let report = SelfCheckService::new(Arc::clone(&repository), SystemClock, config()?)
            .run()
            .await?;
        assert_eq!(
            report
                .checks
                .iter()
                .filter(|check| !check.passed)
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>(),
            vec![name]
        );
        sqlx::query(restore_sql).execute(&pool).await?;
    }

    sqlx::query("INSERT INTO chain_observations(id,source_id,asset_id,collector_address_id,chain,network,chain_environment,observation_kind,tx_hash,event_index,block_number,block_hash,block_time,token_key,token_display,from_address_key,from_address_text,to_address_key,to_address_text,amount_raw,decimals,execution_status,source_finality,source_head,evidence_sha256,observer_version,parser_version,fence_token,semantic_hash,observed_at) VALUES($1,$2,$3,$4,'tron','nile','testnet','cursor_scan','tx',0,100,'block',now(),$5,'USDT',$6,'from',$7,'to',1,6,'success','confirmed',100,repeat('1',64),'test','test',1,$8,now())")
        .bind(Uuid::from_u128(9105)).bind(SOURCE).bind(ASSET).bind(COLLECTOR)
        .bind(asset_key.as_bytes()).bind(key(3)?.as_bytes()).bind(collector_key.as_bytes()).bind(vec![1_u8;32]).execute(&pool).await?;
    sqlx::query("INSERT INTO chain_cursors(source_id,observation_kind,collector_address_id,cursor_kind,cursor_value,fence_token,updated_at) VALUES($1,'cursor_scan',$2,'block','101',1,now())")
        .bind(SOURCE).bind(COLLECTOR).execute(&pool).await?;
    let report = SelfCheckService::new(Arc::clone(&repository), SystemClock, config()?)
        .run()
        .await?;
    assert_eq!(
        report
            .checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>(),
        vec!["cursor_sanity"]
    );
    sqlx::query("UPDATE chain_cursors SET cursor_value='100'")
        .execute(&pool)
        .await?;

    let rows = repository
        .self_check(
            &config()?,
            OffsetDateTime::now_utc() - Duration::seconds(30),
        )
        .await?;
    assert_eq!(
        rows.iter()
            .filter(|check| !check.passed)
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>(),
        vec!["clock_skew"]
    );
    Ok(())
}
