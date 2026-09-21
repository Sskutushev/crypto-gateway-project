use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::RawAmount;

const MAX_ADDRESS_KEY_BYTES: usize = 64;
const MAX_TX_HASH_CHARS: usize = 200;
const MAX_MEMO_CHARS: usize = 200;

/// Mainnet and testnet are different worlds. Every row carries the environment
/// so a testnet reading can never be compared against a mainnet obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainEnvironment {
    Testnet,
    Mainnet,
}

impl ChainEnvironment {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }
}

impl FromStr for ChainEnvironment {
    type Err = ChainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "testnet" => Ok(Self::Testnet),
            "mainnet" => Ok(Self::Mainnet),
            other => Err(ChainError::UnknownChainEnvironment(other.to_owned())),
        }
    }
}

impl fmt::Display for ChainEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The canonical bytes of an address.
///
/// Addresses are compared as bytes and never as display strings: TRON has
/// base58 and hex, EVM has checksummed and lowercase, TON has four textual
/// forms of the same account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AddressKey(Vec<u8>);

impl AddressKey {
    /// Wraps canonical address bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError::InvalidAddressKey`] for empty or oversized input.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, ChainError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > MAX_ADDRESS_KEY_BYTES {
            return Err(ChainError::InvalidAddressKey);
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        const DIGITS: [u8; 16] = *b"0123456789abcdef";
        let mut hex = String::with_capacity(self.0.len() * 2);
        for byte in &self.0 {
            hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
            hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        hex
    }
}

/// A transaction identifier, normalized for comparison but never trusted as
/// proof of anything on its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TxHash(String);

impl TxHash {
    /// Normalizes a transaction hash: trimmed, lowercased, `0x` prefix removed.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError::InvalidTxHash`] when the value is empty, too long,
    /// or contains characters no chain uses in an identifier.
    pub fn new(value: &str) -> Result<Self, ChainError> {
        let trimmed = value.trim();
        let trimmed = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        let normalized = trimmed.to_ascii_lowercase();
        let valid = !normalized.is_empty()
            && normalized.len() <= MAX_TX_HASH_CHARS
            && normalized
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !valid {
            return Err(ChainError::InvalidTxHash);
        }
        Ok(Self(normalized))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TxHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An opaque payment reference carried by chains that support a comment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Memo(String);

impl Memo {
    /// Normalizes a chain memo.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError::InvalidMemo`] for empty or oversized input.
    pub fn new(value: &str) -> Result<Self, ChainError> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.chars().count() > MAX_MEMO_CHARS {
            return Err(ChainError::InvalidMemo);
        }
        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which lane produced a reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    /// The address-indexed fast lane: someone else's index, a detection hint.
    FastDetect,
    /// The durable cursor lane over chain events: the canonical reading path.
    CursorScan,
    /// The verifier's own independent re-read of one transaction.
    TargetedLookup,
}

impl ObservationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FastDetect => "fast_detect",
            Self::CursorScan => "cursor_scan",
            Self::TargetedLookup => "targeted_lookup",
        }
    }
}

impl FromStr for ObservationKind {
    type Err = ChainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fast_detect" => Ok(Self::FastDetect),
            "cursor_scan" => Ok(Self::CursorScan),
            "targeted_lookup" => Ok(Self::TargetedLookup),
            other => Err(ChainError::UnknownObservationKind(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Success,
    Failed,
}

impl ExecutionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for ExecutionStatus {
    type Err = ChainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            other => Err(ChainError::UnknownExecutionStatus(other.to_owned())),
        }
    }
}

/// How final a source claims a reading is. This is a claim, not a fact: the
/// finality policy decides what it is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFinality {
    Seen,
    Confirmed,
    Finalized,
}

impl SourceFinality {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
        }
    }
}

impl FromStr for SourceFinality {
    type Err = ChainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "seen" => Ok(Self::Seen),
            "confirmed" => Ok(Self::Confirmed),
            "finalized" => Ok(Self::Finalized),
            other => Err(ChainError::UnknownSourceFinality(other.to_owned())),
        }
    }
}

/// One source's reading of one transfer, before any trust is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedTransfer {
    pub chain: String,
    pub network: String,
    pub chain_environment: ChainEnvironment,
    pub tx_hash: TxHash,
    pub event_index: i32,
    pub block_number: Option<i64>,
    pub block_hash: Option<String>,
    pub parent_hash: Option<String>,
    pub block_time: Option<OffsetDateTime>,
    pub token_key: AddressKey,
    pub token_display: String,
    pub from_address: AddressKey,
    pub from_address_text: String,
    pub to_address: AddressKey,
    pub to_address_text: String,
    pub amount_raw: RawAmount,
    pub decimals: i16,
    pub memo: Option<Memo>,
    pub execution_status: ExecutionStatus,
    pub source_finality: SourceFinality,
    pub source_head: Option<i64>,
    pub evidence_sha256: String,
    pub evidence_uri: Option<String>,
}

