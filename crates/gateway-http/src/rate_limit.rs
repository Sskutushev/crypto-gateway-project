//! Request budgets per merchant and per client address.
//!
//! Budgets live in this process's memory: with several API replicas each one
//! grants its own budget, so the effective limit is the per-replica limit
//! times the replica count. They protect the database and the other merchants
//! from one caller; they are not an accounting of anything.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Instant,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderName, Method},
    middleware::Next,
    response::Response,
};

use crate::{AppState, auth::MerchantAuth, error::ApiError};

/// Beyond this many tracked keys, keys whose bucket has refilled are dropped,
/// so a caller cycling through addresses cannot grow memory without bound.
const MAX_TRACKED_KEYS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Writes (POST) per merchant per minute; 0 disables the budget.
    pub merchant_writes_per_minute: u32,
    /// Reads (GET) per merchant per minute; 0 disables the budget.
    pub merchant_reads_per_minute: u32,
    /// Failed authentications per client address per minute before every
    /// request from it is refused; 0 disables the budget.
    pub auth_failures_per_minute: u32,
    /// Public payment-page reads per client address per minute; 0 disables.
    pub checkout_reads_per_minute: u32,
    /// A header a trusted reverse proxy appends the client address to. When
    /// set, its last entry is the client; when unset, the TCP peer is.
    pub client_ip_header: Option<HeaderName>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            merchant_writes_per_minute: 600,
            merchant_reads_per_minute: 3_000,
            auth_failures_per_minute: 30,
            checkout_reads_per_minute: 600,
            client_ip_header: None,
        }
    }
}

#[derive(Debug)]
pub struct RateLimits {
    config: RateLimitConfig,
    merchant_writes: Budget,
    merchant_reads: Budget,
    auth_failures: Budget,
    checkout_reads: Budget,
}

impl RateLimits {
    #[must_use]
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            merchant_writes: Budget::per_minute(config.merchant_writes_per_minute),
            merchant_reads: Budget::per_minute(config.merchant_reads_per_minute),
            auth_failures: Budget::per_minute(config.auth_failures_per_minute),
            checkout_reads: Budget::per_minute(config.checkout_reads_per_minute),
            config,
        }
    }

    fn client(&self, request: &Request) -> Option<IpAddr> {
        if let Some(header) = &self.config.client_ip_header {
            // The proxy appends; entries before the last were written by the
            // client and prove nothing.
            return request
                .headers()
                .get(header)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .and_then(|value| value.trim().parse().ok());
        }
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(address)| address.ip())
    }
}

/// One token, in the integer units a bucket counts in.
const TOKEN: u128 = 1_000_000;

/// A token bucket per key: `limit` tokens, refilled continuously at `limit`
/// per minute, so a minute's budget may be spent in one burst. Counted in
/// millionths of a token so the refill is exact integer arithmetic.
#[derive(Debug)]
struct Budget {
    limit: u32,
    buckets: Mutex<HashMap<String, (u128, Instant)>>,
}

