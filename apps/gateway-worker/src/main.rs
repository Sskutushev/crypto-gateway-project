//! The gateway's background processes.
//!
//! One image, one binary, several deployments. `GATEWAY_WORKER_ROLES` decides
//! which loops this process runs, so an observer can be given RPC access and a
//! read-only database role while the process that moves money is given neither.
//!
//! Every role is a bounded, leased loop. They share one shutdown path: the
//! signal a container runtime actually sends stops each loop between batches,
//! and the process does not exit until every loop has stopped, so no in-flight
//! transaction is abandoned by a disappearing runtime.

mod settings;

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use gateway_application::{
    ChainSource, CollectorWatch, ObservationRepository, ObservationService, OutboxService,
    QuoteService, ReconciliationKind, ReconciliationService, ReconciliationWindow,
    SelfCheckService, SettlementService, SystemClock, VerificationService,
};
use gateway_scheduler::{
    BatchConfig, ExpiryScheduler, LeasedWorker, ObservationWorker, OutboxWorker,
    ReconciliationWorker, SettlementWorker, VerificationWorker, WorkerLoop,
};
use gateway_storage::{PgPoolOptions, PostgresRepository};
use gateway_tron::{ReqwestTransport, ScanLane, TokenView, TronHttpSource, TronSourceConfig};
use gateway_webhook::HttpWebhookSender;
use tokio::{signal, sync::watch, task::JoinHandle};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::settings::{Role, TronEndpoint, WorkerSettings};

const PARSER_VERSION: &str = concat!("tron-log/", env!("CARGO_PKG_VERSION"));
const AGENT_VERSION: &str = concat!("gateway-worker/", env!("CARGO_PKG_VERSION"));

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let settings = WorkerSettings::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&settings.database_url)
        .await
        .context("connect to PostgreSQL")?;
    let repository = Arc::new(PostgresRepository::new(pool));
    let startup_report = SelfCheckService::new(
        Arc::clone(&repository),
        SystemClock,
        settings.self_check.clone(),
    )
    .run()
    .await
    .context("run startup self-check")?;
    if !startup_report.passed {
        error!(report = ?startup_report, "startup self-check failed");
        bail!("startup self-check failed");
    }

    // No lease is requested before the checks above succeed: an incorrectly
    // configured process must not briefly become leader and mutate state.
    let (shutdown, _) = watch::channel(false);
    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    for role in settings.roles.clone() {
        let config = settings
            .batch(role)
            .cloned()
            .with_context(|| format!("no schedule was read for the {} role", role.as_str()))?;
        let stop = shutdown.subscribe();
        let handle = match role {
            Role::Expiry => spawn_expiry(&repository, config, stop)?,
            Role::Observer => spawn_observer(&settings, &repository, config, stop).await?,
            Role::Verifier => spawn_verifier(&settings, &repository, config, stop).await?,
            Role::Settlement => spawn_settlement(&settings, &repository, config, stop)?,
            Role::Outbox => spawn_outbox(&settings, &repository, config, stop)?,
            Role::Reconciler => spawn_reconciler(&settings, &repository, config, stop)?,
        };
        handles.push(handle);
        info!(role = role.as_str(), "role started");
    }

    info!(
        instance = settings.instance,
        roles = settings
            .roles
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>()
            .join(","),
        "gateway worker running"
    );

    stop_signal().await;
    info!("shutdown requested; stopping every role between batches");
    if shutdown.send(true).is_err() {
        error!("the shutdown channel closed before shutdown was requested");
    }
    for handle in handles {
        handle.await.context("join a worker role")?;
    }
    info!("every role stopped");
    Ok(())
}

fn spawn_expiry(
    repository: &Arc<PostgresRepository>,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let quotes = Arc::new(QuoteService::new(Arc::clone(repository), SystemClock));
    let scheduler = ExpiryScheduler::new(quotes, config).context("configure the expiry sweep")?;
    Ok(tokio::spawn(async move {
        scheduler.run(stop).await;
    }))
}

async fn spawn_observer(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let observer = settings
        .observer
        .as_ref()
        .context("the observer role needs GATEWAY_OBSERVER_* settings")?;
    let source = registered_source(settings, repository, &observer.source_key).await?;
    let collectors = watched(settings, repository).await?;
    let config_tron = tron_config(
        settings,
        &observer.endpoint,
        &collectors,
        observer.lane,
        observer.max_blocks_per_scan,
        observer.bootstrap_lookback_blocks,
    );
    let transport = ReqwestTransport::new(&config_tron).context("build the TRON transport")?;
    let scanner = Arc::new(TronHttpSource::new(transport, config_tron));
    let service = Arc::new(ObservationService::new(
        Arc::clone(repository),
        SystemClock,
        AGENT_VERSION,
        PARSER_VERSION,
    ));
    let component = format!("observer:{}:{}", source.chain, source.source_key);
    let worker = Arc::new(ObservationWorker::new(
        service,
        Arc::clone(repository),
        scanner,
        source,
        component,
        config.batch_limit,
    ));
    spawn_loop(worker, repository, settings, config, stop)
}

