use std::net::SocketAddr;

use axum::{Router, http::header, response::IntoResponse, routing::get};
use thiserror::Error;
use tokio::{net::TcpListener, sync::watch};
use tracing::info;

use crate::registry::render;

#[derive(Debug, Error)]
pub enum ListenerError {
    #[error("bind the metrics listener to {address}: {source}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("serve the metrics listener: {0}")]
    Serve(#[source] std::io::Error),
}

/// `GET /metrics` on an address of its own.
///
/// The listener is for the cluster's monitoring system only: the deployment
/// definitions keep the port off the Service and off the ingress, and the
/// network policy admits the monitoring namespace alone. There is no key,
/// because what is served here reveals nothing about a merchant.
#[derive(Debug)]
pub struct MetricsListener {
    listener: TcpListener,
    address: SocketAddr,
}

impl MetricsListener {
    /// Binds the address before the process declares itself started, so a
    /// configured listener that cannot exist stops the process instead of
    /// leaving the operator reading last week's dashboard.
    ///
    /// # Errors
    ///
    /// Returns [`ListenerError::Bind`] when the address cannot be bound.
    pub async fn bind(address: SocketAddr) -> Result<Self, ListenerError> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|source| ListenerError::Bind { address, source })?;
        info!(%address, "process metrics listening");
        Ok(Self { listener, address })
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Serves until `shutdown` is set or its sender is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`ListenerError::Serve`] when the server fails.
    pub async fn serve(self, mut shutdown: watch::Receiver<bool>) -> Result<(), ListenerError> {
        let app = Router::new().route("/metrics", get(metrics));
        axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                // A closed channel means the owner is gone; stopping is the
                // only honest reaction to that as well.
                while shutdown.changed().await.is_ok() {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            })
            .await
            .map_err(ListenerError::Serve)
    }
}

async fn metrics() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        render(),
    )
}
