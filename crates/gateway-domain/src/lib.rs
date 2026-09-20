//! Financial and payment domain types.
//!
//! Values crossing the public boundary are represented as decimal strings or
//! integer minor units. This crate intentionally has no floating-point money
//! conversion API.

mod money;
mod payment_intent;

pub use money::{CurrencyCode, FiatAmount, MoneyError, RawAmount};
pub use payment_intent::{PaymentIntent, PaymentIntentError, PaymentIntentStatus};
