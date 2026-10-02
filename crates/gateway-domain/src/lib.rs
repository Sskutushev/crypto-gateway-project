//! Financial and payment domain types.
//!
//! Values crossing the public boundary are represented as decimal strings or
//! integer minor units. This crate intentionally has no floating-point money
//! conversion API.

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

mod chain;
mod money;
mod payment_intent;
mod price;
mod quote;
mod settlement;
mod verification;
mod webhook;

pub use chain::{
    AddressKey, ChainEnvironment, ChainError, ExecutionStatus, Memo, ObservationKind,
    ObservedTransfer, SourceFinality, TransferState, TxHash, hex_digit,
};
pub use money::{CurrencyCode, FiatAmount, MoneyError, RawAmount};
pub use payment_intent::{
    MAX_METADATA_BYTES, PaymentIntent, PaymentIntentError, PaymentIntentStatus,
};
pub use price::{
    AggregatedPrice, PriceAggregationError, PriceAggregationPolicy, PriceDiscardReason,
    PriceReading, aggregate,
};
pub use quote::{
    IssuedQuote, PriceSnapshot, QuoteAsset, QuoteError, QuotePlan, QuotePolicySnapshot, RailHealth,
    RailHealthSnapshot,
};
pub use settlement::{
    AttemptCandidate, AttemptStatus, HoldReason, ManualReason, ManualResolutionAction,
    MatchOutcome, MatchStrategy, RemainderDisposition, RiskDecision, SettlementError,
    SettlementEvidence, SettlementOutcome, SettlementPolicy, SettlementTier, TransferFacts,
    decide_settlement, match_transfer,
};
pub use verification::{
    AttestationRole, CanonicalTransfer, ConflictField, DiscardReason, DiscardedReading,
    EvidenceReading, FieldConflict, FinalityPolicy, InsufficientReason, RejectionReason, Verdict,
    VerificationError, VerifiedTransfer, verify,
};
pub use webhook::{SigningSecret, WebhookError, sign_event, sign_event_with_all};
