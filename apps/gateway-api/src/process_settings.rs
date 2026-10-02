use std::{env, net::SocketAddr};

use anyhow::{Context, Result, bail};

/// The connection pool ceiling. Unset keeps the documented default; a value
/// that is not a positive number stops the process.
pub fn db_max_connections(default: u32) -> Result<u32> {
    match env::var("GATEWAY_DB_MAX_CONNECTIONS") {
        Ok(value) if !value.trim().is_empty() => {
            let limit: u32 = value
                .trim()
                .parse()
                .context("GATEWAY_DB_MAX_CONNECTIONS must be a whole number")?;
            if limit == 0 {
                bail!("GATEWAY_DB_MAX_CONNECTIONS must be at least 1");
            }
            Ok(limit)
        }
        _ => Ok(default),
    }
}

/// Where process metrics are served. Unset means no listener, which the
/// process says at start-up; a set but unreadable address stops it.
pub fn metrics_bind_address() -> Result<Option<SocketAddr>> {
    match env::var("GATEWAY_METRICS_BIND_ADDRESS") {
        Ok(value) if !value.trim().is_empty() => {
            Ok(Some(value.trim().parse().context(
                "GATEWAY_METRICS_BIND_ADDRESS must be a socket address",
            )?))
        }
        _ => Ok(None),
    }
}
