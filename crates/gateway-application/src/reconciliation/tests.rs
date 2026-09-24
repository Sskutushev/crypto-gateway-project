use std::{
    error::Error,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_domain::{CurrencyCode, PriceAggregationPolicy, RailHealth};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    COMPONENT, Discrepancy, DiscrepancyKind, HARD_STOP_REASON, ReconciliationKind,
    ReconciliationRepository, ReconciliationService, ReconciliationWindow, RunRecord, RunStatus,
    ScanFindings,
};
use crate::{
    Clock, ComponentState, ComponentStatus, HealthRepository, ManualResolution,
    ManualResolutionResult, OperationsError, OperationsRepository, OperatorCredential,
    PriceIngestion, RailStop, RecordedPrice, RepositoryError, RiskSubmission,
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET: Uuid = Uuid::from_u128(31);

fn now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
}

#[derive(Debug, Clone, Copy)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        now()
    }
}

#[derive(Debug, Default)]
struct StubRepository {
    findings: Vec<Discrepancy>,
    stops: Mutex<Vec<(Uuid, String)>>,
    states: Mutex<Vec<(String, ComponentState)>>,
}

impl StubRepository {
    fn finding(kind: DiscrepancyKind, asset_id: Option<Uuid>) -> Self {
        Self {
            findings: vec![Discrepancy {
                kind,
                transfer_id: Some(Uuid::from_u128(5)),
                payment_intent_id: None,
                asset_id,
                detail: json!({"allocated_raw": "2", "amount_raw": "1"}),
            }],
            ..Self::default()
        }
    }

    fn stops(&self) -> Vec<(Uuid, String)> {
        self.stops
            .lock()
            .map(|stops| stops.clone())
            .unwrap_or_default()
    }

    fn last_state(&self) -> Option<ComponentState> {
        self.states
            .lock()
            .ok()
            .and_then(|states| states.last().map(|(_, state)| *state))
    }
}

#[async_trait]
impl ReconciliationRepository for StubRepository {
    async fn scan(
        &self,
        _window_start: OffsetDateTime,
        _window_end: OffsetDateTime,
        _window: ReconciliationWindow,
    ) -> Result<ScanFindings, RepositoryError> {
        Ok(ScanFindings {
            transfers_examined: 4,
            intents_examined: 7,
            discrepancies: self.findings.clone(),
        })
    }

    async fn record_run(&self, _record: RunRecord<'_>) -> Result<Uuid, RepositoryError> {
        Ok(Uuid::from_u128(1_000))
    }
}

#[async_trait]
impl HealthRepository for StubRepository {
    async fn publish_component_state(
        &self,
        component: &str,
        state: ComponentState,
        _detail: Option<&str>,
        _now: OffsetDateTime,
    ) -> Result<bool, RepositoryError> {
        if let Ok(mut states) = self.states.lock() {
            states.push((component.to_owned(), state));
        }
        Ok(true)
    }

