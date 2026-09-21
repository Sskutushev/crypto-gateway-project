use std::{error::Error, str::FromStr, sync::Arc};

use async_trait::async_trait;
use gateway_domain::{
    AddressKey, ChainEnvironment, ExecutionStatus, ObservationKind, ObservedTransfer, RawAmount,
    SourceFinality, TxHash,
};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ChainSource, CollectorState, CollectorWatch, ComponentLease, CursorKind, CursorPosition,
    IntakeReport, ObservationRepository, ObservationService, RefusalReason, ResolvedObservation,
    SourceKind, SourceState,
};
use crate::{Clock, RepositoryError};

type TestResult = Result<(), Box<dyn Error>>;

const SOURCE_ID: Uuid = Uuid::from_u128(1);
const COLLECTOR_ID: Uuid = Uuid::from_u128(2);
const ASSET_ID: Uuid = Uuid::from_u128(3);

#[derive(Debug, Default)]
struct RecordingRepository {
    recorded: std::sync::Mutex<Vec<ResolvedObservation>>,
}

impl RecordingRepository {
    fn recorded(&self) -> Vec<ResolvedObservation> {
        self.recorded
            .lock()
            .map(|recorded| recorded.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl ObservationRepository for RecordingRepository {
    async fn find_source(&self, _source_key: &str) -> Result<Option<ChainSource>, RepositoryError> {
        Ok(None)
    }

    async fn watched_collectors(
        &self,
        _chain: &str,
        _network: &str,
        _environment: ChainEnvironment,
    ) -> Result<Vec<CollectorWatch>, RepositoryError> {
        Ok(Vec::new())
    }

    async fn find_cursor(
        &self,
        _source_id: Uuid,
        _kind: ObservationKind,
        _collector_address_id: Uuid,
    ) -> Result<Option<CursorPosition>, RepositoryError> {
        Ok(None)
    }

    async fn record_observations(
        &self,
        _source: &ChainSource,
        _lease: &ComponentLease,
        observations: &[ResolvedObservation],
        cursor: Option<(Uuid, ObservationKind, CursorPosition)>,
    ) -> Result<IntakeReport, RepositoryError> {
        let mut recorded = self
            .recorded
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?;
        recorded.extend(observations.iter().cloned());
        Ok(IntakeReport {
            recorded: u32::try_from(observations.len()).unwrap_or(u32::MAX),
            duplicates: 0,
            refused: 0,
            cursor_advanced: cursor.is_some(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }
}

fn source() -> ChainSource {
    ChainSource {
        id: SOURCE_ID,
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        source_key: "provider-a".to_owned(),
        provider_group: "group-a".to_owned(),
        kind: SourceKind::IndexedApi,
        db_principal: "gateway".to_owned(),
        state: SourceState::Active,
    }
}

fn collector(state: CollectorState) -> Result<CollectorWatch, Box<dyn Error>> {
    Ok(CollectorWatch {
        collector_address_id: COLLECTOR_ID,
        address_key: AddressKey::new([3_u8; 21])?,
        address_text: "TCollector".to_owned(),
        asset_id: ASSET_ID,
        token_key: AddressKey::new([7_u8; 20])?,
        decimals: 6,
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        state,
    })
}

fn transfer() -> Result<ObservedTransfer, Box<dyn Error>> {
    Ok(ObservedTransfer {
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        tx_hash: TxHash::new("abc123")?,
        event_index: 0,
        block_number: Some(10),
        block_hash: Some("block-10".to_owned()),
        parent_hash: None,
        block_time: Some(OffsetDateTime::UNIX_EPOCH),
        token_key: AddressKey::new([7_u8; 20])?,
        token_display: "USDT".to_owned(),
        from_address: AddressKey::new([9_u8; 21])?,
        from_address_text: "TFrom".to_owned(),
        to_address: AddressKey::new([3_u8; 21])?,
        to_address_text: "TCollector".to_owned(),
        amount_raw: RawAmount::from_str("1000")?,
        decimals: 6,
        memo: None,
        execution_status: ExecutionStatus::Success,
        source_finality: SourceFinality::Seen,
        source_head: Some(20),
        evidence_sha256: "a".repeat(64),
        evidence_uri: None,
    })
}

fn lease() -> ComponentLease {
    ComponentLease {
        component: "observer:tron:provider-a".to_owned(),
        holder: "pod-1:boot-1".to_owned(),
        fence_token: 7,
        lease_until: OffsetDateTime::UNIX_EPOCH,
    }
}

fn service() -> (
    Arc<RecordingRepository>,
    ObservationService<RecordingRepository, FixedClock>,
) {
    let repository = Arc::new(RecordingRepository::default());
    let service = ObservationService::new(
        Arc::clone(&repository),
        FixedClock,
        "observer-test",
        "parser-test",
    );
    (repository, service)
}

#[tokio::test]
async fn records_a_reading_for_a_watched_collector() -> TestResult {
    let (repository, service) = service();
    let collectors = vec![collector(CollectorState::Active)?];

    let (report, refused) = service
        .intake(
            &source(),
            &lease(),
            ObservationKind::CursorScan,
            &collectors,
            vec![transfer()?],
            Some((
                COLLECTOR_ID,
                CursorPosition::new(CursorKind::Block, "10", None, 7)?,
            )),
        )
        .await?;

    assert_eq!(report.recorded, 1);
    assert_eq!(report.refused, 0);
    assert!(report.cursor_advanced);
    assert!(refused.is_empty());
    let recorded = repository.recorded();
    let first = recorded.first().ok_or("no observation was recorded")?;
    assert_eq!(first.asset_id, Some(ASSET_ID));
    assert_eq!(first.collector_address_id, COLLECTOR_ID);
    assert_eq!(first.fence_token, 7);
    assert_eq!(first.observer_version, "observer-test");
    Ok(())
}

#[tokio::test]
async fn refuses_readings_that_are_not_about_this_gateway() -> TestResult {
    let (repository, service) = service();
    let collectors = vec![collector(CollectorState::Active)?];
    let base = transfer()?;
    let transfers = vec![
        ObservedTransfer {
            chain_environment: ChainEnvironment::Mainnet,
            ..base.clone()
        },
        ObservedTransfer {
            network: "shasta".to_owned(),
            ..base.clone()
        },
        ObservedTransfer {
            to_address: AddressKey::new([4_u8; 21])?,
            ..base
        },
    ];

    let (report, refused) = service
        .intake(
            &source(),
            &lease(),
            ObservationKind::CursorScan,
            &collectors,
            transfers,
            None,
        )
        .await?;

    assert_eq!(report.recorded, 0);
    assert_eq!(report.refused, 3);
    assert!(repository.recorded().is_empty());
    let reasons: Vec<RefusalReason> = refused.into_iter().map(|(_, reason)| reason).collect();
    assert_eq!(
        reasons,
        vec![
            RefusalReason::ForeignEnvironment,
            RefusalReason::ForeignChain,
            RefusalReason::ForeignRecipient,
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_retired_collector_is_never_listened_to() -> TestResult {
    let (repository, service) = service();
    let collectors = vec![collector(CollectorState::Retired)?];

    let (report, refused) = service
        .intake(
            &source(),
            &lease(),
            ObservationKind::CursorScan,
            &collectors,
            vec![transfer()?],
            None,
        )
        .await?;

    assert_eq!(report.recorded, 0);
    assert!(repository.recorded().is_empty());
    assert_eq!(
        refused.first().map(|(_, reason)| *reason),
        Some(RefusalReason::RetiredCollector)
    );
    Ok(())
}

#[tokio::test]
async fn an_impostor_token_is_kept_as_evidence_without_an_asset() -> TestResult {
    let (repository, service) = service();
    let collectors = vec![collector(CollectorState::Active)?];
    let impostor = ObservedTransfer {
        token_key: AddressKey::new([8_u8; 20])?,
        token_display: "USDT".to_owned(),
        ..transfer()?
    };

    let (report, refused) = service
        .intake(
            &source(),
            &lease(),
            ObservationKind::CursorScan,
            &collectors,
            vec![impostor],
            None,
        )
        .await?;

    assert_eq!(report.recorded, 1);
    assert!(refused.is_empty());
    let recorded = repository.recorded();
    let first = recorded.first().ok_or("no observation was recorded")?;
    assert_eq!(first.asset_id, None);
    Ok(())
}

#[tokio::test]
async fn a_failed_transaction_is_still_recorded_as_evidence() -> TestResult {
    let (repository, service) = service();
    let collectors = vec![collector(CollectorState::Active)?];
    let failed = ObservedTransfer {
        execution_status: ExecutionStatus::Failed,
        ..transfer()?
    };

    let (report, _refused) = service
        .intake(
            &source(),
            &lease(),
            ObservationKind::FastDetect,
            &collectors,
            vec![failed],
            None,
        )
        .await?;

    assert_eq!(report.recorded, 1);
    let recorded = repository.recorded();
    let first = recorded.first().ok_or("no observation was recorded")?;
    assert_eq!(first.transfer.execution_status, ExecutionStatus::Failed);
    assert_eq!(first.kind, ObservationKind::FastDetect);
    Ok(())
}

#[test]
fn cursor_values_must_look_like_chain_positions() {
    assert!(CursorPosition::new(CursorKind::Block, "123", None, 1).is_ok());
    assert!(CursorPosition::new(CursorKind::Block, "12a", None, 1).is_err());
    assert!(CursorPosition::new(CursorKind::Block, "", None, 1).is_err());
    assert!(CursorPosition::new(CursorKind::EventPosition, "7:3", None, 1).is_ok());
    assert!(CursorPosition::new(CursorKind::Block, "123", None, 0).is_err());
}
