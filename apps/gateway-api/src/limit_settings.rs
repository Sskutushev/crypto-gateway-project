use std::env;

use anyhow::{Context, Result, bail};
use gateway_http::RateLimitConfig;
use http::HeaderName;

/// Request budgets from the environment. An unset variable keeps the
/// documented default; a set but unreadable one stops the process, because a
/// typo must not silently become "no limit".
pub fn rate_limit_config() -> Result<RateLimitConfig> {
    let defaults = RateLimitConfig::default();
    Ok(RateLimitConfig {
        merchant_writes_per_minute: per_minute(
            "GATEWAY_RATE_LIMIT_MERCHANT_WRITES_PER_MINUTE",
            defaults.merchant_writes_per_minute,
        )?,
        merchant_reads_per_minute: per_minute(
            "GATEWAY_RATE_LIMIT_MERCHANT_READS_PER_MINUTE",
            defaults.merchant_reads_per_minute,
        )?,
        auth_failures_per_minute: per_minute(
            "GATEWAY_RATE_LIMIT_AUTH_FAILURES_PER_MINUTE",
            defaults.auth_failures_per_minute,
        )?,
        checkout_reads_per_minute: per_minute(
            "GATEWAY_RATE_LIMIT_CHECKOUT_READS_PER_MINUTE",
            defaults.checkout_reads_per_minute,
        )?,
        client_ip_header: match env::var("GATEWAY_CLIENT_IP_HEADER") {
            Ok(value) if !value.trim().is_empty() => Some(
                HeaderName::try_from(value.trim().to_ascii_lowercase())
                    .context("GATEWAY_CLIENT_IP_HEADER must be a header name")?,
            ),
            _ => None,
        },
    })
}

/// The reservation cap per receiving address; unset means no cap.
pub fn max_open_leases_per_collector() -> Result<Option<u64>> {
    match env::var("GATEWAY_MAX_OPEN_LEASES_PER_COLLECTOR") {
        Ok(value) if !value.trim().is_empty() => {
            let limit: u64 = value
                .trim()
                .parse()
                .context("GATEWAY_MAX_OPEN_LEASES_PER_COLLECTOR must be a whole number")?;
            if limit == 0 {
                bail!(
                    "GATEWAY_MAX_OPEN_LEASES_PER_COLLECTOR must be at least 1; unset it for no cap"
                );
            }
            Ok(Some(limit))
        }
        _ => Ok(None),
    }
}

fn per_minute(name: &str, default: u32) -> Result<u32> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse()
            .with_context(|| format!("{name} must be a whole number; 0 disables the budget")),
        _ => Ok(default),
    }
}
