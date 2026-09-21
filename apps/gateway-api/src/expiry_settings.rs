use std::{env, time::Duration};

use anyhow::{Context, Result, bail};
use gateway_scheduler::{ExpiryConfig, RetryPolicy};

/// Reads the expiry scheduler settings, or `None` when it is switched off.
///
/// An unreadable value is a startup failure. Falling back to a default would
/// silently run a different schedule than the operator configured.
pub fn expiry_config() -> Result<Option<ExpiryConfig>> {
    if !toggle(
        "GATEWAY_EXPIRY_ENABLED",
        read("GATEWAY_EXPIRY_ENABLED").as_deref(),
    )? {
        return Ok(None);
    }
    let defaults = ExpiryConfig::default();
    let config = ExpiryConfig {
        interval: seconds(
            "GATEWAY_EXPIRY_INTERVAL_SECONDS",
            defaults.interval.as_secs(),
        )?,
        batch_limit: count("GATEWAY_EXPIRY_BATCH_LIMIT", defaults.batch_limit)?,
        max_batches_per_tick: count(
            "GATEWAY_EXPIRY_MAX_BATCHES_PER_TICK",
            defaults.max_batches_per_tick,
        )?,
        retry: RetryPolicy {
            max_attempts: count("GATEWAY_EXPIRY_RETRY_ATTEMPTS", defaults.retry.max_attempts)?,
            initial_backoff: seconds(
                "GATEWAY_EXPIRY_RETRY_INITIAL_BACKOFF_SECONDS",
                defaults.retry.initial_backoff.as_secs(),
            )?,
            max_backoff: seconds(
                "GATEWAY_EXPIRY_RETRY_MAX_BACKOFF_SECONDS",
                defaults.retry.max_backoff.as_secs(),
            )?,
        },
    };
    Ok(Some(config.validated().context(
        "GATEWAY_EXPIRY_* settings describe an unsafe expiry schedule",
    )?))
}

fn read(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn seconds(name: &str, default: u64) -> Result<Duration> {
    let raw = read(name);
    Ok(Duration::from_secs(number(name, raw.as_deref(), default)?))
}

fn count(name: &str, default: u32) -> Result<u32> {
    let raw = read(name);
    let value = number(name, raw.as_deref(), u64::from(default))?;
    u32::try_from(value).with_context(|| format!("{name} is too large"))
}

fn number(name: &str, raw: Option<&str>, default: u64) -> Result<u64> {
    match raw {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be a whole number of seconds or rows")),
    }
}

fn toggle(name: &str, raw: Option<&str>) -> Result<bool> {
    match raw {
        None | Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => bail!("{name} must be exactly \"true\" or \"false\""),
    }
}

#[cfg(test)]
mod tests {
    use super::{number, toggle};

    #[test]
    fn missing_settings_use_documented_defaults() -> anyhow::Result<()> {
        assert!(toggle("GATEWAY_EXPIRY_ENABLED", None)?);
        assert_eq!(number("GATEWAY_EXPIRY_BATCH_LIMIT", None, 200)?, 200);
        Ok(())
    }

    #[test]
    fn unreadable_settings_fail_startup_instead_of_defaulting() {
        assert!(toggle("GATEWAY_EXPIRY_ENABLED", Some("yes")).is_err());
        assert!(toggle("GATEWAY_EXPIRY_ENABLED", Some("TRUE")).is_err());
        assert!(number("GATEWAY_EXPIRY_BATCH_LIMIT", Some("many"), 200).is_err());
        assert!(number("GATEWAY_EXPIRY_BATCH_LIMIT", Some("-1"), 200).is_err());
        assert!(number("GATEWAY_EXPIRY_INTERVAL_SECONDS", Some("30.5"), 30).is_err());
    }

    #[test]
    fn explicit_settings_replace_defaults() -> anyhow::Result<()> {
        assert!(!toggle("GATEWAY_EXPIRY_ENABLED", Some("false"))?);
        assert_eq!(number("GATEWAY_EXPIRY_BATCH_LIMIT", Some("50"), 200)?, 50);
        Ok(())
    }
}