async fn spawn_verifier(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let verifier = settings
        .verifier
        .as_ref()
        .context("the verifier role needs GATEWAY_VERIFIER_* settings")?;
    let source = registered_source(settings, repository, &verifier.source_key).await?;
    let collectors = watched(settings, repository).await?;
    // The verifier reads one transaction at a time, so no lane setting of its
    // own applies; the bounds below are the ones a re-read never uses.
    let config_tron = tron_config(
        settings,
        &verifier.endpoint,
        &collectors,
        ScanLane::BlockRange,
        1,
        0,
    );
    let transport = ReqwestTransport::new(&config_tron).context("build the TRON transport")?;
    let reader = Arc::new(TronHttpSource::new(transport, config_tron));
    let service = Arc::new(VerificationService::new(
        Arc::clone(repository),
        reader,
        SystemClock,
        source,
        AGENT_VERSION,
        PARSER_VERSION,
    ));
    let worker = Arc::new(VerificationWorker::new(
        service,
        "verifier",
        config.batch_limit,
    ));
    spawn_loop(worker, repository, settings, config, stop)
}

fn spawn_settlement(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let service = Arc::new(SettlementService::new(Arc::clone(repository), SystemClock));
    let worker = Arc::new(SettlementWorker::new(
        service,
        "settlement",
        config.batch_limit,
    ));
    spawn_loop(worker, repository, settings, config, stop)
}

fn spawn_outbox(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let outbox = settings
        .outbox
        .as_ref()
        .context("the outbox role needs GATEWAY_WEBHOOK_* settings")?;
    let sender = Arc::new(
        HttpWebhookSender::new(outbox.request_timeout, AGENT_VERSION)
            .context("build the webhook sender")?,
    );
    let service = Arc::new(
        OutboxService::new(
            Arc::clone(repository),
            sender,
            SystemClock,
            outbox.master_key.clone(),
            settings.instance.clone(),
            outbox.max_attempts,
        )
        .context("configure signed webhook delivery")?,
    );
    let worker = Arc::new(OutboxWorker::new(service, "outbox", config.batch_limit));
    spawn_loop(worker, repository, settings, config, stop)
}

fn spawn_reconciler(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>> {
    let service = Arc::new(ReconciliationService::new(
        Arc::clone(repository),
        SystemClock,
        ReconciliationWindow::default(),
    ));
    let worker = Arc::new(ReconciliationWorker::new(
        service,
        "reconciler",
        ReconciliationKind::Incremental,
    ));
    spawn_loop(worker, repository, settings, config, stop)
}

fn spawn_loop<W>(
    worker: Arc<W>,
    repository: &Arc<PostgresRepository>,
    settings: &WorkerSettings,
    config: BatchConfig,
    stop: watch::Receiver<bool>,
) -> Result<JoinHandle<()>>
where
    W: LeasedWorker + 'static,
{
    let worker_loop = Arc::new(
        WorkerLoop::new(
            worker,
            Arc::clone(repository),
            settings.instance.clone(),
            config,
        )
        .context("configure a worker loop")?,
    );
    Ok(tokio::spawn(async move {
        worker_loop.run(stop).await;
    }))
}

/// Loads the source row this process speaks as, and refuses to start under one
/// that describes another chain or another world.
async fn registered_source(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
    source_key: &str,
) -> Result<ChainSource> {
    let source = repository
        .find_source(source_key)
        .await
        .context("read the chain source")?
        .with_context(|| {
            format!(
                "chain source \"{source_key}\" is not registered; a source with no provider group \
                 can never count as independent evidence"
            )
        })?;
    if source.chain != settings.chain
        || source.network != settings.network
        || source.chain_environment != settings.chain_environment
    {
        bail!(
            "chain source \"{source_key}\" is registered for {}/{}/{} but this process is \
             configured for {}/{}/{}",
            source.chain,
            source.network,
            source.chain_environment.as_str(),
            settings.chain,
            settings.network,
            settings.chain_environment.as_str()
        );
    }
    Ok(source)
}

async fn watched(
    settings: &WorkerSettings,
    repository: &Arc<PostgresRepository>,
) -> Result<Vec<CollectorWatch>> {
    let collectors = repository
        .watched_collectors(
            &settings.chain,
            &settings.network,
            settings.chain_environment,
        )
        .await
        .context("read the watched collector addresses")?;
    if collectors.is_empty() {
        bail!(
            "no active collector address is configured for {}/{}/{}",
            settings.chain,
            settings.network,
            settings.chain_environment.as_str()
        );
    }
    Ok(collectors)
}

/// Builds the source configuration, naming only the tokens the database
/// allowlists. A contract that is not one of them is still read; it simply
/// keeps its own address instead of borrowing a familiar name.
fn tron_config(
    settings: &WorkerSettings,
    endpoint: &TronEndpoint,
    collectors: &[CollectorWatch],
    lane: ScanLane,
    max_blocks_per_scan: u32,
    bootstrap_lookback_blocks: u32,
) -> TronSourceConfig {
    let mut tokens: Vec<TokenView> = Vec::new();
    for collector in collectors {
        if tokens
            .iter()
            .any(|token| token.token_key == collector.token_key)
        {
            continue;
        }
        tokens.push(TokenView {
            token_key: collector.token_key.clone(),
            decimals: collector.decimals,
            display: collector.token_display.clone(),
        });
    }
    TronSourceConfig {
        base_url: endpoint.base_url.clone(),
        api_key: endpoint.api_key.clone(),
        api_key_header: endpoint.api_key_header.clone(),
        chain: settings.chain.clone(),
        network: settings.network.clone(),
        chain_environment: settings.chain_environment,
        lane,
        request_timeout: endpoint.request_timeout,
        max_blocks_per_scan,
        bootstrap_lookback_blocks,
        tokens,
        user_agent: AGENT_VERSION.to_owned(),
    }
}

/// Waits for the signal a supervisor actually sends.
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
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("gateway_worker=info,gateway_scheduler=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize tracing: {error}"))?;
    Ok(())
}
