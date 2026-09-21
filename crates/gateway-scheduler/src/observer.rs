use std::sync::Arc;

use async_trait::async_trait;
use gateway_application::{
    ChainScanner, ChainSource, Clock, ComponentLease, CursorPosition, ObservationRepository,
    ObservationService, RefusalReason,
};
use gateway_domain::ObservationKind;
use tracing::warn;

use crate::worker::{BatchOutcome, LeasedWorker, WorkerError};

/// Reads one source's window onto the watched collector addresses and records
/// what it claimed.
///
/// The worker never decides anything about a payment. It turns a provider's
/// answer into stored evidence, advances its own cursor, and counts every
/// reading it refused.
#[derive(Debug)]
pub struct ObservationWorker<R, S, C> {
    service: Arc<ObservationService<R, C>>,
    repository: Arc<R>,
    scanner: Arc<S>,
    source: ChainSource,
    component: String,
    batch_limit: u32,
}

impl<R, S, C> ObservationWorker<R, S, C> {
    pub fn new(
        service: Arc<ObservationService<R, C>>,
        repository: Arc<R>,
        scanner: Arc<S>,
        source: ChainSource,
        component: impl Into<String>,
        batch_limit: u32,
    ) -> Self {
        Self {
            service,
            repository,
            scanner,
            source,
            component: component.into(),
            batch_limit,
        }
    }
}

#[async_trait]
impl<R, S, C> LeasedWorker for ObservationWorker<R, S, C>
where
    R: ObservationRepository,
    S: ChainScanner,
    C: Clock,
{
    fn component(&self) -> &str {
        &self.component
    }

    fn name(&self) -> &'static str {
        "observer"
    }

    async fn run_batch(&self, lease: &ComponentLease) -> Result<BatchOutcome, WorkerError> {
        let collectors = self
            .repository
            .watched_collectors(
                &self.source.chain,
                &self.source.network,
                self.source.chain_environment,
            )
            .await
            .map_err(|error| WorkerError::Transient(error.to_string()))?;

        let mut processed = 0_u32;
        let mut drained = true;

        for watch in &collectors {
            let cursor = self
                .repository
                .find_cursor(
                    self.source.id,
                    ObservationKind::CursorScan,
                    watch.collector_address_id,
                )
                .await
                .map_err(|error| WorkerError::Transient(error.to_string()))?;

            let page = self
                .scanner
                .scan(watch, cursor.as_ref(), self.batch_limit)
                .await
                .map_err(|error| {
                    if error.is_transient() {
                        WorkerError::Transient(error.to_string())
                    } else {
                        WorkerError::Permanent(error.to_string())
                    }
                })?;

            let read = u32::try_from(page.transfers.len()).unwrap_or(u32::MAX);
            if read >= self.batch_limit {
                drained = false;
            }

            // A cursor only moves under the current fence token, and only when
            // the source actually said where the next page starts.
            let next_cursor = page.next_cursor.map(|position| {
                (
                    watch.collector_address_id,
                    CursorPosition {
                        fence_token: lease.fence_token,
                        ..position
                    },
                )
            });

            let (report, refused) = self
                .service
                .intake(
                    &self.source,
                    lease,
                    ObservationKind::CursorScan,
                    &collectors,
                    page.transfers,
                    next_cursor,
                )
                .await
                .map_err(|error| {
                    if error.is_transient() {
                        WorkerError::Transient(error.to_string())
                    } else {
                        WorkerError::Permanent(error.to_string())
                    }
                })?;

            for (transfer, reason) in &refused {
                // A refusal is never silent: money sent to an address we do not
                // watch, or on the wrong network, is an operational signal.
                warn!(
                    source = %self.source.source_key,
                    tx_hash = %transfer.tx_hash,
                    reason = reason.as_str(),
                    "reading refused during intake"
                );
                if matches!(reason, RefusalReason::RetiredCollector) {
                    warn!(
                        collector = %transfer.to_address_text,
                        "a payment reached a retired collector address"
                    );
                }
            }
            processed = processed.saturating_add(report.recorded);
        }

        Ok(BatchOutcome { processed, drained })
    }
}
