use std::{fmt, str::FromStr};

use alloy_primitives::U256;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CurrencyCode(String);

impl CurrencyCode {
    /// Creates a normalized three-letter currency code.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::InvalidCurrencyCode`] unless the input contains
    /// exactly three ASCII letters.
    pub fn new(value: impl Into<String>) -> Result<Self, MoneyError> {
        let value = value.into().to_ascii_uppercase();
        let valid = value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase());
        if !valid {
            return Err(MoneyError::InvalidCurrencyCode);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for CurrencyCode {
    type Err = MoneyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for CurrencyCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiatAmount {
    pub currency: CurrencyCode,
    #[serde(with = "decimal_i64")]
    pub minor_units: i64,
}

impl FiatAmount {
    /// Creates a strictly positive fiat amount in minor units.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::AmountMustBePositive`] for zero or negative input.
    pub fn positive(currency: CurrencyCode, minor_units: i64) -> Result<Self, MoneyError> {
        if minor_units <= 0 {
            return Err(MoneyError::AmountMustBePositive);
        }
        Ok(Self {
            currency,
            minor_units,
        })
    }

    /// Parses a strictly positive base-10 integer amount from a public API
    /// string.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::InvalidFiatAmount`] for signs, decimals,
    /// whitespace, or values outside the signed 64-bit range, and
    /// [`MoneyError::AmountMustBePositive`] for zero.
    pub fn parse_positive(currency: CurrencyCode, minor_units: &str) -> Result<Self, MoneyError> {
        if minor_units.is_empty() || !minor_units.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(MoneyError::InvalidFiatAmount);
        }
        let minor_units = minor_units
            .parse::<i64>()
            .map_err(|_| MoneyError::InvalidFiatAmount)?;
        Self::positive(currency, minor_units)
    }
}

mod decimal_i64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    // Serde's `serialize_with` contract requires a reference even for Copy
    // scalar fields.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S>(value: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse::<i64>().map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RawAmount(U256);

impl RawAmount {
    /// Creates a strictly positive raw token amount.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::AmountMustBePositive`] for zero.
    pub fn positive(value: U256) -> Result<Self, MoneyError> {
        if value.is_zero() {
            return Err(MoneyError::AmountMustBePositive);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn as_u256(self) -> U256 {
        self.0
    }

    /// Adds two raw token quantities without wrapping.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::RawAmountOverflow`] when the sum exceeds 256 bits.
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MoneyError::RawAmountOverflow)
    }

    /// Adds a small allocation-slot offset without wrapping.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::RawAmountOverflow`] when the sum exceeds 256 bits.
    pub fn checked_add_u32(self, other: u32) -> Result<Self, MoneyError> {
        self.0
            .checked_add(U256::from(other))
            .map(Self)
            .ok_or(MoneyError::RawAmountOverflow)
    }

    /// Multiplies and divides raw integers, rounding any remainder upward.
    ///
    /// # Errors
    ///
    /// Returns an error for zero denominators, overflow, or a zero result.
    pub fn mul_div_ceil(
        multiplier: u64,
        numerator: Self,
        denominator: Self,
    ) -> Result<Self, MoneyError> {
        let product = U256::from(multiplier)
            .checked_mul(numerator.0)
            .ok_or(MoneyError::RawAmountOverflow)?;
        let quotient = product / denominator.0;
        let remainder = product % denominator.0;
        let rounded = if remainder.is_zero() {
            quotient
        } else {
            quotient
                .checked_add(U256::from(1_u8))
                .ok_or(MoneyError::RawAmountOverflow)?
        };
        Self::positive(rounded)
    }
}

impl fmt::Display for RawAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for RawAmount {
    type Err = MoneyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(MoneyError::InvalidRawAmount);
        }
        let parsed = U256::from_str(value).map_err(|_| MoneyError::InvalidRawAmount)?;
        Self::positive(parsed)
    }
}

impl Serialize for RawAmount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RawAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MoneyError {
    #[error("currency must be exactly three ASCII letters")]
    InvalidCurrencyCode,
    #[error("amount must be greater than zero")]
    AmountMustBePositive,
    #[error("fiat amount must be a base-10 positive signed 64-bit integer string")]
    InvalidFiatAmount,
    #[error("raw amount must be a base-10 unsigned 256-bit integer string")]
    InvalidRawAmount,
    #[error("raw amount arithmetic overflowed 256 bits")]
    RawAmountOverflow,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{CurrencyCode, FiatAmount, MoneyError, RawAmount};

    #[test]
    fn currency_is_normalized_without_accepting_invalid_input() -> Result<(), MoneyError> {
        let currency = CurrencyCode::new("usd")?;
        assert_eq!(currency.as_str(), "USD");
        assert_eq!(
            CurrencyCode::new("USDT"),
            Err(MoneyError::InvalidCurrencyCode)
        );
        assert_eq!(
            CurrencyCode::new("U1D"),
            Err(MoneyError::InvalidCurrencyCode)
        );
        Ok(())
    }

    #[test]
    fn raw_amount_round_trips_at_u256_max() -> Result<(), MoneyError> {
        let value =
            "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        let amount = RawAmount::from_str(value)?;
        assert_eq!(amount.to_string(), value);
        Ok(())
    }

    #[test]
    fn raw_amount_rejects_zero_signs_and_decimals() {
        for value in ["0", "-1", "+1", "1.0", " 1"] {
            assert!(RawAmount::from_str(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn fiat_amount_uses_decimal_strings_at_the_json_boundary() -> Result<(), MoneyError> {
        let amount = FiatAmount::parse_positive(CurrencyCode::new("USD")?, "12345")?;
        let json = serde_json::to_value(&amount).map_err(|_| MoneyError::InvalidFiatAmount)?;

        assert_eq!(json["minor_units"], "12345");
        assert!(
            serde_json::from_str::<FiatAmount>(r#"{"currency":"USD","minor_units":12345}"#)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn fiat_amount_rejects_non_canonical_public_values() -> Result<(), MoneyError> {
        let currency = CurrencyCode::new("USD")?;
        for value in ["", "-1", "+1", "1.0", " 1", "9223372036854775808"] {
            assert!(
                FiatAmount::parse_positive(currency.clone(), value).is_err(),
                "accepted {value}"
            );
        }
        Ok(())
    }

    #[test]
    fn rational_conversion_rounds_up_without_floating_point() -> Result<(), MoneyError> {
        let numerator = RawAmount::from_str("5")?;
        let denominator = RawAmount::from_str("2")?;

        assert_eq!(
            RawAmount::mul_div_ceil(3, numerator, denominator)?.to_string(),
            "8"
        );
        assert_eq!(
            RawAmount::mul_div_ceil(4, numerator, denominator)?.to_string(),
            "10"
        );
        Ok(())
    }
}
