//! Financial and payment domain types.
//!
//! Values crossing the public boundary are represented as decimal strings or
//! integer minor units. This crate intentionally has no floating-point money
//! conversion API.

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
    ObservedTransfer, SourceFinality, TransferState, TxHash,
};
pub use money::{CurrencyCode, FiatAmount, MoneyError, RawAmount};
pub use payment_intent::{PaymentIntent, PaymentIntentError, PaymentIntentStatus};
pub use price::{
    AggregatedPrice, PriceAggregationError, PriceAggregationPolicy, PriceDiscardReason,
    PriceReading, aggregate,
};
pub use quote::{
    IssuedQuote, PriceSnapshot, QuoteError, QuotePlan, QuotePolicySnapshot, RailHealth,
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
pub use webhook::{SigningSecret, WebhookError, sign_event};