impl ObservedTransfer {
    /// Hashes the identity of the *claim*, not of the event.
    ///
    /// An exact repeat of the same answer collapses into one row, while the
    /// same source moving a transfer from `seen` to `finalized`, or reporting a
    /// different block hash after a reorg, produces new immutable rows instead
    /// of a unique-constraint conflict.
    #[must_use]
    pub fn semantic_hash(&self, source_id: Uuid, kind: ObservationKind) -> [u8; 32] {
        let mut digest = Sha256::new();
        let mut field = |bytes: &[u8]| {
            let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
            digest.update(length.to_be_bytes());
            digest.update(bytes);
        };
        field(source_id.as_bytes());
        field(self.chain.as_bytes());
        field(self.network.as_bytes());
        field(self.chain_environment.as_str().as_bytes());
        field(kind.as_str().as_bytes());
        field(self.tx_hash.as_str().as_bytes());
        field(&self.event_index.to_be_bytes());
        field(self.block_hash.as_deref().unwrap_or("").as_bytes());
        field(self.execution_status.as_str().as_bytes());
        field(self.source_finality.as_str().as_bytes());
        field(self.token_key.as_bytes());
        field(self.to_address.as_bytes());
        field(self.amount_raw.to_string().as_bytes());
        field(self.memo.as_ref().map_or("", Memo::as_str).as_bytes());
        digest.finalize().into()
    }
}

/// The monotonic lifecycle of a canonical transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    Observed,
    Canonical,
    Confirmed,
    Finalized,
    Invalidated,
}

impl TransferState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Canonical => "canonical",
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
            Self::Invalidated => "invalidated",
        }
    }

    /// Reports whether a transition is allowed.
    ///
    /// Progress is monotonic and any state may be invalidated. A late
    /// `confirmed` arriving after `finalized` is kept as evidence but must
    /// never move the current state backwards.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Observed, Self::Canonical | Self::Invalidated)
                | (Self::Canonical, Self::Confirmed | Self::Invalidated)
                | (Self::Confirmed, Self::Finalized | Self::Invalidated)
                | (Self::Finalized, Self::Invalidated)
        )
    }
}

