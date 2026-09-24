//! Parsing provider answers into readings.
//!
//! Nothing here performs I/O, so every hostile answer a provider can produce is
//! reachable from a fixture test. The rule this module exists to enforce: a
//! reading is built from a transaction's **event log**, never from an
//! address-indexed summary. Only the log carries the event index, and without
//! that index two transfers inside one transaction are the same fact.

use alloy_primitives::U256;
use gateway_domain::{
    AddressKey, ChainEnvironment, ExecutionStatus, ObservedTransfer, RawAmount, SourceFinality,
    TxHash,
};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::address::{decode_hex, from_evm_bytes, from_hex, to_base58};

/// `keccak256("Transfer(address,address,uint256)")`.
///
/// The constant is written out rather than computed so the crate needs no
/// keccak dependency, and so a changed value is visible in review.
pub const TRANSFER_TOPIC: &str = "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// An ABI word. A `Transfer` event uses exactly one word per value, and
/// anything else is not the event this gateway understands.
const WORD_BYTES: usize = 32;
/// The leading bytes of an address word, which an honest encoder zero-fills.
const ADDRESS_PADDING_BYTES: usize = 12;

/// One log entry as the node's HTTP API returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct LogEntry {
    /// The emitting contract, hex, with or without the `41` prefix.
    pub address: String,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub data: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Receipt {
    #[serde(default)]
    pub result: Option<String>,
}

/// The `gettransactioninfobyid` and `gettransactioninfobyblocknum` shape.
#[derive(Debug, Clone, Deserialize)]
pub struct TransactionInfo {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "blockNumber", default)]
    pub block_number: Option<i64>,
    #[serde(rename = "blockTimeStamp", default)]
    pub block_time_stamp: Option<i64>,
    /// Present and not `SUCCESS` when the transaction did not execute.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub receipt: Option<Receipt>,
    #[serde(default)]
    pub log: Vec<LogEntry>,
}

/// The identity of the block a reading belongs to.
///
/// A canonical transfer names its block hash, so a reorg is a visible change of
/// the claim rather than an invisible one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRef {
    pub number: i64,
    pub hash: String,
    pub parent_hash: Option<String>,
    pub time: OffsetDateTime,
}

/// What this deployment is reading, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainContext {
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
}

/// How far the chain has advanced, as the source reports it.
///
/// TRON's own finality source is the solidified head: a block at or below it is
/// agreed by the super representatives, and one above it is not yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadState {
    pub head: i64,
    pub solid_head: i64,
}

impl HeadState {
    /// Decides how final the source's own answer says a block is.
    #[must_use]
    pub const fn finality_of(self, block_number: i64) -> SourceFinality {
        if block_number <= self.solid_head {
            SourceFinality::Finalized
        } else if block_number <= self.head {
            SourceFinality::Confirmed
        } else {
            SourceFinality::Seen
        }
    }
}

/// One `Transfer` event, decoded from a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTransfer {
    pub event_index: i32,
    pub contract: AddressKey,
    pub from: AddressKey,
    pub to: AddressKey,
    pub amount: RawAmount,
}

/// Decodes the execution result.
///
/// An absent result is not read as success. A TRC20 transfer always carries a
/// receipt, so a missing one means the answer is not the one this parser
/// understands.
fn execution_status(info: &TransactionInfo) -> Result<ExecutionStatus, TronParseError> {
    if info
        .result
        .as_deref()
        .is_some_and(|result| !result.eq_ignore_ascii_case("SUCCESS"))
    {
        return Ok(ExecutionStatus::Failed);
    }
    match info
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.result.as_deref())
    {
        Some(result) if result.eq_ignore_ascii_case("SUCCESS") => Ok(ExecutionStatus::Success),
        Some(_) => Ok(ExecutionStatus::Failed),
        None => Err(TronParseError::MissingExecutionResult),
    }
}

