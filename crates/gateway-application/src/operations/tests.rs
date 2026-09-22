use std::{
    error::Error,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_domain::{
    CurrencyCode, PriceAggregationError, PriceAggregationPolicy, PriceReading, RailHealth,
    RawAmount, RiskDecision,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    OperationsError, OperationsRepository, OperationsService, OperatorCredential, OperatorScope,
    PriceIngestion, PriceOutcome, RailStop, RecordedPrice, RiskSubmission,
};
use crate::{Clock, RepositoryError};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET: Uuid = Uuid::from_u128(11);

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
    policy: Option<PriceAggregationPolicy>,
    ingestions: Mutex<Vec<PriceIngestion>>,
    open_stop: Mutex<bool>,
    cleared: Mutex<Vec<String>>,
}

impl StubRepository {
    fn with_policy() -> Self {
        Self {
            policy: Some(PriceAggregationPolicy {
                min_sources: 2,
                max_age_seconds: 300,
                max_deviation_bps: 200,
            }),
            ..Self::default()
        }
    }

    fn recorded(&self) -> Vec<PriceIngestion> {
        self.ingestions
            .lock()
            .map(|ingestions| ingestions.clone())
            .unwrap_or_default()
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
        Ok(self.policy)
    }

    async fn record_price_ingestion(
        &self,
        ingestion: &PriceIngestion,
    ) -> Result<Option<RecordedPrice>, RepositoryError> {
        if let Ok(mut recorded) = self.ingestions.lock() {
            recorded.push(ingestion.clone());
        }
        match &ingestion.outcome {
            PriceOutcome::Aggregated(aggregated) => Ok(Some(RecordedPrice {
                snapshot_id: Uuid::from_u128(99),
                rate_numerator: aggregated.rate_numerator.to_string(),
                rate_denominator: aggregated.rate_denominator.to_string(),
                group_count: aggregated.group_count,
                deviation_bps: aggregated.deviation_bps,
                observed_at: ingestion.received_at,
            })),
            PriceOutcome::Refused(_) => Ok(None),
        }
    }

    async fn record_rail_health(
        &self,
        _asset_id: Uuid,
        _health: RailHealth,
        _detail: Option<String>,
        _ingested_by: Uuid,
        _observed_at: OffsetDateTime,
    ) -> Result<Uuid, RepositoryError> {
        Ok(Uuid::from_u128(77))
    }

    async fn open_rail_stop(
        &self,
        asset_id: Uuid,
        reason_code: &str,
        detail: Option<&str>,
        opened_by: &str,
        opened_at: OffsetDateTime,
    ) -> Result<RailStop, RepositoryError> {
        if let Ok(mut open) = self.open_stop.lock() {
            *open = true;
        }
        Ok(RailStop {
            id: Uuid::from_u128(55),
            asset_id,
            reason_code: reason_code.to_owned(),
            detail: detail.map(ToOwned::to_owned),
            opened_by: opened_by.to_owned(),
            opened_at,
        })
    }

    async fn clear_rail_stop(
        &self,
        _asset_id: Uuid,
        cleared_by: &str,
        reason: &str,
        _cleared_at: OffsetDateTime,
    ) -> Result<bool, RepositoryError> {
        let was_open = self.open_stop.lock().map(|open| *open).unwrap_or(false);
        if was_open {
            if let Ok(mut open) = self.open_stop.lock() {
                *open = false;
            }
            if let Ok(mut cleared) = self.cleared.lock() {
                cleared.push(format!("{cleared_by}:{reason}"));
            }
        }
        Ok(was_open)
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
        Ok(Uuid::from_u128(88))
    }
}

fn credential(scopes: &[OperatorScope]) -> OperatorCredential {
    OperatorCredential {
        key_id: Uuid::from_u128(1),
        label: "price-feeder".to_owned(),
        scopes: scopes.to_vec(),
    }
}

fn reading(group: &str, numerator: &str) -> Result<PriceReading, Box<dyn Error>> {
    Ok(PriceReading {
        source_key: format!("{group}-key"),
        provider_group: group.to_owned(),
        rate_numerator: RawAmount::from_str(numerator)?,
        rate_denominator: RawAmount::from_str("10000")?,
        observed_at: now() - Duration::seconds(5),
    })
}

fn service(repository: StubRepository) -> OperationsService<StubRepository, FixedClock> {
    OperationsService::new(Arc::new(repository), FixedClock)
}

