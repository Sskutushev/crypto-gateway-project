//! What the buyer sees on the hosted payment page.
//!
//! The page is opened with an unguessable per-attempt token and shows only
//! what the buyer needs anyway: where to pay, exactly how much, in which
//! token, until when, and what has happened since. Seeing a transfer on the
//! chain is never shown as "paid": paid means settled.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_domain::{FiatAmount, QuoteAsset, RawAmount};
use serde::Serialize;
use time::OffsetDateTime;

use crate::RepositoryError;

/// The buyer-facing state of one payment attempt, most decisive first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutStatus {
    /// Settled: the merchant has been told the order is paid.
    Paid,
    /// Money arrived that a person must look at: late, after a cancellation,
    /// held by policy or risk screening. It is not lost.
    NeedsReview,
    /// Some money arrived, less than the quote.
    Underpaid,
    /// The merchant cancelled the order.
    Cancelled,
    /// A matching transfer is on the chain and not yet final or settled.
    Confirming,
    /// The quote ran out before a payment was seen.
    Expired,
    /// Nothing seen yet; the quote is live.
    Waiting,
}

/// Facts read for one attempt; the status is decided from them here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutFacts {
    pub intent_status: String,
    pub attempt_status: String,
    pub needs_review: bool,
    pub seen_on_chain: bool,
}

impl CheckoutFacts {
    #[must_use]
    pub fn status(&self) -> CheckoutStatus {
        if self.intent_status == "paid" {
            CheckoutStatus::Paid
        } else if self.needs_review {
            CheckoutStatus::NeedsReview
        } else if self.intent_status == "partially_paid" {
            CheckoutStatus::Underpaid
        } else if self.intent_status == "cancelled" || self.attempt_status == "cancelled" {
            CheckoutStatus::Cancelled
        } else if self.seen_on_chain {
            CheckoutStatus::Confirming
        } else if self.attempt_status == "expired" || self.intent_status == "expired" {
            CheckoutStatus::Expired
        } else {
            CheckoutStatus::Waiting
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckoutView {
    pub status: CheckoutStatus,
    pub merchant_name: String,
    pub description: Option<String>,
    pub fiat_amount: FiatAmount,
    pub asset: QuoteAsset,
    /// Where to pay, in the chain's display form.
    pub collector_address: String,
    /// The exact integer to send, and the same value in whole tokens.
    pub amount_raw: RawAmount,
    pub amount: String,
    /// What has been credited to this attempt so far, in whole tokens.
    pub received: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub late_payment_until: OffsetDateTime,
    /// The transaction that paid or is paying this attempt, once known.
    pub transaction_hash: Option<String>,
    /// A public block explorer page for that transaction, where one is known.
    pub explorer_url: Option<String>,
}

#[async_trait]
pub trait CheckoutRepository: Send + Sync {
    async fn checkout_view(&self, token: &str) -> Result<Option<CheckoutView>, RepositoryError>;
}

#[derive(Debug)]
pub struct CheckoutService<R> {
    repository: Arc<R>,
}

impl<R: CheckoutRepository> CheckoutService<R> {
    pub const fn new(repository: Arc<R>) -> Self {
        Self { repository }
    }

    /// The page for a token; `None` for a token that is malformed or unknown,
    /// which are deliberately indistinguishable.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError`] when storage is unavailable.
    pub async fn view(&self, token: &str) -> Result<Option<CheckoutView>, RepositoryError> {
        if !is_checkout_token(token) {
            return Ok(None);
        }
        self.repository.checkout_view(token).await
    }
}

/// 64 lowercase hex characters, as migration 0017 generates them.
#[must_use]
pub fn is_checkout_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::{CheckoutFacts, CheckoutStatus, is_checkout_token};

    fn facts(intent: &str, attempt: &str, needs_review: bool, seen: bool) -> CheckoutFacts {
        CheckoutFacts {
            intent_status: intent.to_owned(),
            attempt_status: attempt.to_owned(),
            needs_review,
            seen_on_chain: seen,
        }
    }

    #[test]
    fn seeing_money_is_never_shown_as_paid() {
        assert_eq!(
            facts("awaiting_payment", "awaiting_payment", false, true).status(),
            CheckoutStatus::Confirming
        );
        assert_eq!(
            facts("paid", "settled", false, true).status(),
            CheckoutStatus::Paid
        );
    }

    #[test]
    fn the_most_decisive_fact_wins() {
        // Money that arrived after a cancellation is under review, not "cancelled".
        assert_eq!(
            facts("cancelled", "cancelled", true, true).status(),
            CheckoutStatus::NeedsReview
        );
        assert_eq!(
            facts("partially_paid", "expired", false, false).status(),
            CheckoutStatus::Underpaid
        );
        assert_eq!(
            facts("cancelled", "cancelled", false, false).status(),
            CheckoutStatus::Cancelled
        );
        assert_eq!(
            facts("expired", "expired", false, false).status(),
            CheckoutStatus::Expired
        );
        assert_eq!(
            facts("expired", "expired", false, true).status(),
            CheckoutStatus::Confirming
        );
        assert_eq!(
            facts("awaiting_payment", "awaiting_payment", false, false).status(),
            CheckoutStatus::Waiting
        );
    }

    #[test]
    fn only_a_well_formed_token_reaches_storage() {
        assert!(is_checkout_token(&"a".repeat(64)));
        assert!(!is_checkout_token(&"A".repeat(64)));
        assert!(!is_checkout_token(&"a".repeat(63)));
        assert!(!is_checkout_token("../../etc/passwd"));
    }
}