/// Decodes every `Transfer` event in one transaction, keeping each event's
/// index inside the transaction.
///
/// Logs that are not `Transfer` events are skipped: they are other contracts
/// doing other things, not a malformed answer. A log that claims to be a
/// `Transfer` and is not shaped like one is refused, because that is either a
/// broken provider or an attempt to smuggle a different event past the parser.
///
/// # Errors
///
/// Returns [`TronParseError`] when a `Transfer` log cannot be decoded.
pub fn parse_transfers(info: &TransactionInfo) -> Result<Vec<ParsedTransfer>, TronParseError> {
    let mut transfers = Vec::new();
    for (index, log) in info.log.iter().enumerate() {
        let Some(topic) = log.topics.first() else {
            continue;
        };
        if !topic.trim().eq_ignore_ascii_case(TRANSFER_TOPIC) {
            continue;
        }
        let event_index = i32::try_from(index).map_err(|_| TronParseError::TooManyLogs)?;
        transfers.push(parse_transfer_log(event_index, log)?);
    }
    Ok(transfers)
}

fn parse_transfer_log(event_index: i32, log: &LogEntry) -> Result<ParsedTransfer, TronParseError> {
    if log.topics.len() != 3 {
        return Err(TronParseError::WrongTopicCount(log.topics.len()));
    }
    let contract = from_hex(&log.address).map_err(|_| TronParseError::InvalidAddress)?;
    let from = address_from_word(&log.topics[1])?;
    let to = address_from_word(&log.topics[2])?;
    let amount = amount_from_word(&log.data)?;
    Ok(ParsedTransfer {
        event_index,
        contract,
        from,
        to,
        amount,
    })
}

/// Reads an address out of a 32-byte ABI word.
///
/// The padding must be zero. A word with data in it is not an address, and
/// silently taking its last twenty bytes is how a crafted event becomes a
/// payment to somebody else.
fn address_from_word(word: &str) -> Result<AddressKey, TronParseError> {
    let bytes = word_bytes(word)?;
    if bytes[..ADDRESS_PADDING_BYTES].iter().any(|byte| *byte != 0) {
        return Err(TronParseError::AddressWordNotPadded);
    }
    from_evm_bytes(&bytes[ADDRESS_PADDING_BYTES..]).map_err(|_| TronParseError::InvalidAddress)
}

/// Reads a strictly positive amount out of a 32-byte ABI word.
fn amount_from_word(word: &str) -> Result<RawAmount, TronParseError> {
    let bytes = word_bytes(word)?;
    RawAmount::positive(U256::from_be_slice(&bytes)).map_err(|_| TronParseError::ZeroAmount)
}

fn word_bytes(word: &str) -> Result<[u8; WORD_BYTES], TronParseError> {
    let trimmed = word.trim();
    let trimmed = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes = decode_hex(trimmed).map_err(|_| TronParseError::NotHex)?;
    if bytes.len() != WORD_BYTES {
        return Err(TronParseError::WrongWordLength(bytes.len()));
    }
    let mut word = [0_u8; WORD_BYTES];
    word.copy_from_slice(&bytes);
    Ok(word)
}

/// What the caller already knows about the token behind a contract address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenView {
    pub token_key: AddressKey,
    pub decimals: i16,
    pub display: String,
}

