use std::{env, net::SocketAddr};

use anyhow::{Context, Result, bail};
use gateway_http::{AppState, router};
use gateway_storage::{PgPoolOptions, migrate};
use tokio::{net::TcpListener, signal};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let database_url = required_env("GATEWAY_DATABASE_URL")?;
    let bind_address: SocketAddr = env::var("GATEWAY_BIND_ADDRESS")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse()
        .context("GATEWAY_BIND_ADDRESS must be a socket address")?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .context("connect to PostgreSQL")?;

    if env::var("GATEWAY_RUN_MIGRATIONS").as_deref() == Ok("true") {
        migrate(&pool).await.context("run database migrations")?;
    }

    let listener = TcpListener::bind(bind_address)
        .await
        .with_context(|| format!("bind HTTP server to {bind_address}"))?;
    info!(%bind_address, "gateway API listening");
    axum::serve(listener, router(AppState::new(pool)))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP API")?;
    Ok(())
}

fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("gateway_api=info,tower_http=info"));
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

async fn shutdown_signal() {
    if let Err(error) = signal::ctrl_c().await {
        tracing::error!(%error, "failed to install shutdown signal handler");
    }
}