    async fn component_statuses(&self) -> Result<Vec<ComponentStatus>, RepositoryError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl OperationsRepository for StubRepository {
    async fn authenticate_operator_key(
        &self,
        _secret_hash: &[u8; 32],
    ) -> Result<Option<OperatorCredential>, RepositoryError> {
        Ok(None)
    }

    async fn price_policy(
        &self,
        _asset_id: Uuid,
        _currency: &CurrencyCode,
    ) -> Result<Option<PriceAggregationPolicy>, RepositoryError> {
        Ok(None)
    }

    async fn record_price_ingestion(
        &self,
        _ingestion: &PriceIngestion,
    ) -> Result<Option<RecordedPrice>, RepositoryError> {
        Ok(None)
    }

    async fn record_rail_health(
        &self,
        _asset_id: Uuid,
        _health: RailHealth,
        _detail: Option<String>,
        _ingested_by: Uuid,
        _observed_at: OffsetDateTime,
    ) -> Result<Uuid, RepositoryError> {
        Ok(Uuid::nil())
    }

    async fn open_rail_stop(
        &self,
        asset_id: Uuid,
        reason_code: &str,
        _detail: Option<&str>,
        _opened_by: &str,
        opened_at: OffsetDateTime,
    ) -> Result<RailStop, RepositoryError> {
        if let Ok(mut stops) = self.stops.lock() {
            stops.push((asset_id, reason_code.to_owned()));
        }
        Ok(RailStop {
            id: Uuid::from_u128(2_000),
            asset_id,
            reason_code: reason_code.to_owned(),
            detail: None,
            opened_by: COMPONENT.to_owned(),
            opened_at,
        })
    }

    async fn clear_rail_stop(
        &self,
        _asset_id: Uuid,
        _cleared_by: &str,
        _reason: &str,
        _cleared_at: OffsetDateTime,
    ) -> Result<bool, RepositoryError> {
        Ok(false)
    }

    async fn find_open_rail_stop(
        &self,
        _asset_id: Uuid,
    ) -> Result<Option<RailStop>, RepositoryError> {
        Ok(None)
    }

    async fn record_risk_evaluation(
        &self,
        _submission: &RiskSubmission,
        _submitted_by: Uuid,
    ) -> Result<Uuid, RepositoryError> {
        Ok(Uuid::nil())
    }

    async fn risk_provider_allowed(
        &self,
        _operator_key_id: Uuid,
        _provider: &str,
    ) -> Result<bool, RepositoryError> {
        Ok(false)
    }

    async fn resolve_manual(
        &self,
        _credential: &OperatorCredential,
        _idempotency_key: &str,
        _request_hash: &[u8; 32],
        _resolution: &ManualResolution,
        _decided_at: OffsetDateTime,
    ) -> Result<ManualResolutionResult, OperationsError> {
        Err(OperationsError::ManualResolutionConflict)
    }
}

fn service(repository: Arc<StubRepository>) -> ReconciliationService<StubRepository, FixedClock> {
    ReconciliationService::new(repository, FixedClock, ReconciliationWindow::default())
}

#[tokio::test]
async fn a_clean_run_says_so_and_closes_nothing() -> TestResult {
    let repository = Arc::new(StubRepository::default());
    let report = service(Arc::clone(&repository))
        .run_once(ReconciliationKind::Incremental)
        .await?;

    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(report.transfers_examined, 4);
    assert_eq!(report.intents_examined, 7);
    assert!(repository.stops().is_empty());
    assert_eq!(repository.last_state(), Some(ComponentState::Ok));
    Ok(())
}

#[tokio::test]
async fn money_that_does_not_add_up_closes_the_rail_by_itself() -> TestResult {
    let repository = Arc::new(StubRepository::finding(
        DiscrepancyKind::AllocationExceedsTransfer,
        Some(ASSET),
    ));
    let report = service(Arc::clone(&repository))
        .run_once(ReconciliationKind::Daily)
        .await?;

    assert_eq!(report.status, RunStatus::HardStop);
    assert_eq!(report.money_discrepancies, 1);
    assert_eq!(report.stopped_assets, vec![ASSET]);
    assert_eq!(
        repository.stops(),
        vec![(ASSET, HARD_STOP_REASON.to_owned())]
    );
    assert_eq!(repository.last_state(), Some(ComponentState::Stopped));
    Ok(())
}

#[tokio::test]
async fn a_counter_that_disagrees_is_drift_not_a_stop() -> TestResult {
    let repository = Arc::new(StubRepository::finding(
        DiscrepancyKind::UnmatchedInboundAging,
        Some(ASSET),
    ));
    let report = service(Arc::clone(&repository))
        .run_once(ReconciliationKind::Incremental)
        .await?;

    assert_eq!(report.status, RunStatus::Drift);
    assert_eq!(report.money_discrepancies, 0);
    assert!(
        repository.stops().is_empty(),
        "unmatched money is looked at, not a reason to stop taking payments"
    );
    assert_eq!(repository.last_state(), Some(ComponentState::Degraded));
    Ok(())
}

#[tokio::test]
async fn a_money_finding_with_no_asset_still_stops_the_run() -> TestResult {
    let repository = Arc::new(StubRepository::finding(
        DiscrepancyKind::SettledNotFulfilled,
        None,
    ));
    let report = service(Arc::clone(&repository))
        .run_once(ReconciliationKind::Daily)
        .await?;

    // Nothing names a rail to close, and the run still says hard stop rather
    // than reporting a clean pass it did not have.
    assert_eq!(report.status, RunStatus::HardStop);
    assert!(report.stopped_assets.is_empty());
    assert!(repository.stops().is_empty());
    Ok(())
}

#[test]
fn every_money_finding_is_named_as_one() {
    for kind in [
        DiscrepancyKind::AllocationExceedsTransfer,
        DiscrepancyKind::SettledNotFulfilled,
        DiscrepancyKind::FulfilledNotSettled,
        DiscrepancyKind::AllocatedOnInvalidatedTransfer,
    ] {
        assert!(kind.affects_money(), "{} moves money", kind.as_str());
    }
    for kind in [
        DiscrepancyKind::ObservedNotCanonical,
        DiscrepancyKind::UnmatchedInboundAging,
        DiscrepancyKind::ObserverBehind,
        DiscrepancyKind::HeldPaymentAging,
    ] {
        assert!(!kind.affects_money(), "{} is a counter", kind.as_str());
    }
}
