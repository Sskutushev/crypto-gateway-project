//! The connection pool as the process sees it.
//!
//! `sqlx` offers no hook on acquire, so the wait for a connection is
//! measured at the two places this crate reaches the pool: every
//! transaction begins through [`crate::PostgresRepository::begin`], which
//! times the hand-over, and the sampler acquires one connection on each
//! tick as a probe of what a new caller would wait right now. The pool's
//! shape is published beside it, so a pool pinned at its ceiling and the
//! queue behind it are both visible.

use std::time::{Duration, Instant};

use gateway_telemetry::{Gauge, Histogram, LATENCY_BUCKETS_MICROS, Labels};
use sqlx::PgPool;
use tokio::{sync::watch, time::interval};

static ACQUIRE_WAIT: Histogram = Histogram::new(
    "gateway_db_pool_acquire_wait_seconds",
    "How long the pool took to hand over a connection: at a transaction's start, or for the sampler's probe.",
    &LATENCY_BUCKETS_MICROS,
);

static CONNECTIONS: Gauge = Gauge::new(
    "gateway_db_pool_connections",
    "Connections the pool holds, by state.",
);

static MAX_CONNECTIONS: Gauge = Gauge::new(
    "gateway_db_pool_max_connections",
    "The pool's configured ceiling.",
);

/// How often the pool is sampled and probed.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// Records one wait for a connection. `path` says where it was measured.
pub(crate) fn record_acquire(path: &str, waited: Duration, acquired: bool) {
    ACQUIRE_WAIT.observe(
        &Labels::new(&[
            ("path", path),
            ("outcome", if acquired { "acquired" } else { "failed" }),
        ]),
        waited,
    );
}

/// Acquires and releases one connection, recording the wait. A failure is
/// recorded as such and not retried: the probe reports, it does not heal.
pub async fn probe_pool(pool: &PgPool) {
    let started = Instant::now();
    let acquired = pool.acquire().await.is_ok();
    record_acquire("probe", started.elapsed(), acquired);
}

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

/// Samples and probes the pool every few seconds until `shutdown` is set or
/// its sender is dropped.
pub async fn sample_pool(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = interval(SAMPLE_INTERVAL);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                record_pool(&pool);
                probe_pool(&pool).await;
            }
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

    use super::{probe_pool, record_pool};

    #[tokio::test]
    async fn the_pool_shape_is_published_and_a_failed_probe_is_counted_as_failed()
    -> Result<(), sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(7)
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy("postgres://nobody:nobody@127.0.0.1:1/nowhere")?;
        record_pool(&pool);
        probe_pool(&pool).await;
        let text = gateway_telemetry::render();
        assert!(
            text.contains("gateway_db_pool_max_connections 7\n"),
            "{text}"
        );
        assert!(
            text.contains("gateway_db_pool_connections{state=\"open\"} 0\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "gateway_db_pool_acquire_wait_seconds_count{path=\"probe\",outcome=\"failed\"} "
            ),
            "{text}"
        );
        Ok(())
    }
}
