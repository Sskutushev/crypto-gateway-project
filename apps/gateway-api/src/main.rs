mod expiry_settings;
mod limit_settings;

use std::{env, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result, bail};
use gateway_application::{HonorApprovalPolicy, SelfCheckConfig, SelfCheckService, SystemClock};
use gateway_http::{AppState, router};
use gateway_scheduler::ExpiryScheduler;
use gateway_storage::{PgPoolOptions, PostgresRepository, migrate};
use gateway_tron::from_base58;
use tokio::{net::TcpListener, signal, sync::watch};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::{
    expiry_settings::expiry_config,
    limit_settings::{max_open_leases_per_collector, rate_limit_config},
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let database_url = required_env("GATEWAY_DATABASE_URL")?;
    let bind_address: SocketAddr = env::var("GATEWAY_BIND_ADDRESS")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse()
        .context("GATEWAY_BIND_ADDRESS must be a socket address")?;
    let expiry_config = expiry_config()?;
    let rate_limits = rate_limit_config()?;
    let lease_cap = max_open_leases_per_collector()?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .context("connect to PostgreSQL")?;

    if env::var("GATEWAY_RUN_MIGRATIONS").as_deref() == Ok("true") {
        migrate(&pool).await.context("run database migrations")?;
    }
    // A release applies migrations as the migrator role before any process
    // serves; that process has nothing to serve and no reference data to
    // check yet, so it stops here.
    if env::var("GATEWAY_MIGRATE_ONLY").as_deref() == Ok("true") {
        info!("migrations applied; GATEWAY_MIGRATE_ONLY is set, exiting");
        return Ok(());
    }

    let self_check_config = self_check_config()?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let startup_report = SelfCheckService::new(
        Arc::clone(&repository),
        SystemClock,
        self_check_config.clone(),
    )
    .run()
    .await
    .context("run startup self-check")?;
    if !startup_report.passed {
        error!(report = ?startup_report, "startup self-check failed");
        bail!("startup self-check failed");
    }
    let honor_policy = honor_approval_policy()?;
    info!(
        dual_control_min_raw = %honor_policy.dual_control_min_raw,
        "manual honors at or above this raw amount need a second operator"
    );
    let mut state = AppState::new(pool)
        .with_self_check(self_check_config)
        .with_rate_limits(rate_limits)
        .with_honor_approval_policy(honor_policy);
    if let Some(limit) = lease_cap {
        state = state.with_max_open_leases_per_collector(limit);
    }
    let (shutdown, expiry_shutdown) = watch::channel(false);
    let http_shutdown = shutdown.subscribe();

    let expiry = if let Some(config) = expiry_config {
        let scheduler = ExpiryScheduler::new(Arc::clone(&state.quotes), config)
            .context("configure the quote expiry scheduler")?;
        state = state.with_expiry_metrics(scheduler.metrics());
        Some(tokio::spawn(async move {
            scheduler.run(expiry_shutdown).await;
        }))
    } else {
        info!("quote expiry scheduler disabled by GATEWAY_EXPIRY_ENABLED");
        None
    };

    let listener = TcpListener::bind(bind_address)
        .await
        .with_context(|| format!("bind HTTP server to {bind_address}"))?;
    info!(%bind_address, "gateway API listening");
    let served = axum::serve(
        listener,
        // The peer address feeds the per-client budgets.
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(wait_for_shutdown(http_shutdown))
    .await
    .context("serve HTTP API");

    // The scheduler must stop before the process exits, so an in-flight expiry
    // transaction is never abandoned by a disappearing runtime.
    if shutdown.send(true).is_err() {
        error!("expiry scheduler shutdown channel closed early");
    }
    if let Some(expiry) = expiry {
        expiry.await.context("join the quote expiry scheduler")?;
    }
    served
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    tokio::select! {
        () = stop_signal() => {}
        changed = shutdown.changed() => {
            if changed.is_err() {
                error!("shutdown channel closed before shutdown was requested");
            }
        }
    }
}

/// Waits for the signal a supervisor actually sends. Container runtimes stop a
/// process with `SIGTERM`, so ignoring it would kill an in-flight sweep instead
/// of letting it finish.
async fn stop_signal() {
    #[cfg(unix)]
    {
        let terminate = match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(stream) => Some(stream),
            Err(error) => {
                error!(%error, "failed to install SIGTERM handler");
                None
            }
        };
        let Some(mut terminate) = terminate else {
            interrupt().await;
            return;
        };
        tokio::select! {
            () = interrupt() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    interrupt().await;
}

async fn interrupt() {
    if let Err(error) = signal::ctrl_c().await {
        error!(%error, "failed to install interrupt handler");
        std::future::pending::<()>().await;
    }
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("gateway_api=info,gateway_scheduler=info,tower_http=info")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize tracing: {error}"))?;
    Ok(())
}

fn required_env(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => bail!("{name} is required"),
    }
}

/// Reads `GATEWAY_MANUAL_HONOR_DUAL_CONTROL_MIN_RAW`. Unset means every
/// manual honor needs a second operator; a malformed value stops the process
/// instead of quietly lowering the bar.
fn honor_approval_policy() -> Result<HonorApprovalPolicy> {
    let value = env::var("GATEWAY_MANUAL_HONOR_DUAL_CONTROL_MIN_RAW").ok();
    HonorApprovalPolicy::from_setting(value.as_deref())
        .context("GATEWAY_MANUAL_HONOR_DUAL_CONTROL_MIN_RAW")
}

fn self_check_config() -> Result<SelfCheckConfig> {
    SelfCheckConfig::parse(
        &required_env("GATEWAY_EXPECTED_COLLECTORS")?,
        &required_env("GATEWAY_EXPECTED_ASSETS")?,
        &required_env("GATEWAY_CHAIN_ENVIRONMENT")?,
        &env::var("GATEWAY_MAX_CLOCK_SKEW_SECONDS").unwrap_or_else(|_| "5".to_owned()),
        from_base58,
    )
    .context("parse startup self-check configuration")
}