impl Budget {
    fn per_minute(limit: u32) -> Self {
        Self {
            limit,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn capacity(&self) -> u128 {
        u128::from(self.limit) * TOKEN
    }

    /// Units refilled over `elapsed`: `limit` tokens per 60 000 ms.
    fn refilled(&self, units: u128, since: Instant, now: Instant) -> u128 {
        let elapsed_ms = now.saturating_duration_since(since).as_millis();
        units
            .saturating_add(elapsed_ms.saturating_mul(u128::from(self.limit)) * TOKEN / 60_000)
            .min(self.capacity())
    }

    /// Whole seconds until a bucket holding `units` has one token again.
    fn wait_seconds(&self, units: u128) -> u64 {
        let missing = TOKEN.saturating_sub(units);
        // `limit` tokens per minute is `limit * TOKEN` units per 60 s.
        let per_minute = u128::from(self.limit) * TOKEN;
        let seconds = (missing * 60).div_ceil(per_minute);
        u64::try_from(seconds).unwrap_or(u64::MAX).max(1)
    }

    /// Takes one token; on refusal returns the whole seconds until one exists.
    fn take(&self, key: &str, now: Instant) -> Result<(), u64> {
        if self.limit == 0 {
            return Ok(());
        }
        let capacity = self.capacity();
        let mut buckets = self.lock();
        if buckets.len() >= MAX_TRACKED_KEYS && !buckets.contains_key(key) {
            buckets.retain(|_, (units, at)| self.refilled(*units, *at, now) < capacity);
        }
        let (units, at) = buckets.entry(key.to_owned()).or_insert((capacity, now));
        let available = self.refilled(*units, *at, now);
        *at = now;
        if available >= TOKEN {
            *units = available - TOKEN;
            Ok(())
        } else {
            *units = available;
            Err(self.wait_seconds(available))
        }
    }

    /// Whether the key could take a token now, without taking it.
    fn has_token(&self, key: &str, now: Instant) -> Result<(), u64> {
        if self.limit == 0 {
            return Ok(());
        }
        let buckets = self.lock();
        let Some((units, at)) = buckets.get(key) else {
            return Ok(());
        };
        let available = self.refilled(*units, *at, now);
        if available >= TOKEN {
            Ok(())
        } else {
            Err(self.wait_seconds(available))
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (u128, Instant)>> {
        // A panic while holding the lock leaves only token counts behind,
        // which are safe to keep using.
        self.buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Refuses every request from a client address that has spent its budget of
/// failed authentications, and charges one to it for each failure.
pub async fn limit_auth_failures(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let limits = Arc::clone(&state.rate_limits);
    let client = limits.client(&request).map(|ip| ip.to_string());
    if let Some(client) = &client {
        limits
            .auth_failures
            .has_token(client, Instant::now())
            .map_err(|retry_after_seconds| ApiError::RateLimited {
                retry_after_seconds,
            })?;
    }
    let response = next.run(request).await;
    if response.status() == http::StatusCode::UNAUTHORIZED
        && let Some(client) = &client
    {
        // The failure is charged after the fact, so the request that spends
        // the last token is still answered with its own 401.
        let _ = limits.auth_failures.take(client, Instant::now());
    }
    Ok(response)
}

/// A budget per authenticated merchant: writes and reads separately, so a
/// polling loop cannot use up the budget for creating payments.
pub async fn limit_merchant(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(MerchantAuth(credential)) = request.extensions().get::<MerchantAuth>() {
        let budget = if request.method() == Method::GET {
            &state.rate_limits.merchant_reads
        } else {
            &state.rate_limits.merchant_writes
        };
        budget
            .take(&credential.merchant_id.to_string(), Instant::now())
            .map_err(|retry_after_seconds| ApiError::RateLimited {
                retry_after_seconds,
            })?;
    }
    Ok(next.run(request).await)
}

/// A budget per client address for the public payment page, which needs no
/// credential and so has no other key to be limited by.
pub async fn limit_checkout(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(client) = state.rate_limits.client(&request) {
        state
            .rate_limits
            .checkout_reads
            .take(&client.to_string(), Instant::now())
            .map_err(|retry_after_seconds| ApiError::RateLimited {
                retry_after_seconds,
            })?;
    }
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Budget;

    #[test]
    fn a_budget_allows_its_burst_then_refuses_until_a_token_refills() {
        let budget = Budget::per_minute(3);
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(budget.take("m", start), Ok(()));
        }
        // One token refills every 20 seconds.
        assert_eq!(budget.take("m", start), Err(20));
        assert_eq!(budget.take("other", start), Ok(()));
        assert_eq!(budget.take("m", start + Duration::from_secs(20)), Ok(()));
        assert!(budget.take("m", start + Duration::from_secs(21)).is_err());
    }

    #[test]
    fn a_zero_budget_is_disabled_rather_than_closed() {
        let budget = Budget::per_minute(0);
        let now = Instant::now();
        for _ in 0..1_000 {
            assert_eq!(budget.take("m", now), Ok(()));
        }
    }

    #[test]
    fn looking_at_a_budget_does_not_spend_it() {
        let budget = Budget::per_minute(1);
        let now = Instant::now();
        assert_eq!(budget.has_token("ip", now), Ok(()));
        assert_eq!(budget.has_token("ip", now), Ok(()));
        assert_eq!(budget.take("ip", now), Ok(()));
        assert_eq!(budget.has_token("ip", now), Err(60));
    }
}
