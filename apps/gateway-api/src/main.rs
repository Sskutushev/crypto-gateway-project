mod expiry_settings;

use std::{env, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result, bail};
use gateway_http::{AppState, router};
use gateway_scheduler::ExpiryScheduler;
use gateway_storage::{PgPoolOptions, migrate};
use tokio::{net::TcpListener, signal, sync::watch};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::expiry_settings::expiry_config;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let database_url = required_env("GATEWAY_DATABASE_URL")?;
    let bind_address: SocketAddr = env::var("GATEWAY_BIND_ADDRESS")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse()
        .context("GATEWAY_BIND_ADDRESS must be a socket address")?;
    let expiry_config = expiry_config()?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .context("connect to PostgreSQL")?;

    if env::var("GATEWAY_RUN_MIGRATIONS").as_deref() == Ok("true") {
        migrate(&pool).await.context("run database migrations")?;
    }

    let state = AppState::new(pool);
    let (shutdown, expiry_shutdown) = watch::channel(false);
    let http_shutdown = shutdown.subscribe();

    let expiry = if let Some(config) = expiry_config {
        let scheduler = ExpiryScheduler::new(Arc::clone(&state.quotes), config)
            .context("configure the quote expiry scheduler")?;
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
    let served = axum::serve(listener, router(state))
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
