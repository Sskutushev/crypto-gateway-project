//! The TRON rail.
//!
//! This crate holds what must be exactly right before any network call is
//! made: canonical addresses. TRON writes one account four ways — base58check,
//! hex with the `41` prefix, hex without it, and the 20-byte form inside event
//! logs — and comparing any two of them as strings is how money reaches the
//! wrong place. Everything here reduces an address to its canonical bytes and
//! refuses anything whose checksum does not hold.
//!
//! The HTTP source that turns provider answers into observations is the next
//! slice; it will parse transaction logs, not the address-indexed summary,
//! because only the log carries the event identity a canonical fact needs.

pub mod address;

pub use address::{TronAddressError, from_base58, from_evm_bytes, from_hex, to_base58};
