use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use gateway_application::{
    ChainEventKey, ChainReader, ChainScanner, CollectorState, CollectorWatch, CursorKind,
    CursorPosition, ScanError,
};
use gateway_domain::{ChainEnvironment, TxHash};
use uuid::Uuid;

use super::{ScanLane, TronHttpSource, TronSourceConfig, TronSourceError, TronTransport};
use crate::{address::from_hex, event::TokenView};

type TestResult = Result<(), Box<dyn Error>>;

const USDT_LOG_FORM: &str = "a614f803b6fd780986a42c78ec9c7f77e6ded13c";
const COLLECTOR_LOG_FORM: &str = "1f2e3d4c5b6a798807162534435261708f9e0d1c";
const TX: &str = "b1f6e2b6b9d4d0a3c25b1e70b9a1f1e2c3d4e5f60718293a4b5c6d7e8f901234";

/// A provider that answers from captured files and records what was asked.
///
/// Every answer this gateway can receive is a file, so the hostile ones are as
/// easy to reach as the healthy one.
#[derive(Debug, Default)]
struct StubTransport {
    answers: HashMap<String, Result<String, TronSourceError>>,
    calls: Mutex<Vec<String>>,
}

impl StubTransport {
    fn with(mut self, key: &str, body: &str) -> Self {
        self.answers.insert(key.to_owned(), Ok(body.to_owned()));
        self
    }

    fn failing(mut self, key: &str, error: TronSourceError) -> Self {
        self.answers.insert(key.to_owned(), Err(error));
        self
    }

    fn answer(&self, key: &str) -> Result<String, TronSourceError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(key.to_owned());
        }
        self.answers.get(key).cloned().unwrap_or_else(|| {
            Err(TronSourceError::Unreachable(format!(
                "the test provider was not given an answer for {key}"
            )))
        })
    }

    fn called(&self, key: &str) -> usize {
        self.calls
            .lock()
            .map(|calls| calls.iter().filter(|call| call.as_str() == key).count())
            .unwrap_or_default()
    }
}

#[async_trait]
impl TronTransport for StubTransport {
    async fn post(&self, path: &str, body: serde_json::Value) -> Result<String, TronSourceError> {
        let selector = body
            .get("num")
            .map(ToString::to_string)
            .or_else(|| {
                body.get("value")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default();
        self.answer(&format!("POST {path}:{selector}"))
    }

    async fn get(&self, path_and_query: &str) -> Result<String, TronSourceError> {
        let path = path_and_query.split('?').next().unwrap_or(path_and_query);
        self.answer(&format!("GET {path}"))
    }
}

fn healthy_transport() -> StubTransport {
    StubTransport::default()
        .with(
            "POST wallet/getnowblock:",
            include_str!("../../fixtures/now_block.json"),
        )
        .with(
            "POST walletsolidity/getnowblock:",
            include_str!("../../fixtures/solid_block.json"),
        )
        .with(
            "POST wallet/getblockbynum:68000000",
            include_str!("../../fixtures/block_68000000.json"),
        )
        .with(
            "POST wallet/getblockbynum:68000001",
            include_str!("../../fixtures/block_68000001.json"),
        )
        .with(
            "POST wallet/gettransactioninfobyblocknum:68000000",
            include_str!("../../fixtures/block_infos_68000000.json"),
        )
        .with(
            "POST wallet/gettransactioninfobyblocknum:68000001",
            include_str!("../../fixtures/block_infos_empty.json"),
        )
        .with(
            &format!("POST wallet/gettransactioninfobyid:{TX}"),
            include_str!("../../fixtures/transfer_success.json"),
        )
        .with(
            "GET v1/accounts/TCp5Ln88paZVmapdUXWSQMjCWTUWTHybep/transactions/trc20",
            include_str!("../../fixtures/trc20_index.json"),
        )
}

fn config(lane: ScanLane) -> Result<TronSourceConfig, Box<dyn Error>> {
    Ok(TronSourceConfig {
        base_url: "https://provider.invalid".to_owned(),
        api_key: None,
        api_key_header: "TRON-PRO-API-KEY".to_owned(),
        chain: "tron".to_owned(),
        network: "mainnet".to_owned(),
        chain_environment: ChainEnvironment::Mainnet,
        lane,
        request_timeout: Duration::from_secs(10),
        max_blocks_per_scan: 2,
        bootstrap_lookback_blocks: 5,
        tokens: vec![TokenView {
            token_key: from_hex(USDT_LOG_FORM)?,
            decimals: 6,
            display: "USDT".to_owned(),
        }],
        user_agent: "crypto-gateway-test".to_owned(),
    })
}

fn watch() -> Result<CollectorWatch, Box<dyn Error>> {
    Ok(CollectorWatch {
        collector_address_id: Uuid::from_u128(7),
        address_key: from_hex(COLLECTOR_LOG_FORM)?,
        address_text: "TCp5Ln88paZVmapdUXWSQMjCWTUWTHybep".to_owned(),
        asset_id: Uuid::from_u128(8),
        token_key: from_hex(USDT_LOG_FORM)?,
        decimals: 6,
        chain: "tron".to_owned(),
        network: "mainnet".to_owned(),
        chain_environment: ChainEnvironment::Mainnet,
        state: CollectorState::Active,
    })
}

fn cursor(value: &str) -> Result<CursorPosition, Box<dyn Error>> {
    Ok(CursorPosition::new(CursorKind::Block, value, None, 4)?)
}

#[tokio::test]
async fn the_block_lane_reads_the_log_and_reports_the_event_index() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);

