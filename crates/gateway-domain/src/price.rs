//! Turning several sources' opinions about a price into one usable rate.
//!
//! A rate is a ratio of integers from end to end. Nothing here divides into a
//! float, rounds to a decimal, or averages by converting to something smaller:
//! two rates are compared by cross-multiplication, and the middle of two rates
//! is their exact rational mean.
//!
//! The rules are the ones that keep a wrong price from becoming a wrong
//! payment. Independence is counted by provider group, because two API keys of
//! one vendor are one opinion. Sources that disagree by more than the policy
//! allows do not average into a plausible-looking middle: the disagreement is
//! the answer, and the rail closes rather than quoting through it.

use alloy_primitives::U256;
use thiserror::Error;
use time::OffsetDateTime;

use crate::RawAmount;

/// Basis points in one whole. A deviation ceiling is expressed in these.
const BPS_SCALE: u64 = 10_000;

/// What one source said, and when it said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceReading {
    pub source_key: String,
    /// Independence is counted by this, never by `source_key`.
    pub provider_group: String,
    pub rate_numerator: RawAmount,
    pub rate_denominator: RawAmount,
    pub observed_at: OffsetDateTime,
}

/// What the policy demands of the evidence behind a price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceAggregationPolicy {
    pub min_sources: u32,
    pub max_age_seconds: i64,
    pub max_deviation_bps: u32,
}

/// Why one reading did not count towards the rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceDiscardReason {
    /// Older than the policy allows.
    Stale,
    /// Dated after the moment the aggregate was computed.
    FutureDated,
    /// Another reading from the same provider group was newer. Two keys of one
    /// vendor are one opinion, and the fresher one is that opinion.
    SupersededInGroup,
}

impl PriceDiscardReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::FutureDated => "future_dated",
            Self::SupersededInGroup => "superseded_in_group",
        }
    }
}

/// The rate, and the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatedPrice {
    pub rate_numerator: RawAmount,
    pub rate_denominator: RawAmount,
    /// Independent provider groups that agreed closely enough to be used.
    pub group_count: u32,
    /// The spread between the lowest and the highest reading that was used.
    pub deviation_bps: u32,
    /// Indices into the submitted readings that the rate rests on.
    pub used: Vec<usize>,
    /// Indices that did not count, each with the reason it did not.
    pub discarded: Vec<(usize, PriceDiscardReason)>,
}

/// Combines readings into one rate, or refuses to.
///
/// # Errors
///
/// Returns [`PriceAggregationError`] when too few independent groups remain,
/// when the readings that remain disagree by more than the policy allows, or
/// when the arithmetic would exceed 256 bits.
pub fn aggregate(
    readings: &[PriceReading],
    policy: PriceAggregationPolicy,
    now: OffsetDateTime,
) -> Result<AggregatedPrice, PriceAggregationError> {
    if policy.min_sources < 2 {
        return Err(PriceAggregationError::PolicyDemandsTooFewSources);
    }
    let mut discarded = Vec::new();
    let mut fresh: Vec<(usize, &PriceReading)> = Vec::new();
    for (index, reading) in readings.iter().enumerate() {
        if reading.observed_at > now {
            discarded.push((index, PriceDiscardReason::FutureDated));
            continue;
        }
        let age = (now - reading.observed_at).whole_seconds();
        if age > policy.max_age_seconds {
            discarded.push((index, PriceDiscardReason::Stale));
            continue;
        }
        fresh.push((index, reading));
    }

    // One opinion per provider group: the newest reading of a group speaks for
    // it, and the rest are recorded as superseded rather than dropped.
    let mut per_group: Vec<(usize, &PriceReading)> = Vec::new();
    for (index, reading) in fresh {
        match per_group
            .iter_mut()
            .find(|(_, kept)| kept.provider_group == reading.provider_group)
        {
            Some(slot) => {
                if reading.observed_at > slot.1.observed_at {
                    discarded.push((slot.0, PriceDiscardReason::SupersededInGroup));
                    *slot = (index, reading);
                } else {
                    discarded.push((index, PriceDiscardReason::SupersededInGroup));
                }
            }
            None => per_group.push((index, reading)),
        }
    }

    let group_count = u32::try_from(per_group.len()).unwrap_or(u32::MAX);
    if group_count < policy.min_sources {
        return Err(PriceAggregationError::NotEnoughSources {
            required: policy.min_sources,
            available: group_count,
        });
    }

    per_group.sort_by(|left, right| compare(left.1, right.1));
    let lowest = per_group
        .first()
        .ok_or(PriceAggregationError::NoReadings)?
        .1;
    let highest = per_group.last().ok_or(PriceAggregationError::NoReadings)?.1;
    let deviation_bps = deviation_bps(lowest, highest)?;
    if deviation_bps > policy.max_deviation_bps {
        return Err(PriceAggregationError::Diverged {
            deviation_bps,
            ceiling: policy.max_deviation_bps,
        });
    }

    let (rate_numerator, rate_denominator) = median(&per_group)?;
    Ok(AggregatedPrice {
        rate_numerator,
        rate_denominator,
        group_count,
        deviation_bps,
        used: per_group.iter().map(|(index, _)| *index).collect(),
        discarded,
    })
}

