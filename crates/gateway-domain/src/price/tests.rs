use std::{error::Error, str::FromStr};

use time::{Duration, OffsetDateTime};

use super::{
    PriceAggregationError, PriceAggregationPolicy, PriceDiscardReason, PriceReading, aggregate,
};
use crate::RawAmount;

type TestResult = Result<(), Box<dyn Error>>;

fn now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
}

fn policy() -> PriceAggregationPolicy {
    PriceAggregationPolicy {
        min_sources: 2,
        max_age_seconds: 300,
        max_deviation_bps: 200,
    }
}

fn reading(
    group: &str,
    numerator: &str,
    denominator: &str,
    age_seconds: i64,
) -> Result<PriceReading, Box<dyn Error>> {
    Ok(PriceReading {
        source_key: format!("{group}-key"),
        provider_group: group.to_owned(),
        rate_numerator: RawAmount::from_str(numerator)?,
        rate_denominator: RawAmount::from_str(denominator)?,
        observed_at: now() - Duration::seconds(age_seconds),
    })
}

#[test]
fn the_middle_of_two_agreeing_sources_is_their_exact_mean() -> TestResult {
    // 0.272700 and 0.272900 USDT per fils: the mean is 0.2728 exactly, it is
    // reached without a decimal anywhere, and it is kept in lowest terms so
    // repeated averaging cannot walk the terms towards the 256-bit ceiling.
    let readings = vec![
        reading("alpha", "2727", "10000", 10)?,
        reading("beta", "2729", "10000", 10)?,
    ];

    let aggregated = aggregate(&readings, policy(), now())?;

    assert_eq!(aggregated.rate_numerator.to_string(), "341");
    assert_eq!(aggregated.rate_denominator.to_string(), "1250");
    assert_eq!(aggregated.group_count, 2);
    assert_eq!(aggregated.used.len(), 2);
    assert!(aggregated.discarded.is_empty());
    Ok(())
}

#[test]
fn an_odd_number_of_sources_takes_the_middle_one() -> TestResult {
    let readings = vec![
        reading("alpha", "2729", "10000", 5)?,
        reading("beta", "2727", "10000", 5)?,
        reading("gamma", "2728", "10000", 5)?,
    ];

    let aggregated = aggregate(&readings, policy(), now())?;

    assert_eq!(aggregated.rate_numerator.to_string(), "2728");
    assert_eq!(aggregated.rate_denominator.to_string(), "10000");
    assert_eq!(aggregated.group_count, 3);
    Ok(())
}

#[test]
fn two_keys_of_one_vendor_are_one_opinion() -> TestResult {
    let readings = vec![
        PriceReading {
            source_key: "alpha-key-one".to_owned(),
            ..reading("alpha", "2727", "10000", 60)?
        },
        PriceReading {
            source_key: "alpha-key-two".to_owned(),
            ..reading("alpha", "2728", "10000", 10)?
        },
    ];

    let outcome = aggregate(&readings, policy(), now());

    assert_eq!(
        outcome.err(),
        Some(PriceAggregationError::NotEnoughSources {
            required: 2,
            available: 1
        }),
        "one vendor answering twice is not two independent sources"
    );
    Ok(())
}

#[test]
fn the_newest_reading_speaks_for_its_group() -> TestResult {
    let readings = vec![
        reading("alpha", "2000", "10000", 120)?,
        reading("alpha", "2728", "10000", 5)?,
        reading("beta", "2728", "10000", 5)?,
    ];

    let aggregated = aggregate(&readings, policy(), now())?;

    assert_eq!(aggregated.group_count, 2);
    // Both remaining readings are 0.2728, kept in lowest terms.
    assert_eq!(aggregated.rate_numerator.to_string(), "341");
    assert_eq!(aggregated.rate_denominator.to_string(), "1250");
    assert_eq!(
        aggregated.discarded,
        vec![(0, PriceDiscardReason::SupersededInGroup)]
    );
    Ok(())
}

#[test]
fn sources_that_disagree_do_not_average_into_a_plausible_middle() -> TestResult {
    // Three per cent apart: one of them is wrong, and quoting the middle would
    // simply be wrong by half as much.
    let readings = vec![
        reading("alpha", "2728", "10000", 5)?,
        reading("beta", "2810", "10000", 5)?,
    ];

    let outcome = aggregate(&readings, policy(), now());

    assert_eq!(
        outcome.err(),
        Some(PriceAggregationError::Diverged {
            deviation_bps: 301,
            ceiling: 200
        })
    );
    Ok(())
}

#[test]
fn a_spread_at_the_ceiling_is_still_usable() -> TestResult {
    // Exactly two per cent apart, and the ceiling is two hundred basis points.
    let readings = vec![
        reading("alpha", "10000", "10000", 5)?,
        reading("beta", "10200", "10000", 5)?,
    ];

    let aggregated = aggregate(&readings, policy(), now())?;

    assert_eq!(aggregated.deviation_bps, 200);
    Ok(())
}

#[test]
fn stale_and_future_readings_are_named_not_silently_dropped() -> TestResult {
    let readings = vec![
        reading("alpha", "2728", "10000", 5)?,
        reading("beta", "2728", "10000", 600)?,
        PriceReading {
            observed_at: now() + Duration::seconds(30),
            ..reading("gamma", "2728", "10000", 0)?
        },
    ];

    let outcome = aggregate(&readings, policy(), now());

    assert_eq!(
        outcome.err(),
        Some(PriceAggregationError::NotEnoughSources {
            required: 2,
            available: 1
        }),
        "a stale and a future-dated reading leave one usable source"
    );

    // The same readings, with a policy that tolerates the age, keep the record
    // of why the future-dated one still did not count.
    let tolerant = PriceAggregationPolicy {
        max_age_seconds: 3_600,
        ..policy()
    };
    let aggregated = aggregate(&readings, tolerant, now())?;
    assert_eq!(aggregated.group_count, 2);
    assert_eq!(
        aggregated.discarded,
        vec![(2, PriceDiscardReason::FutureDated)]
    );
    Ok(())
}

#[test]
fn a_policy_that_would_trust_one_source_is_refused() -> TestResult {
    let readings = vec![reading("alpha", "2728", "10000", 5)?];
    let single = PriceAggregationPolicy {
        min_sources: 1,
        ..policy()
    };

    assert_eq!(
        aggregate(&readings, single, now()).err(),
        Some(PriceAggregationError::PolicyDemandsTooFewSources)
    );
    Ok(())
}

#[test]
fn rates_written_differently_are_the_same_rate() -> TestResult {
    // 2728/10000 and 341/1250 are the same number; agreeing sources that write
    // it differently must not read as a disagreement.
    let readings = vec![
        reading("alpha", "2728", "10000", 5)?,
        reading("beta", "341", "1250", 5)?,
    ];

    let aggregated = aggregate(&readings, policy(), now())?;

    assert_eq!(aggregated.deviation_bps, 0);
    assert_eq!(aggregated.rate_numerator.to_string(), "341");
    assert_eq!(aggregated.rate_denominator.to_string(), "1250");
    Ok(())
}
