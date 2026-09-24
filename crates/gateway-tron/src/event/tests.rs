use std::error::Error;

use gateway_domain::{ChainEnvironment, ExecutionStatus, SourceFinality};
use time::OffsetDateTime;

use super::{
    BlockRef, ChainContext, HeadState, TokenView, TransactionInfo, TronParseError, block_time,
    parse_transfers, to_observation,
};
use crate::address::{decode_hex, from_evm_bytes, from_hex};

type TestResult = Result<(), Box<dyn Error>>;

const USDT_LOG_FORM: &str = "a614f803b6fd780986a42c78ec9c7f77e6ded13c";
const COLLECTOR_LOG_FORM: &str = "1f2e3d4c5b6a798807162534435261708f9e0d1c";
const FOREIGN_LOG_FORM: &str = "b1c2d3e4f5061728394a5b6c7d8e9fa0b1c2d3e4";
const EVIDENCE: &str = "1111111111111111111111111111111111111111111111111111111111111111";

const HEAD: HeadState = HeadState {
    head: 68_000_010,
    solid_head: 68_000_005,
};

fn load(name: &str) -> Result<TransactionInfo, Box<dyn Error>> {
    let raw = match name {
        "transfer_success" => include_str!("../../fixtures/transfer_success.json"),
        "transfer_multiple" => include_str!("../../fixtures/transfer_multiple.json"),
        "transfer_foreign_token" => include_str!("../../fixtures/transfer_foreign_token.json"),
        "transfer_foreign_recipient" => {
            include_str!("../../fixtures/transfer_foreign_recipient.json")
        }
        "transfer_failed" => include_str!("../../fixtures/transfer_failed.json"),
        "hostile_unpadded_address" => include_str!("../../fixtures/hostile_unpadded_address.json"),
        "hostile_short_amount" => include_str!("../../fixtures/hostile_short_amount.json"),
        "hostile_topic_count" => include_str!("../../fixtures/hostile_topic_count.json"),
        "hostile_zero_amount" => include_str!("../../fixtures/hostile_zero_amount.json"),
        "hostile_no_receipt" => include_str!("../../fixtures/hostile_no_receipt.json"),
        "hostile_max_amount" => include_str!("../../fixtures/hostile_max_amount.json"),
        other => return Err(format!("unknown fixture {other}").into()),
    };
    Ok(serde_json::from_str(raw)?)
}

fn context() -> ChainContext {
    ChainContext {
        chain: "tron".to_owned(),
        network: "mainnet".to_owned(),
        chain_environment: ChainEnvironment::Mainnet,
    }
}

fn block() -> Result<BlockRef, Box<dyn Error>> {
    Ok(BlockRef {
        number: 68_000_000,
        hash: "0000000004".to_owned(),
        parent_hash: Some("0000000003".to_owned()),
        time: OffsetDateTime::from_unix_timestamp(1_758_000_000)?,
    })
}

fn usdt() -> Result<TokenView, Box<dyn Error>> {
    Ok(TokenView {
        token_key: from_hex(USDT_LOG_FORM)?,
        decimals: 6,
        display: "USDT".to_owned(),
    })
}

fn refusal(name: &str) -> Result<TronParseError, Box<dyn Error>> {
    match parse_transfers(&load(name)?) {
        Ok(_) => {
            Err(format!("{name} was accepted, which is the defect this test exists for").into())
        }
        Err(error) => Ok(error),
    }
}

#[test]
fn a_payment_is_read_from_the_log_and_not_from_a_summary() -> TestResult {
    let info = load("transfer_success")?;
    let transfers = parse_transfers(&info)?;

    assert_eq!(transfers.len(), 1, "the Approval event is not a transfer");
    let transfer = &transfers[0];
    // The Approval log sits at index 0, so this transfer's identity is index 1.
    // An address-indexed summary reports the transaction and loses exactly this.
    assert_eq!(transfer.event_index, 1);
    assert_eq!(transfer.amount.to_string(), "273001427");
    assert_eq!(
        transfer.to,
        from_evm_bytes(&decode_hex(COLLECTOR_LOG_FORM)?)?
    );

    let observation = to_observation(
        transfer,
        &info,
        &block()?,
        &context(),
        HEAD,
        Some(&usdt()?),
        EVIDENCE.to_owned(),
    )?;

    assert_eq!(observation.execution_status, ExecutionStatus::Success);
    assert_eq!(observation.decimals, 6);
    assert_eq!(observation.token_display, "USDT");
    assert_eq!(observation.block_number, Some(68_000_000));
    assert_eq!(observation.source_head, Some(68_000_005));
    assert!(observation.to_address_text.starts_with('T'));
    Ok(())
}

#[test]
fn two_transfers_in_one_transaction_keep_separate_identities() -> TestResult {
    let transfers = parse_transfers(&load("transfer_multiple")?)?;

    assert_eq!(transfers.len(), 2);
    assert_eq!(transfers[0].event_index, 0);
    assert_eq!(transfers[1].event_index, 2);
    assert_ne!(transfers[0].amount, transfers[1].amount);
    Ok(())
}