/// Orders two rates without leaving the integers: `a/b` against `c/d` is
/// `a*d` against `c*b`.
fn compare(left: &PriceReading, right: &PriceReading) -> std::cmp::Ordering {
    let left_side = left
        .rate_numerator
        .as_u256()
        .checked_mul(right.rate_denominator.as_u256());
    let right_side = right
        .rate_numerator
        .as_u256()
        .checked_mul(left.rate_denominator.as_u256());
    match (left_side, right_side) {
        (Some(left_side), Some(right_side)) => left_side.cmp(&right_side),
        // An overflow cannot order the pair. The caller reports it separately;
        // here the pair is treated as equal so the sort stays well defined.
        _ => std::cmp::Ordering::Equal,
    }
}

/// The spread between the lowest and highest rate, rounded up.
///
/// Rounding up is deliberate: a ceiling that is exceeded by a fraction of a
/// basis point has been exceeded.
fn deviation_bps(
    lowest: &PriceReading,
    highest: &PriceReading,
) -> Result<u32, PriceAggregationError> {
    let low_numerator = lowest.rate_numerator.as_u256();
    let low_denominator = lowest.rate_denominator.as_u256();
    let high_numerator = highest.rate_numerator.as_u256();
    let high_denominator = highest.rate_denominator.as_u256();

    let scaled_high = high_numerator
        .checked_mul(low_denominator)
        .ok_or(PriceAggregationError::Overflow)?;
    let scaled_low = low_numerator
        .checked_mul(high_denominator)
        .ok_or(PriceAggregationError::Overflow)?;
    let difference = scaled_high.saturating_sub(scaled_low);
    if difference.is_zero() {
        return Ok(0);
    }
    let numerator = difference
        .checked_mul(U256::from(BPS_SCALE))
        .ok_or(PriceAggregationError::Overflow)?;
    let quotient = numerator / scaled_low;
    let remainder = numerator % scaled_low;
    let rounded = if remainder.is_zero() {
        quotient
    } else {
        quotient
            .checked_add(U256::from(1_u8))
            .ok_or(PriceAggregationError::Overflow)?
    };
    u32::try_from(rounded).map_err(|_| PriceAggregationError::Overflow)
}

/// The middle rate. With an even number of readings it is the exact rational
/// mean of the two middle ones, which needs no rounding and favours neither
/// side.
fn median(
    sorted: &[(usize, &PriceReading)],
) -> Result<(RawAmount, RawAmount), PriceAggregationError> {
    let count = sorted.len();
    if count == 0 {
        return Err(PriceAggregationError::NoReadings);
    }
    if count % 2 == 1 {
        let middle = sorted
            .get(count / 2)
            .ok_or(PriceAggregationError::NoReadings)?
            .1;
        return Ok((middle.rate_numerator, middle.rate_denominator));
    }
    let left = sorted
        .get(count / 2 - 1)
        .ok_or(PriceAggregationError::NoReadings)?
        .1;
    let right = sorted
        .get(count / 2)
        .ok_or(PriceAggregationError::NoReadings)?
        .1;

    let left_cross = left
        .rate_numerator
        .as_u256()
        .checked_mul(right.rate_denominator.as_u256())
        .ok_or(PriceAggregationError::Overflow)?;
    let right_cross = right
        .rate_numerator
        .as_u256()
        .checked_mul(left.rate_denominator.as_u256())
        .ok_or(PriceAggregationError::Overflow)?;
    let numerator = left_cross
        .checked_add(right_cross)
        .ok_or(PriceAggregationError::Overflow)?;
    let denominator = left
        .rate_denominator
        .as_u256()
        .checked_mul(right.rate_denominator.as_u256())
        .and_then(|product| product.checked_mul(U256::from(2_u8)))
        .ok_or(PriceAggregationError::Overflow)?;

    let divisor = greatest_common_divisor(numerator, denominator);
    let numerator = numerator / divisor;
    let denominator = denominator / divisor;
    Ok((
        RawAmount::positive(numerator).map_err(|_| PriceAggregationError::Overflow)?,
        RawAmount::positive(denominator).map_err(|_| PriceAggregationError::Overflow)?,
    ))
}

/// Keeps the mean's terms as small as the mean allows, so repeated averaging
/// cannot walk a rate towards the 256-bit ceiling.
fn greatest_common_divisor(left: U256, right: U256) -> U256 {
    let mut left = left;
    let mut right = right;
    while !right.is_zero() {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    if left.is_zero() {
        U256::from(1_u8)
    } else {
        left
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PriceAggregationError {
    #[error("a price needs at least two independent sources by policy")]
    PolicyDemandsTooFewSources,
    #[error("no reading was supplied")]
    NoReadings,
    #[error("{available} independent source groups are usable, policy requires {required}")]
    NotEnoughSources { required: u32, available: u32 },
    #[error("sources disagree by {deviation_bps} basis points, policy allows {ceiling}")]
    Diverged { deviation_bps: u32, ceiling: u32 },
    #[error("the rate arithmetic does not fit in 256 bits")]
    Overflow,
}

#[cfg(test)]
mod tests;