    let page = source
        .scan(&watch()?, Some(&cursor("67999999")?), 50)
        .await?;

    assert_eq!(page.transfers.len(), 1, "only the payment to us is read");
    let reading = &page.transfers[0];
    assert_eq!(reading.event_index, 1);
    assert_eq!(reading.amount_raw.to_string(), "273001427");
    assert_eq!(reading.block_hash.as_deref(), Some("0000000004"));
    assert_eq!(reading.decimals, 6);
    assert_eq!(page.head, Some(68_000_005));
    Ok(())
}

#[tokio::test]
async fn the_block_lane_stops_at_the_solidified_head() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);

    // Two blocks are allowed per scan, and both sit below the solidified head,
    // so the cursor lands on the second one rather than on the newest block.
    let page = source
        .scan(&watch()?, Some(&cursor("67999999")?), 50)
        .await?;
    let advanced = page.next_cursor.ok_or("the lane advanced its cursor")?;
    assert_eq!(advanced.value, "68000001");
    assert_eq!(advanced.kind, CursorKind::Block);

    // Nothing irreversible is left: the lane reports no movement instead of
    // walking into blocks the chain may still replace.
    let caught_up = source
        .scan(&watch()?, Some(&cursor("68000005")?), 50)
        .await?;
    assert!(caught_up.transfers.is_empty());
    assert_eq!(caught_up.next_cursor, None);
    assert_eq!(caught_up.head, Some(68_000_005));
    Ok(())
}

#[tokio::test]
async fn a_payment_to_somebody_else_in_the_same_block_is_not_ours() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);

    let page = source
        .scan(&watch()?, Some(&cursor("67999999")?), 50)
        .await?;

    // The block fixture also carries a transfer to a stranger. It is not a
    // reading about this collector and never reaches storage.
    let ours = watch()?.address_key;
    assert_eq!(page.transfers.len(), 1);
    assert!(
        page.transfers
            .iter()
            .all(|reading| reading.to_address == ours)
    );
    Ok(())
}