#[tokio::test]
async fn a_key_without_the_scope_changes_nothing() -> TestResult {
    let service = service(StubRepository::with_policy());
    let read_only = credential(&[OperatorScope::Read]);

    let refused = service
        .submit_price(
            &read_only,
            ASSET,
            &CurrencyCode::new("AED")?,
            vec![reading("alpha", "2728")?, reading("beta", "2728")?],
        )
        .await;

    assert!(matches!(
        refused,
        Err(OperationsError::MissingScope(OperatorScope::Ingest))
    ));

    let stop = service
        .open_rail_stop(&credential(&[OperatorScope::Ingest]), ASSET, "manual", None)
        .await;
    assert!(matches!(
        stop,
        Err(OperationsError::MissingScope(OperatorScope::Admin))
    ));
    Ok(())
}

#[tokio::test]
async fn an_agreed_price_is_stored_with_the_readings_behind_it() -> TestResult {
    let repository = Arc::new(StubRepository::with_policy());
    let service = OperationsService::new(Arc::clone(&repository), FixedClock);

    let recorded = service
        .submit_price(
            &credential(&[OperatorScope::Ingest]),
            ASSET,
            &CurrencyCode::new("AED")?,
            vec![reading("alpha", "2727")?, reading("beta", "2729")?],
        )
        .await?;

    assert_eq!(recorded.group_count, 2);
    assert_eq!(recorded.deviation_bps, 8);
    let ingestions = repository.recorded();
    assert_eq!(ingestions.len(), 1);
    assert!(matches!(ingestions[0].outcome, PriceOutcome::Aggregated(_)));
    assert_eq!(ingestions[0].readings.len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_disagreement_is_recorded_and_refused_not_averaged() -> TestResult {
    let repository = Arc::new(StubRepository::with_policy());
    let service = OperationsService::new(Arc::clone(&repository), FixedClock);

    let outcome = service
        .submit_price(
            &credential(&[OperatorScope::Ingest]),
            ASSET,
            &CurrencyCode::new("AED")?,
            vec![reading("alpha", "2728")?, reading("beta", "2900")?],
        )
        .await;

    assert!(matches!(
        outcome,
        Err(OperationsError::Price(
            PriceAggregationError::Diverged { .. }
        ))
    ));
    // The refusal is a record: the readings that disagreed are stored with the
    // reason, so the operator can see which source drifted.
    let ingestions = repository.recorded();
    assert_eq!(ingestions.len(), 1);
    assert_eq!(
        ingestions[0].outcome,
        PriceOutcome::Refused("diverged".to_owned())
    );
    assert_eq!(ingestions[0].readings.len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_price_without_a_policy_is_refused_rather_than_guessed() -> TestResult {
    let service = service(StubRepository::default());

    let outcome = service
        .submit_price(
            &credential(&[OperatorScope::Ingest]),
            ASSET,
            &CurrencyCode::new("AED")?,
            vec![reading("alpha", "2728")?, reading("beta", "2728")?],
        )
        .await;

    assert!(matches!(outcome, Err(OperationsError::NoPricePolicy)));
    Ok(())
}

#[tokio::test]
async fn reopening_a_rail_needs_an_open_stop_and_a_reason() -> TestResult {
    let repository = Arc::new(StubRepository::with_policy());
    let service = OperationsService::new(Arc::clone(&repository), FixedClock);
    let admin = credential(&[OperatorScope::Admin]);

    assert!(matches!(
        service.clear_rail_stop(&admin, ASSET, "explained").await,
        Err(OperationsError::NoOpenRailStop)
    ));

    let stop = service
        .open_rail_stop(&admin, ASSET, "reconciliation_drift", Some("AED 12 short"))
        .await?;
    assert_eq!(stop.reason_code, "reconciliation_drift");
    assert_eq!(stop.opened_by, "price-feeder");

    assert!(matches!(
        service.clear_rail_stop(&admin, ASSET, "   ").await,
        Err(OperationsError::ReasonRequired)
    ));
    service
        .clear_rail_stop(&admin, ASSET, "counted twice, corrected")
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_screening_decision_is_attributable() -> TestResult {
    let service = service(StubRepository::with_policy());

    let id = service
        .submit_risk_evaluation(
            &credential(&[OperatorScope::Ingest]),
            &RiskSubmission {
                transfer_id: Uuid::from_u128(4),
                provider: "example-kyt".to_owned(),
                decision: RiskDecision::Allow,
                score: Some(12),
                reasons: serde_json::json!({"lists": []}),
                evaluated_at: now(),
            },
        )
        .await?;

    assert_eq!(id, Uuid::from_u128(88));
    Ok(())
}
