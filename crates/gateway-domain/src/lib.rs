//! Financial and payment domain types.
//!
//! Values crossing the public boundary are represented as decimal strings or
//! integer minor units. This crate intentionally has no floating-point money
//! conversion API.

mod chain;
mod money;
mod payment_intent;
mod quote;
mod verification;

pub use chain::{
    AddressKey, ChainEnvironment, ChainError, ExecutionStatus, Memo, ObservationKind,
    ObservedTransfer, SourceFinality, TransferState, TxHash,
};
pub use money::{CurrencyCode, FiatAmount, MoneyError, RawAmount};
pub use payment_intent::{PaymentIntent, PaymentIntentError, PaymentIntentStatus};
pub use quote::{
    IssuedQuote, PriceSnapshot, QuoteError, QuotePlan, QuotePolicySnapshot, RailHealth,
    RailHealthSnapshot,
};
pub use verification::{
    AttestationRole, CanonicalTransfer, ConflictField, DiscardReason, DiscardedReading,
    EvidenceReading, FieldConflict, FinalityPolicy, InsufficientReason, RejectionReason, Verdict,
    VerificationError, VerifiedTransfer, verify,
};
