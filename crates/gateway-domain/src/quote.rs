use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{FiatAmount, MoneyError, RawAmount};

const MAX_AMOUNT_SLOT_COUNT: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RailHealth {
    Healthy,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSnapshot {
    pub id: Uuid,
    pub rate_numerator: RawAmount,
    pub rate_denominator: RawAmount,
    pub sources: Value,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotePolicySnapshot {
    pub id: Uuid,
    pub version: String,
    pub quote_ttl_seconds: i64,
    pub late_payment_window_seconds: i64,
    pub amount_slot_count: u32,
    pub max_price_age_seconds: i64,
    pub max_policy_age_seconds: i64,
    pub max_rail_health_age_seconds: i64,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailHealthSnapshot {
    pub id: Uuid,
    pub health: RailHealth,
    pub observed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotePlan {
    quote_id: Uuid,
    attempt_id: Uuid,
    merchant_id: Uuid,
    payment_intent_id: Uuid,
    asset_id: Uuid,
    collector_address_id: Uuid,
    collector_address: String,
    price_snapshot_id: Uuid,
    quote_policy_id: Uuid,
    rail_health_snapshot_id: Uuid,
    fiat_amount: FiatAmount,
    base_amount_raw: RawAmount,
    rate_numerator: RawAmount,
    rate_denominator: RawAmount,
    price_sources: Value,
    price_observed_at: OffsetDateTime,
    policy_version: String,
    policy_observed_at: OffsetDateTime,
    max_price_age_seconds: i64,
    max_policy_age_seconds: i64,
    max_rail_health_age_seconds: i64,
    rail_health_observed_at: OffsetDateTime,
    amount_slot_count: u32,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    late_payment_until: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedQuote {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub payment_intent_id: Uuid,
    pub asset_id: Uuid,
    pub collector_address_id: Uuid,
    pub collector_address: String,
    pub price_snapshot_id: Uuid,
    pub quote_policy_id: Uuid,
    pub rail_health_snapshot_id: Uuid,
    pub fiat_amount: FiatAmount,
    pub amount_raw: RawAmount,
    pub rate_numerator: RawAmount,
    pub rate_denominator: RawAmount,
    pub price_sources: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub price_observed_at: OffsetDateTime,
    pub policy_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub rail_health_observed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub late_payment_until: OffsetDateTime,
}

impl QuotePlan {
    /// Builds an immutable quote plan from fresh, healthy evidence.
    ///
    /// # Errors
    ///
    /// Fails closed when any evidence is missing, stale, from the future, or
    /// unhealthy, or when policy durations and source evidence are invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        merchant_id: Uuid,
        payment_intent_id: Uuid,
        asset_id: Uuid,
        collector_address_id: Uuid,
        collector_address: String,
        fiat_amount: FiatAmount,
        price: Option<PriceSnapshot>,
        policy: Option<QuotePolicySnapshot>,
        rail: Option<RailHealthSnapshot>,
        now: OffsetDateTime,
    ) -> Result<Self, QuoteError> {
        let price = price.ok_or(QuoteError::UnknownPrice)?;
        let policy = policy.ok_or(QuoteError::UnknownPolicy)?;
        let rail = rail.ok_or(QuoteError::UnknownRailHealth)?;

        validate_policy(&policy)?;
        validate_age(now, price.observed_at, policy.max_price_age_seconds)
            .map_err(|()| QuoteError::StalePrice)?;
        validate_age(now, policy.observed_at, policy.max_policy_age_seconds)
            .map_err(|()| QuoteError::StalePolicy)?;
        validate_age(now, rail.observed_at, policy.max_rail_health_age_seconds)
            .map_err(|()| QuoteError::StaleRailHealth)?;
        if rail.health != RailHealth::Healthy {
            return Err(QuoteError::UnhealthyRail);
        }
        if collector_address.is_empty() || collector_address.len() > 200 {
            return Err(QuoteError::InvalidCollectorAddress);
        }
        if !has_independent_price_sources(&price.sources) {
            return Err(QuoteError::InvalidPriceSources);
        }

        let fiat_minor =
            u64::try_from(fiat_amount.minor_units).map_err(|_| QuoteError::InvalidPolicy)?;
        let base_amount_raw =
            RawAmount::mul_div_ceil(fiat_minor, price.rate_numerator, price.rate_denominator)?;
        let expires_at = now
            .checked_add(Duration::seconds(policy.quote_ttl_seconds))
            .ok_or(QuoteError::InvalidPolicy)?;
        let late_payment_until = expires_at
            .checked_add(Duration::seconds(policy.late_payment_window_seconds))
            .ok_or(QuoteError::InvalidPolicy)?;

        Ok(Self {
            quote_id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            merchant_id,
            payment_intent_id,
            asset_id,
            collector_address_id,
            collector_address,
            price_snapshot_id: price.id,
            quote_policy_id: policy.id,
            rail_health_snapshot_id: rail.id,
            fiat_amount,
            base_amount_raw,
            rate_numerator: price.rate_numerator,
            rate_denominator: price.rate_denominator,
            price_sources: price.sources,
            price_observed_at: price.observed_at,
            policy_version: policy.version,
            policy_observed_at: policy.observed_at,
            max_price_age_seconds: policy.max_price_age_seconds,
            max_policy_age_seconds: policy.max_policy_age_seconds,
            max_rail_health_age_seconds: policy.max_rail_health_age_seconds,
            rail_health_observed_at: rail.observed_at,
            amount_slot_count: policy.amount_slot_count,
            created_at: now,
            expires_at,
            late_payment_until,
        })
    }

    #[must_use]
    pub const fn quote_id(&self) -> Uuid {
        self.quote_id
    }
    #[must_use]
    pub const fn attempt_id(&self) -> Uuid {
        self.attempt_id
    }
    #[must_use]
    pub const fn merchant_id(&self) -> Uuid {
        self.merchant_id
    }
    #[must_use]
    pub const fn payment_intent_id(&self) -> Uuid {
        self.payment_intent_id
    }
    #[must_use]
    pub const fn asset_id(&self) -> Uuid {
        self.asset_id
    }
    #[must_use]
    pub const fn collector_address_id(&self) -> Uuid {
        self.collector_address_id
    }
    #[must_use]
    pub fn collector_address(&self) -> &str {
        &self.collector_address
    }
    #[must_use]
    pub const fn price_snapshot_id(&self) -> Uuid {
        self.price_snapshot_id
    }
    #[must_use]
    pub const fn quote_policy_id(&self) -> Uuid {
        self.quote_policy_id
    }
    #[must_use]
    pub const fn rail_health_snapshot_id(&self) -> Uuid {
        self.rail_health_snapshot_id
    }
    #[must_use]
    pub const fn fiat_amount(&self) -> &FiatAmount {
        &self.fiat_amount
    }
    #[must_use]
    pub const fn base_amount_raw(&self) -> RawAmount {
        self.base_amount_raw
    }
    #[must_use]
    pub const fn rate_numerator(&self) -> RawAmount {
        self.rate_numerator
    }
    #[must_use]
    pub const fn rate_denominator(&self) -> RawAmount {
        self.rate_denominator
    }
    #[must_use]
    pub const fn price_sources(&self) -> &Value {
        &self.price_sources
    }
    #[must_use]
    pub const fn price_observed_at(&self) -> OffsetDateTime {
        self.price_observed_at
    }
    #[must_use]
    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }
    #[must_use]
    pub const fn policy_observed_at(&self) -> OffsetDateTime {
        self.policy_observed_at
    }
    #[must_use]
    pub const fn max_price_age_seconds(&self) -> i64 {
        self.max_price_age_seconds
    }
    #[must_use]
    pub const fn max_policy_age_seconds(&self) -> i64 {
        self.max_policy_age_seconds
    }
    #[must_use]
    pub const fn max_rail_health_age_seconds(&self) -> i64 {
        self.max_rail_health_age_seconds
    }
    #[must_use]
    pub const fn rail_health_observed_at(&self) -> OffsetDateTime {
        self.rail_health_observed_at
    }
    #[must_use]
    pub const fn amount_slot_count(&self) -> u32 {
        self.amount_slot_count
    }
    #[must_use]
    pub const fn created_at(&self) -> OffsetDateTime {
        self.created_at
    }
    #[must_use]
    pub const fn expires_at(&self) -> OffsetDateTime {
        self.expires_at
    }
    #[must_use]
    pub const fn late_payment_until(&self) -> OffsetDateTime {
        self.late_payment_until
    }
}

fn has_independent_price_sources(sources: &Value) -> bool {
    let Some(sources) = sources.as_array() else {
        return false;
    };
    if sources.len() < 2 {
        return false;
    }
    let groups = sources
        .iter()
        .filter_map(|source| source.get("provider_group")?.as_str())
        .filter(|group| !group.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    groups.len() >= 2 && groups.len() == sources.len()
}

fn validate_policy(policy: &QuotePolicySnapshot) -> Result<(), QuoteError> {
    let valid = !policy.version.is_empty()
        && policy.version.len() <= 100
        && policy.quote_ttl_seconds > 0
        && policy.late_payment_window_seconds > 0
        && (1..=MAX_AMOUNT_SLOT_COUNT).contains(&policy.amount_slot_count)
        && policy.max_price_age_seconds >= 0
        && policy.max_policy_age_seconds >= 0
        && policy.max_rail_health_age_seconds >= 0;
    if !valid {
        return Err(QuoteError::InvalidPolicy);
    }
    Ok(())
}

fn validate_age(
    now: OffsetDateTime,
    observed_at: OffsetDateTime,
    max_age_seconds: i64,
) -> Result<(), ()> {
    let age = now - observed_at;
    if age.is_negative() || age > Duration::seconds(max_age_seconds) {
        return Err(());
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum QuoteError {
    #[error("price is unknown")]
    UnknownPrice,
    #[error("quote policy is unknown")]
    UnknownPolicy,
    #[error("rail health is unknown")]
    UnknownRailHealth,
    #[error("price is stale or future-dated")]
    StalePrice,
    #[error("quote policy is stale or future-dated")]
    StalePolicy,
    #[error("rail health is stale or future-dated")]
    StaleRailHealth,
    #[error("rail is not healthy enough for a new quote")]
    UnhealthyRail,
    #[error("price sources must name at least two distinct provider groups")]
    InvalidPriceSources,
    #[error("collector address must contain between 1 and 200 characters")]
    InvalidCollectorAddress,
    #[error("quote policy is invalid")]
    InvalidPolicy,
    #[error(transparent)]
    Money(#[from] MoneyError),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::json;

    use super::*;
    use crate::CurrencyCode;

    fn valid_inputs(
        now: OffsetDateTime,
    ) -> Result<(PriceSnapshot, QuotePolicySnapshot, RailHealthSnapshot), MoneyError> {
        Ok((
            PriceSnapshot {
                id: Uuid::from_u128(1),
                rate_numerator: RawAmount::from_str("5")?,
                rate_denominator: RawAmount::from_str("2")?,
                sources: json!([
                    {"provider_group":"a","observed_at":"1970-01-01T00:00:00Z"},
                    {"provider_group":"b","observed_at":"1970-01-01T00:00:00Z"}
                ]),
                observed_at: now,
            },
            QuotePolicySnapshot {
                id: Uuid::from_u128(2),
                version: "policy-v1".to_owned(),
                quote_ttl_seconds: 900,
                late_payment_window_seconds: 2_592_000,
                amount_slot_count: 10_000,
                max_price_age_seconds: 60,
                max_policy_age_seconds: 300,
                max_rail_health_age_seconds: 60,
                observed_at: now,
            },
            RailHealthSnapshot {
                id: Uuid::from_u128(3),
                health: RailHealth::Healthy,
                observed_at: now,
            },
        ))
    }

    fn build_with(
        now: OffsetDateTime,
        price: Option<PriceSnapshot>,
        policy: Option<QuotePolicySnapshot>,
        rail: Option<RailHealthSnapshot>,
    ) -> Result<QuotePlan, QuoteError> {
        QuotePlan::build(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            "TCollector".to_owned(),
            FiatAmount::positive(CurrencyCode::new("USD").map_err(QuoteError::from)?, 1)
                .map_err(QuoteError::from)?,
            price,
            policy,
            rail,
            now,
        )
    }

    #[test]
    fn builds_separate_quote_and_late_payment_times() -> Result<(), Box<dyn std::error::Error>> {
        let now = OffsetDateTime::UNIX_EPOCH;
        let (price, policy, rail) = valid_inputs(now)?;
        let plan = QuotePlan::build(
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            Uuid::nil(),
            "TCollector".to_owned(),
            FiatAmount::positive(CurrencyCode::new("USD")?, 3)?,
            Some(price),
            Some(policy),
            Some(rail),
            now,
        )?;

        assert_eq!(plan.base_amount_raw.to_string(), "8");
        assert_eq!(plan.expires_at, now + Duration::seconds(900));
        assert_eq!(
            plan.late_payment_until,
            plan.expires_at + Duration::days(30)
        );
        Ok(())
    }

    #[test]
    fn fails_closed_for_missing_stale_or_unhealthy_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::hours(1);
        let (price, policy, rail) = valid_inputs(now)?;
        assert_eq!(
            build_with(now, None, Some(policy.clone()), Some(rail.clone())),
            Err(QuoteError::UnknownPrice)
        );
        assert_eq!(
            build_with(now, Some(price.clone()), None, Some(rail.clone())),
            Err(QuoteError::UnknownPolicy)
        );
        assert_eq!(
            build_with(now, Some(price.clone()), Some(policy.clone()), None),
            Err(QuoteError::UnknownRailHealth)
        );

        let mut stale_price = price.clone();
        stale_price.observed_at = now - Duration::seconds(61);
        assert_eq!(
            build_with(
                now,
                Some(stale_price),
                Some(policy.clone()),
                Some(rail.clone())
            ),
            Err(QuoteError::StalePrice)
        );
        let mut stale_policy = policy.clone();
        stale_policy.observed_at = now - Duration::seconds(301);
        assert_eq!(
            build_with(
                now,
                Some(price.clone()),
                Some(stale_policy),
                Some(rail.clone())
            ),
            Err(QuoteError::StalePolicy)
        );
        let mut stale_rail = rail.clone();
        stale_rail.observed_at = now - Duration::seconds(61);
        assert_eq!(
            build_with(
                now,
                Some(price.clone()),
                Some(policy.clone()),
                Some(stale_rail)
            ),
            Err(QuoteError::StaleRailHealth)
        );
        let healthy_rail = rail.clone();
        let mut unhealthy_rail = rail;
        unhealthy_rail.health = RailHealth::Degraded;
        assert_eq!(
            build_with(
                now,
                Some(price.clone()),
                Some(policy.clone()),
                Some(unhealthy_rail)
            ),
            Err(QuoteError::UnhealthyRail)
        );
        let mut unbounded_policy = policy;
        unbounded_policy.amount_slot_count = 10_001;
        assert_eq!(
            build_with(now, Some(price), Some(unbounded_policy), Some(healthy_rail)),
            Err(QuoteError::InvalidPolicy)
        );
        let (mut one_source, policy, rail) = valid_inputs(now)?;
        one_source.sources = json!([{"provider_group":"only-source"}]);
        assert_eq!(
            build_with(now, Some(one_source), Some(policy), Some(rail)),
            Err(QuoteError::InvalidPriceSources)
        );
        Ok(())
    }
}