#[tokio::test]
async fn the_address_lane_asks_the_index_then_reads_the_transaction() -> TestResult {
    let transport = Arc::new(healthy_transport());
    let source = TronHttpSource::new(Arc::clone(&transport), config(ScanLane::AddressIndex)?);

    let page = source.scan(&watch()?, None, 25).await?;

    assert_eq!(page.transfers.len(), 1);
    assert_eq!(page.transfers[0].event_index, 1);
    // The index named the same transaction twice and once without an
    // identifier. The transaction is read once, and the row without an
    // identifier is not invented into a reading.
    assert_eq!(
        transport.called(&format!("POST wallet/gettransactioninfobyid:{TX}")),
        1
    );
    let advanced = page.next_cursor.ok_or("the lane advanced its cursor")?;
    assert_eq!(advanced.kind, CursorKind::LogicalTime);
    assert_eq!(advanced.value, "1758000000000");
    Ok(())
}

#[tokio::test]
async fn a_provider_outage_is_reported_as_an_outage_not_as_an_empty_chain() -> TestResult {
    let transport = healthy_transport().failing(
        "POST wallet/getnowblock:",
        TronSourceError::Unreachable("429 Too Many Requests".to_owned()),
    );
    let source = TronHttpSource::new(transport, config(ScanLane::BlockRange)?);

    let Err(error) = source.scan(&watch()?, Some(&cursor("67999999")?), 50).await else {
        return Err("an unreachable provider must not produce a page".into());
    };

    assert!(matches!(error, ScanError::Unreachable(_)));
    assert!(error.is_transient(), "an outage is worth retrying");
    Ok(())
}

#[tokio::test]
async fn an_unparseable_answer_is_permanent_not_retried_forever() -> TestResult {
    let transport =
        healthy_transport().with("POST walletsolidity/getnowblock:", "<html>503</html>");
    let source = TronHttpSource::new(transport, config(ScanLane::BlockRange)?);

    let Err(error) = source.scan(&watch()?, Some(&cursor("67999999")?), 50).await else {
        return Err("a broken answer must not produce a page".into());
    };

    assert!(matches!(error, ScanError::Unparseable(_)));
    assert!(!error.is_transient());
    Ok(())
}

#[tokio::test]
async fn a_collector_from_another_environment_is_refused() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);
    let foreign = CollectorWatch {
        chain_environment: ChainEnvironment::Testnet,
        ..watch()?
    };

    let outcome = source.scan(&foreign, None, 10).await;

    assert!(matches!(outcome, Err(ScanError::Unparseable(_))));
    Ok(())
}

#[tokio::test]
async fn the_verifier_re_reads_one_event_and_gets_the_same_identity() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);
    let event = ChainEventKey {
        chain: "tron".to_owned(),
        network: "mainnet".to_owned(),
        chain_environment: ChainEnvironment::Mainnet,
        tx_hash: TxHash::new(TX)?,
        event_index: 1,
    };

    let reading = source
        .lookup(&event)
        .await?
        .ok_or("the chain knows this event")?;

    assert_eq!(reading.amount_raw.to_string(), "273001427");
    assert_eq!(reading.event_index, 1);
    assert_eq!(reading.evidence_sha256.len(), 64);
    Ok(())
}

#[tokio::test]
async fn an_event_the_chain_does_not_carry_is_absent_not_invented() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);
    let event = ChainEventKey {
        chain: "tron".to_owned(),
        network: "mainnet".to_owned(),
        chain_environment: ChainEnvironment::Mainnet,
        tx_hash: TxHash::new(TX)?,
        // The transaction exists; this event index does not.
        event_index: 9,
    };

    assert_eq!(source.lookup(&event).await?, None);
    Ok(())
}

#[tokio::test]
async fn a_source_never_answers_about_another_environment() -> TestResult {
    let source = TronHttpSource::new(healthy_transport(), config(ScanLane::BlockRange)?);
    let event = ChainEventKey {
        chain: "tron".to_owned(),
        network: "mainnet".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        tx_hash: TxHash::new(TX)?,
        event_index: 1,
    };

    assert_eq!(source.lookup(&event).await?, None);
    Ok(())
}