impl FromStr for TransferState {
    type Err = ChainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "observed" => Ok(Self::Observed),
            "canonical" => Ok(Self::Canonical),
            "confirmed" => Ok(Self::Confirmed),
            "finalized" => Ok(Self::Finalized),
            "invalidated" => Ok(Self::Invalidated),
            other => Err(ChainError::UnknownTransferState(other.to_owned())),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ChainError {
    #[error("an address key must contain between 1 and 64 canonical bytes")]
    InvalidAddressKey,
    #[error("a transaction hash must be a short alphanumeric identifier")]
    InvalidTxHash,
    #[error("a memo must contain between 1 and 200 characters")]
    InvalidMemo,
    #[error("unknown chain environment: {0}")]
    UnknownChainEnvironment(String),
    #[error("unknown observation kind: {0}")]
    UnknownObservationKind(String),
    #[error("unknown execution status: {0}")]
    UnknownExecutionStatus(String),
    #[error("unknown source finality: {0}")]
    UnknownSourceFinality(String),
    #[error("unknown transfer state: {0}")]
    UnknownTransferState(String),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{
        AddressKey, ChainEnvironment, ExecutionStatus, Memo, ObservationKind, ObservedTransfer,
        SourceFinality, TransferState, TxHash,
    };
    use crate::{ChainError, RawAmount};

    const SOURCE: Uuid = Uuid::from_u128(1);

    fn transfer() -> Result<ObservedTransfer, ChainError> {
        Ok(ObservedTransfer {
            chain: "tron".to_owned(),
            network: "mainnet".to_owned(),
            chain_environment: ChainEnvironment::Mainnet,
            tx_hash: TxHash::new("0xABCDEF01")?,
            event_index: 0,
            block_number: Some(100),
            block_hash: Some("block-1".to_owned()),
            parent_hash: Some("block-0".to_owned()),
            block_time: Some(OffsetDateTime::UNIX_EPOCH),
            token_key: AddressKey::new([1_u8; 20])?,
            token_display: "USDT".to_owned(),
            from_address: AddressKey::new([2_u8; 20])?,
            from_address_text: "TFrom".to_owned(),
            to_address: AddressKey::new([3_u8; 20])?,
            to_address_text: "TTo".to_owned(),
            amount_raw: RawAmount::from_str("1000").map_err(|_| ChainError::InvalidAddressKey)?,
            decimals: 6,
            memo: None,
            execution_status: ExecutionStatus::Success,
            source_finality: SourceFinality::Seen,
            source_head: Some(120),
            evidence_sha256: "0".repeat(64),
            evidence_uri: None,
        })
    }

    #[test]
    fn addresses_compare_as_bytes_not_as_display_strings() -> Result<(), ChainError> {
        let lowercase = AddressKey::new([0xAB_u8, 0xCD])?;
        let same_bytes = AddressKey::new(vec![0xAB_u8, 0xCD])?;

        assert_eq!(lowercase, same_bytes);
        assert_eq!(lowercase.to_hex(), "abcd");
        assert_eq!(AddressKey::new([]), Err(ChainError::InvalidAddressKey));
        assert_eq!(
            AddressKey::new([0_u8; 65]),
            Err(ChainError::InvalidAddressKey)
        );
        Ok(())
    }

    #[test]
    fn transaction_hashes_normalize_case_and_prefix() -> Result<(), ChainError> {
        assert_eq!(TxHash::new("0xAbCd")?.as_str(), "abcd");
        assert_eq!(TxHash::new("  abcd  ")?.as_str(), "abcd");
        assert_eq!(TxHash::new(""), Err(ChainError::InvalidTxHash));
        assert_eq!(TxHash::new("../etc/passwd"), Err(ChainError::InvalidTxHash));
        Ok(())
    }

    #[test]
    fn a_repeated_answer_hashes_the_same_and_progress_does_not() -> Result<(), ChainError> {
        let seen = transfer()?;
        let repeat = transfer()?;
        assert_eq!(
            seen.semantic_hash(SOURCE, ObservationKind::CursorScan),
            repeat.semantic_hash(SOURCE, ObservationKind::CursorScan)
        );

        let finalized = ObservedTransfer {
            source_finality: SourceFinality::Finalized,
            ..transfer()?
        };
        assert_ne!(
            seen.semantic_hash(SOURCE, ObservationKind::CursorScan),
            finalized.semantic_hash(SOURCE, ObservationKind::CursorScan)
        );

        let reorged = ObservedTransfer {
            block_hash: Some("block-1b".to_owned()),
            ..transfer()?
        };
        assert_ne!(
            seen.semantic_hash(SOURCE, ObservationKind::CursorScan),
            reorged.semantic_hash(SOURCE, ObservationKind::CursorScan)
        );

        let other_lane = seen.semantic_hash(SOURCE, ObservationKind::TargetedLookup);
        assert_ne!(
            seen.semantic_hash(SOURCE, ObservationKind::CursorScan),
            other_lane
        );

        let other_source = seen.semantic_hash(Uuid::from_u128(2), ObservationKind::CursorScan);
        assert_ne!(
            seen.semantic_hash(SOURCE, ObservationKind::CursorScan),
            other_source
        );
        Ok(())
    }

    #[test]
    fn field_boundaries_cannot_be_shifted_between_neighbours() -> Result<(), ChainError> {
        let left = ObservedTransfer {
            chain: "tro".to_owned(),
            network: "nmainnet".to_owned(),
            ..transfer()?
        };
        let right = ObservedTransfer {
            chain: "tron".to_owned(),
            network: "mainnet".to_owned(),
            ..transfer()?
        };

        assert_ne!(
            left.semantic_hash(SOURCE, ObservationKind::CursorScan),
            right.semantic_hash(SOURCE, ObservationKind::CursorScan)
        );
        Ok(())
    }

    #[test]
    fn memo_changes_the_claim() -> Result<(), ChainError> {
        let without = transfer()?;
        let with = ObservedTransfer {
            memo: Some(Memo::new("RP-4FM93AZK")?),
            ..transfer()?
        };

        assert_ne!(
            without.semantic_hash(SOURCE, ObservationKind::CursorScan),
            with.semantic_hash(SOURCE, ObservationKind::CursorScan)
        );
        assert_eq!(Memo::new("   "), Err(ChainError::InvalidMemo));
        Ok(())
    }

    #[test]
    fn transfer_state_never_regresses() {
        assert!(TransferState::Observed.can_transition_to(TransferState::Canonical));
        assert!(TransferState::Confirmed.can_transition_to(TransferState::Finalized));
        assert!(TransferState::Finalized.can_transition_to(TransferState::Invalidated));

        assert!(!TransferState::Finalized.can_transition_to(TransferState::Confirmed));
        assert!(!TransferState::Canonical.can_transition_to(TransferState::Observed));
        assert!(!TransferState::Observed.can_transition_to(TransferState::Finalized));
        assert!(!TransferState::Invalidated.can_transition_to(TransferState::Finalized));
        assert!(!TransferState::Finalized.can_transition_to(TransferState::Finalized));
    }

    #[test]
    fn enumerations_round_trip_through_their_database_text() -> Result<(), ChainError> {
        assert_eq!(
            ChainEnvironment::from_str(ChainEnvironment::Mainnet.as_str())?,
            ChainEnvironment::Mainnet
        );
        assert_eq!(
            ObservationKind::from_str(ObservationKind::CursorScan.as_str())?,
            ObservationKind::CursorScan
        );
        assert_eq!(
            SourceFinality::from_str(SourceFinality::Finalized.as_str())?,
            SourceFinality::Finalized
        );
        assert_eq!(
            TransferState::from_str(TransferState::Canonical.as_str())?,
            TransferState::Canonical
        );
        assert!(ExecutionStatus::from_str("maybe").is_err());
        Ok(())
    }
}
