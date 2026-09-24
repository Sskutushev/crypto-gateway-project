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
    /// The empty quantity.
    ///
    /// Running totals such as "allocated so far" start here. A payable amount
    /// never does, which is why every parsing constructor rejects zero.
    pub const ZERO: Self = Self(U256::ZERO);

    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }

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

/// Seeded property tests over the money parsers.
///
/// They stand in for a fuzzer: the pinned stable toolchain has no `cargo
/// fuzz`, so a deterministic generator drives the same parsers through many
/// thousands of inputs, in CI with `GATEWAY_FUZZ_ITERATIONS` raised. A panic
/// anywhere is the finding; the invariants are asserted on top.
#[cfg(test)]
mod fuzz_smoke {
    use std::str::FromStr;

    use super::{CurrencyCode, FiatAmount, MoneyError, RawAmount};

    /// xorshift64*: enough randomness to walk the input space, and a fixed
    /// seed so a failure reproduces from the iteration number alone.
    struct Generator(u64);

    impl Generator {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }

        fn digits(&mut self, length: usize) -> String {
            (0..length)
                .map(|_| char::from(b'0' + u8::try_from(self.below(10)).unwrap_or(0)))
                .collect()
        }

        fn bytes(&mut self, length: usize) -> Vec<u8> {
            (0..length)
                .map(|_| u8::try_from(self.below(256)).unwrap_or(0))
                .collect()
        }
    }

    fn iterations() -> u64 {
        std::env::var("GATEWAY_FUZZ_ITERATIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2_000)
    }

    #[test]
    fn raw_amount_round_trips_every_digit_string_or_refuses_it() -> Result<(), MoneyError> {
        let mut generator = Generator(0x9E37_79B9_7F4A_7C15);
        for _ in 0..iterations() {
            let length = usize::try_from(generator.below(90)).unwrap_or(0) + 1;
            let text = generator.digits(length);
            let parsed = RawAmount::from_str(&text);
            let stripped = text.trim_start_matches('0');
            if stripped.is_empty() {
                assert_eq!(parsed, Err(MoneyError::AmountMustBePositive), "{text}");
            } else if stripped.len() > 78 {
                assert!(parsed.is_err(), "{text} exceeds 256 bits and was accepted");
            } else if let Ok(amount) = parsed {
                // Leading zeros are dropped and nothing else changes.
                assert_eq!(amount.to_string(), stripped, "{text}");
                assert_eq!(RawAmount::from_str(&amount.to_string())?, amount);
            } else {
                // 78 digits may still overflow 2^256 - 1; anything shorter
                // must parse.
                assert_eq!(stripped.len(), 78, "{text} was refused");
            }
        }
        Ok(())
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_parsers() {
        let mut generator = Generator(0xD1B5_4A32_D192_ED03);
        for _ in 0..iterations() {
            let length = usize::try_from(generator.below(40)).unwrap_or(0);
            let bytes = generator.bytes(length);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let raw = RawAmount::from_str(&text);
            let fiat =
                CurrencyCode::new("USD").and_then(|usd| FiatAmount::parse_positive(usd, &text));
            let all_digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
            if !all_digits {
                assert!(raw.is_err(), "{text:?} is not digits and was accepted");
                assert!(fiat.is_err(), "{text:?} is not digits and was accepted");
            }
            let code = CurrencyCode::new(&text);
            if let Ok(code) = code {
                assert_eq!(code.as_str().len(), 3);
                assert!(code.as_str().bytes().all(|byte| byte.is_ascii_uppercase()));
            }
        }
    }

    #[test]
    fn fiat_minor_units_stay_inside_the_signed_range() {
        let mut generator = Generator(0x0123_4567_89AB_CDEF);
        for _ in 0..iterations() {
            let length = usize::try_from(generator.below(25)).unwrap_or(0) + 1;
            let text = generator.digits(length);
            let Ok(usd) = CurrencyCode::new("USD") else {
                unreachable!("USD is a currency code");
            };
            match FiatAmount::parse_positive(usd, &text) {
                Ok(amount) => {
                    assert!(amount.minor_units > 0, "{text}");
                    assert_eq!(text.trim_start_matches('0'), amount.minor_units.to_string());
                }
                Err(error) => {
                    let value = text.parse::<i128>().unwrap_or(i128::MAX);
                    assert!(
                        value == 0 || value > i128::from(i64::MAX),
                        "{text} was refused with {error}"
                    );
                }
            }
        }
    }
}