/// Turns one decoded event into the reading the gateway stores.
///
/// The decimals and display name of an unknown contract are not invented: a
/// token nobody allowlisted is described by its own address and its scale is
/// recorded as unknown, so no part of the system can read it as an amount of
/// something familiar.
///
/// # Errors
///
/// Returns [`TronParseError`] when the transaction identifier or the execution
/// result is not one a chain produces.
pub fn to_observation(
    transfer: &ParsedTransfer,
    info: &TransactionInfo,
    block: &BlockRef,
    context: &ChainContext,
    head: HeadState,
    token: Option<&TokenView>,
    evidence_sha256: String,
) -> Result<ObservedTransfer, TronParseError> {
    let tx_hash = TxHash::new(&info.id).map_err(|_| TronParseError::InvalidTxHash)?;
    let known = token.filter(|token| token.token_key == transfer.contract);
    Ok(ObservedTransfer {
        chain: context.chain.clone(),
        network: context.network.clone(),
        chain_environment: context.chain_environment,
        tx_hash,
        event_index: transfer.event_index,
        block_number: Some(block.number),
        block_hash: Some(block.hash.clone()),
        parent_hash: block.parent_hash.clone(),
        block_time: Some(block.time),
        token_key: transfer.contract.clone(),
        token_display: known.map_or_else(
            || display_of(&transfer.contract),
            |token| token.display.clone(),
        ),
        from_address: transfer.from.clone(),
        from_address_text: display_of(&transfer.from),
        to_address: transfer.to.clone(),
        to_address_text: display_of(&transfer.to),
        amount_raw: transfer.amount,
        decimals: known.map_or(0, |token| token.decimals),
        memo: None,
        execution_status: execution_status(info)?,
        source_finality: head.finality_of(block.number),
        source_head: Some(head.solid_head),
        evidence_sha256,
        evidence_uri: None,
    })
}

/// The human-readable form of an address. Display only: every comparison in
/// this system uses the canonical bytes.
fn display_of(address: &AddressKey) -> String {
    to_base58(address).unwrap_or_else(|_| address.to_hex())
}

/// Converts the millisecond timestamps TRON reports into a point in time.
///
/// # Errors
///
/// Returns [`TronParseError::InvalidBlockTime`] for a value outside the range
/// of representable times.
pub fn block_time(milliseconds: i64) -> Result<OffsetDateTime, TronParseError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(milliseconds) * 1_000_000)
        .map_err(|_| TronParseError::InvalidBlockTime)
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TronParseError {
    #[error("a Transfer event carries exactly three topics, found {0}")]
    WrongTopicCount(usize),
    #[error("an ABI word is 32 bytes, found {0}")]
    WrongWordLength(usize),
    #[error("an address word must be zero-padded")]
    AddressWordNotPadded,
    #[error("the value is not hexadecimal")]
    NotHex,
    #[error("the log address is not a TRON address")]
    InvalidAddress,
    #[error("a transfer of zero is not a payment")]
    ZeroAmount,
    #[error("the transaction identifier is not a chain identifier")]
    InvalidTxHash,
    #[error("the answer carries no execution result")]
    MissingExecutionResult,
    #[error("the block time is not a representable instant")]
    InvalidBlockTime,
    #[error("a transaction carries more logs than an index can address")]
    TooManyLogs,
}

#[cfg(test)]
mod tests;

/// Seeded property tests over the event-log decoder; see the money module
/// in `gateway-domain` for why these stand in for a fuzzer.
#[cfg(test)]
mod fuzz_smoke {
    use std::fmt::Write as _;

    use alloy_primitives::U256;

    use super::{LogEntry, Receipt, TRANSFER_TOPIC, TransactionInfo, parse_transfers};

    struct Generator(u64);

    impl Generator {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }

        fn hex(&mut self, bytes: usize) -> String {
            (0..bytes).fold(String::with_capacity(bytes * 2), |mut out, _| {
                let byte = u8::try_from(self.below(256)).unwrap_or(0);
                let _ = write!(out, "{byte:02x}");
                out
            })
        }

        fn hex_up_to(&mut self, max_bytes: u64) -> String {
            let length = usize::try_from(self.below(max_bytes)).unwrap_or(0);
            self.hex(length)
        }

        fn text_up_to(&mut self, max_length: u64) -> String {
            let length = usize::try_from(self.below(max_length)).unwrap_or(0);
            self.text(length)
        }

