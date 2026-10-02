use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::FiatAmount;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentIntentStatus {
    RequiresQuote,
    AwaitingPayment,
    PartiallyPaid,
    RiskHold,
    Paid,
    Expired,
    Cancelled,
}

impl PaymentIntentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequiresQuote => "requires_quote",
            Self::AwaitingPayment => "awaiting_payment",
            Self::PartiallyPaid => "partially_paid",
            Self::RiskHold => "risk_hold",
            Self::Paid => "paid",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

impl FromStr for PaymentIntentStatus {
    type Err = PaymentIntentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "requires_quote" => Ok(Self::RequiresQuote),
            "awaiting_payment" => Ok(Self::AwaitingPayment),
            "partially_paid" => Ok(Self::PartiallyPaid),
            "risk_hold" => Ok(Self::RiskHold),
            "paid" => Ok(Self::Paid),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(PaymentIntentError::UnknownStatus(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentIntent {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub amount: FiatAmount,
    pub status: PaymentIntentStatus,
    pub reference: String,
    pub description: Option<String>,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl PaymentIntent {
    /// Creates a new payment intent in the `requires_quote` state.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentIntentError`] when reference, description, or metadata
    /// violates a domain constraint.
    pub fn create(
        merchant_id: Uuid,
        amount: FiatAmount,
        reference: String,
        description: Option<String>,
        metadata: Value,
        now: OffsetDateTime,
    ) -> Result<Self, PaymentIntentError> {
        if reference.is_empty() || reference.len() > 128 {
            return Err(PaymentIntentError::InvalidReference);
        }
        if description.as_ref().is_some_and(|value| value.len() > 500) {
            return Err(PaymentIntentError::DescriptionTooLong);
        }
        if !metadata.is_object() {
            return Err(PaymentIntentError::MetadataMustBeObject);
        }
        // Metadata is stored on every intent and returned on every read; an
        // unbounded object lets one merchant grow the table and every response.
        if metadata.to_string().len() > MAX_METADATA_BYTES {
            return Err(PaymentIntentError::MetadataTooLarge);
        }

        Ok(Self {
            id: Uuid::now_v7(),
            merchant_id,
            amount,
            status: PaymentIntentStatus::RequiresQuote,
            reference,
            description,
            metadata,
            created_at: now,
            updated_at: now,
        })
    }
}

/// Upper bound on serialized merchant metadata.
pub const MAX_METADATA_BYTES: usize = 16_384;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PaymentIntentError {
    #[error("reference must contain between 1 and 128 characters")]
    InvalidReference,
    #[error("description must not exceed 500 characters")]
    DescriptionTooLong,
    #[error("metadata must be a JSON object")]
    MetadataMustBeObject,
    #[error("metadata must not exceed 16384 bytes when serialized")]
    MetadataTooLarge,
    #[error("unknown payment intent status: {0}")]
    UnknownStatus(String),
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::integer_division,
        clippy::string_slice
    )]
    use serde_json::json;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{MAX_METADATA_BYTES, PaymentIntent, PaymentIntentError};
    use crate::{CurrencyCode, FiatAmount};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn create(
        metadata: serde_json::Value,
    ) -> Result<Result<PaymentIntent, PaymentIntentError>, Box<dyn std::error::Error>> {
        let amount = FiatAmount::positive(CurrencyCode::new("USD")?, 1_000)?;
        Ok(PaymentIntent::create(
            Uuid::nil(),
            amount,
            "order-1".to_owned(),
            None,
            metadata,
            OffsetDateTime::UNIX_EPOCH,
        ))
    }

    #[test]
    fn metadata_is_bounded_by_its_serialized_size() -> TestResult {
        // `{"k":"..."}` adds eight bytes around the value.
        let fits = "a".repeat(MAX_METADATA_BYTES - 8);
        assert!(create(json!({ "k": fits }))?.is_ok());
        let over = "a".repeat(MAX_METADATA_BYTES - 7);
        assert_eq!(
            create(json!({ "k": over }))?.err(),
            Some(PaymentIntentError::MetadataTooLarge)
        );
        assert_eq!(
            create(json!([1]))?.err(),
            Some(PaymentIntentError::MetadataMustBeObject)
        );
        Ok(())
    }
}
