//! The connection pool as the process sees it.
//!
//! `sqlx` offers no hook on acquire, so the wait for a connection is not
//! measured here; it is logged by `sqlx` itself above its slow-acquire
//! threshold. What is published is the pool's shape at each sample, which is
//! enough to see a pool that is pinned at its ceiling.

use std::time::Duration;

use gateway_telemetry::{Gauge, Labels};
use sqlx::PgPool;
use tokio::{sync::watch, time::interval};

static CONNECTIONS: Gauge = Gauge::new(
    "gateway_db_pool_connections",
    "Connections the pool holds, by state.",
);

static MAX_CONNECTIONS: Gauge = Gauge::new(
    "gateway_db_pool_max_connections",
    "The pool's configured ceiling.",
);

/// How often the pool is sampled.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// Records the pool's current shape.
pub fn record_pool(pool: &PgPool) {
    let open = i64::from(pool.size());
    let idle = i64::try_from(pool.num_idle()).unwrap_or(i64::MAX);
    CONNECTIONS.set(&Labels::new(&[("state", "open")]), open);
    CONNECTIONS.set(&Labels::new(&[("state", "idle")]), idle);
    CONNECTIONS.set(
        &Labels::new(&[("state", "busy")]),
        open.saturating_sub(idle),
    );
    MAX_CONNECTIONS.set(
        &Labels::none(),
        i64::from(pool.options().get_max_connections()),
    );
}

/// Samples the pool every few seconds until `shutdown` is set or its sender
/// is dropped.
pub async fn sample_pool(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = interval(SAMPLE_INTERVAL);
    loop {
        tokio::select! {
            _ = ticker.tick() => record_pool(&pool),
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::record_pool;

    #[tokio::test]
    async fn the_pool_shape_is_published_without_a_connection() -> Result<(), sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(7)
            .connect_lazy("postgres://nobody:nobody@127.0.0.1:1/nowhere")?;
        record_pool(&pool);
        let text = gateway_telemetry::render();
        assert!(
            text.contains("gateway_db_pool_max_connections 7\n"),
            "{text}"
        );
        assert!(
            text.contains("gateway_db_pool_connections{state=\"open\"} 0\n"),
            "{text}"
        );
        Ok(())
    }
}