        fn text(&mut self, length: usize) -> String {
            let bytes: Vec<u8> = (0..length)
                .map(|_| u8::try_from(self.below(256)).unwrap_or(0))
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    fn iterations() -> u64 {
        std::env::var("GATEWAY_FUZZ_ITERATIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2_000)
    }

    fn info(logs: Vec<LogEntry>) -> TransactionInfo {
        TransactionInfo {
            id: "a".repeat(64),
            block_number: Some(1),
            block_time_stamp: Some(1_700_000_000_000),
            result: None,
            receipt: Some(Receipt {
                result: Some("SUCCESS".to_owned()),
            }),
            log: logs,
        }
    }

    #[test]
    fn a_well_formed_transfer_log_decodes_to_exactly_its_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut generator = Generator(0x1234_5678_9ABC_DEF0);
        for _ in 0..iterations() {
            let contract = generator.hex(20);
            let from = generator.hex(20);
            let to = generator.hex(20);
            let mut amount = generator.hex(32);
            if U256::from_str_radix(&amount, 16)?.is_zero() {
                amount = format!("{}1", "0".repeat(63));
            }
            let noise = LogEntry {
                address: generator.hex(20),
                topics: vec![generator.hex(32)],
                data: generator.hex_up_to(70),
            };
            let transfer = LogEntry {
                address: format!("41{contract}"),
                topics: vec![
                    TRANSFER_TOPIC.to_owned(),
                    format!("{}{from}", "0".repeat(24)),
                    format!("{}{to}", "0".repeat(24)),
                ],
                data: amount.clone(),
            };
            let parsed = parse_transfers(&info(vec![noise, transfer]))?;
            assert_eq!(parsed.len(), 1);
            let event = &parsed[0];
            assert_eq!(event.event_index, 1, "the index is the log's position");
            assert_eq!(event.contract.as_bytes()[1..], hex_bytes(&contract)?[..]);
            assert_eq!(event.from.as_bytes()[1..], hex_bytes(&from)?[..]);
            assert_eq!(event.to.as_bytes()[1..], hex_bytes(&to)?[..]);
            assert_eq!(event.amount.as_u256(), U256::from_str_radix(&amount, 16)?);
        }
        Ok(())
    }

    #[test]
    fn arbitrary_logs_never_panic_and_a_transfer_topic_is_decoded_or_refused() {
        let mut generator = Generator(0xFEED_FACE_CAFE_BEEF);
        for _ in 0..iterations() {
            let mut logs = Vec::new();
            for _ in 0..generator.below(4) {
                let topic_count = usize::try_from(generator.below(5)).unwrap_or(0);
                let mut topics = Vec::with_capacity(topic_count);
                for index in 0..topic_count {
                    let word = match generator.below(4) {
                        0 => TRANSFER_TOPIC.to_owned(),
                        1 => generator.hex(32),
                        2 => generator.hex_up_to(40),
                        _ => generator.text_up_to(70),
                    };
                    let word = if index == 0 && generator.below(2) == 0 {
                        TRANSFER_TOPIC.to_owned()
                    } else {
                        word
                    };
                    topics.push(word);
                }
                logs.push(LogEntry {
                    topics,
                    address: match generator.below(3) {
                        0 => format!("41{}", generator.hex(20)),
                        1 => generator.hex_up_to(30),
                        _ => generator.text_up_to(50),
                    },
                    data: match generator.below(2) {
                        0 => generator.hex(32),
                        _ => generator.text_up_to(70),
                    },
                });
            }
            let transfers_claimed = logs
                .iter()
                .filter(|log| {
                    log.topics
                        .first()
                        .is_some_and(|topic| topic == TRANSFER_TOPIC)
                })
                .count();
            match parse_transfers(&info(logs)) {
                Ok(parsed) => {
                    assert_eq!(parsed.len(), transfers_claimed);
                    for event in parsed {
                        assert!(!event.amount.is_zero());
                        assert_eq!(event.contract.as_bytes().len(), 21);
                    }
                }
                Err(_) => assert!(
                    transfers_claimed > 0,
                    "a refusal needs a Transfer log to refuse"
                ),
            }
        }
    }

    fn hex_bytes(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(crate::address::decode_hex(text)?)
    }
}
