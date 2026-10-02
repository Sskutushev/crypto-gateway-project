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

// These crates parse bytes and numbers that arrive from outside: a provider's
// answer, a merchant's signature, a price. An index past the end or an
// overflow here is a panic a stranger can cause, so every slice is checked
// and every operation that can overflow is spelled as the checked, saturating
// or wrapping form it means. A test module relaxes this for its fixtures.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::string_slice
)]

pub mod address;
pub mod event;
pub mod ownership;
pub mod source;
mod telemetry;

pub use address::{TronAddressError, from_base58, from_evm_bytes, from_hex, to_base58};
pub use event::{
    BlockRef, ChainContext, HeadState, ParsedTransfer, TokenView, TransactionInfo, TronParseError,
    parse_transfers, to_observation,
};
pub use ownership::{
    OWNERSHIP_PROOF_TTL, OwnershipError, ownership_statement, recover_signer, verify_ownership,
};
pub use source::{
    ReqwestTransport, ScanLane, TronHttpSource, TronSourceConfig, TronSourceError, TronTransport,
};