#[test]
fn an_unknown_token_is_described_by_its_own_address_and_no_scale() -> TestResult {
    let info = load("transfer_foreign_token")?;
    let transfers = parse_transfers(&info)?;
    let observation = to_observation(
        &transfers[0],
        &info,
        &block()?,
        &context(),
        HEAD,
        Some(&usdt()?),
        EVIDENCE.to_owned(),
    )?;

    // The allowlist decision is made later, from these bytes. Nothing here may
    // make a stranger's token look like the one this gateway accepts.
    assert_ne!(observation.token_display, "USDT");
    assert_eq!(observation.decimals, 0);
    assert_eq!(
        observation.token_key.to_hex(),
        format!("41{FOREIGN_LOG_FORM}")
    );
    Ok(())
}

#[test]
fn a_transfer_to_a_stranger_still_decodes_and_names_the_stranger() -> TestResult {
    let transfers = parse_transfers(&load("transfer_foreign_recipient")?)?;

    // The parser does not filter by recipient. Refusing a reading that is not
    // about us is the intake layer's decision, and it counts what it refused.
    assert_eq!(transfers[0].to.to_hex(), format!("41{FOREIGN_LOG_FORM}"));
    Ok(())
}

#[test]
fn a_reverted_transaction_is_read_as_failed_not_skipped() -> TestResult {
    let info = load("transfer_failed")?;
    let transfers = parse_transfers(&info)?;
    let observation = to_observation(
        &transfers[0],
        &info,
        &block()?,
        &context(),
        HEAD,
        Some(&usdt()?),
        EVIDENCE.to_owned(),
    )?;

    assert_eq!(observation.execution_status, ExecutionStatus::Failed);
    Ok(())
}

#[test]
fn an_answer_without_an_execution_result_is_refused_rather_than_assumed() -> TestResult {
    let info = load("hostile_no_receipt")?;
    let transfers = parse_transfers(&info)?;
    let outcome = to_observation(
        &transfers[0],
        &info,
        &block()?,
        &context(),
        HEAD,
        Some(&usdt()?),
        EVIDENCE.to_owned(),
    );

    assert_eq!(outcome.err(), Some(TronParseError::MissingExecutionResult));
    Ok(())
}

#[test]
fn hostile_words_are_refused_instead_of_being_trimmed_into_shape() -> TestResult {
    assert_eq!(
        refusal("hostile_unpadded_address")?,
        TronParseError::AddressWordNotPadded
    );
    assert_eq!(
        refusal("hostile_short_amount")?,
        TronParseError::WrongWordLength(3)
    );
    // Two indexed fields is not a TRC-20 transfer; the log is another event,
    // passed over without stopping the scan.
    assert!(parse_transfers(&load("hostile_topic_count")?)?.is_empty());
    assert_eq!(refusal("hostile_zero_amount")?, TronParseError::ZeroAmount);
    Ok(())
}

#[test]
fn the_largest_representable_amount_survives_without_wrapping() -> TestResult {
    let transfers = parse_transfers(&load("hostile_max_amount")?)?;

    assert_eq!(
        transfers[0].amount.to_string(),
        "115792089237316195423570985008687907853269984665640564039457584007913129639935"
    );
    Ok(())
}

#[test]
fn finality_follows_the_solidified_head_not_the_newest_block() {
    assert_eq!(HEAD.finality_of(68_000_000), SourceFinality::Finalized);
    assert_eq!(HEAD.finality_of(68_000_005), SourceFinality::Finalized);
    assert_eq!(HEAD.finality_of(68_000_006), SourceFinality::Confirmed);
    assert_eq!(HEAD.finality_of(68_000_011), SourceFinality::Seen);
}

#[test]
fn block_times_are_read_as_milliseconds() -> TestResult {
    let parsed = block_time(1_758_000_000_000)?;

    assert_eq!(parsed.unix_timestamp(), 1_758_000_000);
    assert!(block_time(i64::MAX).is_err());
    Ok(())
}

#[test]
fn an_nft_transfer_beside_a_token_transfer_is_skipped_not_refused() -> TestResult {
    let nft = super::LogEntry {
        address: format!("41{}", "ab".repeat(20)),
        topics: vec![
            super::TRANSFER_TOPIC.to_owned(),
            format!("{}{}", "0".repeat(24), "11".repeat(20)),
            format!("{}{}", "0".repeat(24), "22".repeat(20)),
            format!("{}7", "0".repeat(63)),
        ],
        data: String::new(),
    };
    let token = super::LogEntry {
        address: format!("41{}", "cd".repeat(20)),
        topics: vec![
            super::TRANSFER_TOPIC.to_owned(),
            format!("{}{}", "0".repeat(24), "33".repeat(20)),
            format!("{}{}", "0".repeat(24), "44".repeat(20)),
        ],
        data: format!("{}f4240", "0".repeat(59)),
    };
    let info = TransactionInfo {
        id: "a".repeat(64),
        block_number: Some(1),
        block_time_stamp: Some(1_700_000_000_000),
        result: None,
        receipt: Some(super::Receipt {
            result: Some("SUCCESS".to_owned()),
        }),
        log: vec![nft, token],
    };
    let parsed = parse_transfers(&info)?;
    assert_eq!(parsed.len(), 1);
    // The index is the log's position in the transaction, the NFT included:
    // it is what makes the fact identifiable, so skipping must not renumber.
    assert_eq!(parsed[0].event_index, 1);
    assert_eq!(parsed[0].amount.to_string(), "1000000");
    Ok(())
}
