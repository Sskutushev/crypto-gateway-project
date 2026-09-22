//! The TRON rail.
//!
//! Two things must be exactly right before a single TRX of somebody's money is
//! read from this chain. Addresses: TRON writes one account four ways —
//! base58check, hex with the `41` prefix, hex without it, and the 20-byte form
//! inside event logs — and comparing any two of them as strings is how money
//! reaches the wrong place. Events: a payment is a `Transfer` log inside a
//! transaction, and only the log carries the event index that gives the fact
//! its identity. The address-indexed endpoints of every provider are hints
//! about which transaction to read, never the reading itself.

pub mod address;
pub mod event;
pub mod source;

pub use address::{TronAddressError, from_base58, from_evm_bytes, from_hex, to_base58};
pub use event::{
    BlockRef, ChainContext, HeadState, ParsedTransfer, TokenView, TransactionInfo, TronParseError,
    parse_transfers, to_observation,
};
pub use source::{ScanLane, TronHttpSource, TronSourceConfig, TronSourceError};
