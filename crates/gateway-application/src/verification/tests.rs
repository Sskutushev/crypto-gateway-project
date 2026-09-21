use std::{
    error::Error,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_domain::{
    AddressKey, ChainEnvironment, EvidenceReading, ExecutionStatus, FinalityPolicy,
    ObservationKind, ObservedTransfer, RawAmount, SourceFinality, TxHash, Verdict,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    ChainEventKey, ChainReader, ChainReaderError, VerdictOutcome, VerificationRepository,
    VerificationService,
};
use crate::{
    ChainSource, Clock, CollectorState, CollectorWatch, ComponentLease, CursorPosition,
    IntakeReport, ObservationRepository, RepositoryError, ResolvedObservation, SourceKind,
    SourceState,
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET_ID: Uuid = Uuid::from_u128(1);
const COLLECTOR_ID: Uuid = Uuid::from_u128(2);
const VERIFIER_SOURCE: Uuid = Uuid::from_u128(3);
const PROVIDER_A: Uuid = Uuid::from_u128(4);
const PROVIDER_B: Uuid = Uuid::from_u128(5);

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

#[derive(Debug)]
struct FakeRepository {
    event: ChainEventKey,
    evidence: Mutex<Vec<EvidenceReading>>,
    recorded: Mutex<Vec<ResolvedObservation>>,
    verdicts: Mutex<Vec<(ChainEventKey, String)>>,
    policy: Mutex<Option<FinalityPolicy>>,
}

impl FakeRepository {
    fn with(
        evidence: Vec<EvidenceReading>,
        policy: Option<FinalityPolicy>,
    ) -> Result<Arc<Self>, Box<dyn Error>> {
        Ok(Arc::new(Self {
            event: event()?,
            evidence: Mutex::new(evidence),
            recorded: Mutex::new(Vec::new()),
            verdicts: Mutex::new(Vec::new()),
            policy: Mutex::new(policy),
        }))
    }

    fn verdicts(&self) -> Vec<String> {
        self.verdicts
            .lock()
            .map(|verdicts| verdicts.iter().map(|(_, name)| name.clone()).collect())
            .unwrap_or_default()
    }

    fn recorded(&self) -> Vec<ResolvedObservation> {
        self.recorded
            .lock()
            .map(|recorded| recorded.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl VerificationRepository for FakeRepository {
    async fn find_finality_policy(
        &self,
        _chain: &str,
        _network: &str,
        _environment: ChainEnvironment,
    ) -> Result<Option<FinalityPolicy>, RepositoryError> {
        Ok(self
            .policy
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .clone())
    }

    async fn events_awaiting_verdict(
        &self,
        _limit: u32,
    ) -> Result<Vec<ChainEventKey>, RepositoryError> {
        Ok(vec![self.event.clone()])
    }

    async fn evidence_for(
        &self,
        _event: &ChainEventKey,
    ) -> Result<Vec<EvidenceReading>, RepositoryError> {
        Ok(self
            .evidence
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .clone())
    }

    async fn commit_verdict(
        &self,
        _lease: &ComponentLease,
        event: &ChainEventKey,
        verdict: &Verdict,
        _evidence_count: u32,
        _verifier_version: &str,
        _now: OffsetDateTime,
    ) -> Result<VerdictOutcome, RepositoryError> {
        let name = match verdict {
            Verdict::Verified(_) => "verified",
            Verdict::Conflicted { .. } => "conflicted",
            Verdict::Insufficient { .. } => "insufficient",
            Verdict::Rejected { .. } => "rejected",
        };
        self.verdicts
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push((event.clone(), name.to_owned()));
        Ok(VerdictOutcome::Pending)
    }
}

#[async_trait]
impl ObservationRepository for FakeRepository {
    async fn find_source(&self, _source_key: &str) -> Result<Option<ChainSource>, RepositoryError> {
        Ok(None)
    }

    async fn watched_collectors(
        &self,
        _chain: &str,
        _network: &str,
        _environment: ChainEnvironment,
    ) -> Result<Vec<CollectorWatch>, RepositoryError> {
        collector()
            .map(|collector| vec![collector])
            .map_err(|_| RepositoryError::CorruptData("test fixture".to_owned()))
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
        _cursor: Option<(Uuid, ObservationKind, CursorPosition)>,
    ) -> Result<IntakeReport, RepositoryError> {
        let mut recorded = self
            .recorded
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?;
        recorded.extend(observations.iter().cloned());
        // The verifier reloads evidence after its own re-read, so the fake
        // makes the new reading visible the same way storage would.
        let mut evidence = self
            .evidence
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?;
        for observation in observations {
            evidence.push(EvidenceReading {
                observation_id: Uuid::now_v7(),
                source_id: VERIFIER_SOURCE,
                provider_group: "verifier".to_owned(),
                source_kind: "own_node".to_owned(),
                source_principal: "gateway_verifier".to_owned(),
                declared_principal: "gateway_verifier".to_owned(),
                kind: observation.kind,
                asset_id: observation.asset_id,
                collector_address_id: Some(observation.collector_address_id),
                transfer: observation.transfer.clone(),
                observed_at: observation.observed_at,
            });
        }
        Ok(IntakeReport {
            recorded: u32::try_from(observations.len()).unwrap_or(u32::MAX),
            duplicates: 0,
            refused: 0,
            cursor_advanced: false,
        })
    }
}

#[derive(Debug)]
struct ScriptedReader {
    answer: Option<ObservedTransfer>,
    failure: Option<&'static str>,
    calls: Mutex<u32>,
}

impl ScriptedReader {
    fn answering(transfer: ObservedTransfer) -> Arc<Self> {
        Arc::new(Self {
            answer: Some(transfer),
            failure: None,
            calls: Mutex::new(0),
        })
    }

    fn blind() -> Arc<Self> {
        Arc::new(Self {
            answer: None,
            failure: None,
            calls: Mutex::new(0),
        })
    }

    fn unreachable() -> Arc<Self> {
        Arc::new(Self {
            answer: None,
            failure: Some("connection refused"),
            calls: Mutex::new(0),
        })
    }

    fn calls(&self) -> u32 {
        self.calls.lock().map(|calls| *calls).unwrap_or_default()
    }
}

#[async_trait]
impl ChainReader for ScriptedReader {
    async fn lookup(
        &self,
        _event: &ChainEventKey,
    ) -> Result<Option<ObservedTransfer>, ChainReaderError> {
        if let Ok(mut calls) = self.calls.lock() {
            *calls = calls.saturating_add(1);
        }
        if let Some(failure) = self.failure {
            return Err(ChainReaderError::Unreachable(failure.to_owned()));
        }
        Ok(self.answer.clone())
    }
}

fn event() -> Result<ChainEventKey, Box<dyn Error>> {
    Ok(ChainEventKey {
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        tx_hash: TxHash::new("abc123")?,
        event_index: 0,
    })
}

fn policy() -> FinalityPolicy {
    FinalityPolicy {
        id: Uuid::from_u128(9),
        version: "finality-v1".to_owned(),
        min_confirmations: 19,
        required_source_finality: SourceFinality::Finalized,
        min_independent_groups: 2,
        max_evidence_age_seconds: 3_600,
        observed_at: now(),
    }
}

fn collector() -> Result<CollectorWatch, Box<dyn Error>> {
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
        state: CollectorState::Active,
    })
}

fn transfer() -> Result<ObservedTransfer, Box<dyn Error>> {
    Ok(ObservedTransfer {
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        tx_hash: TxHash::new("abc123")?,
        event_index: 0,
        block_number: Some(100),
        block_hash: Some("block-100".to_owned()),
        parent_hash: Some("block-99".to_owned()),
        block_time: Some(now() - Duration::minutes(5)),
        token_key: AddressKey::new([7_u8; 20])?,
        token_display: "USDT".to_owned(),
        from_address: AddressKey::new([9_u8; 21])?,
        from_address_text: "TFrom".to_owned(),
        to_address: AddressKey::new([3_u8; 21])?,
        to_address_text: "TCollector".to_owned(),
        amount_raw: RawAmount::from_str("273001427")?,
        decimals: 6,
        memo: None,
        execution_status: ExecutionStatus::Success,
        source_finality: SourceFinality::Finalized,
        source_head: Some(130),
        evidence_sha256: "a".repeat(64),
        evidence_uri: None,
    })
}

fn reading(
    observation_id: u128,
    source: Uuid,
    group: &str,
    kind: ObservationKind,
) -> Result<EvidenceReading, Box<dyn Error>> {
    Ok(EvidenceReading {
        observation_id: Uuid::from_u128(observation_id),
        source_id: source,
        provider_group: group.to_owned(),
        source_kind: "indexed_api".to_owned(),
        source_principal: format!("principal_{group}"),
        declared_principal: format!("principal_{group}"),
        kind,
        asset_id: Some(ASSET_ID),
        collector_address_id: Some(COLLECTOR_ID),
        transfer: transfer()?,
        observed_at: now() - Duration::minutes(1),
    })
}

fn lease() -> ComponentLease {
    ComponentLease {
        component: "verifier:tron".to_owned(),
        holder: "pod-verifier:boot-1".to_owned(),
        fence_token: 4,
        lease_until: now() + Duration::seconds(30),
    }
}

fn verifier_source() -> ChainSource {
    ChainSource {
        id: VERIFIER_SOURCE,
        chain: "tron".to_owned(),
        network: "nile".to_owned(),
        chain_environment: ChainEnvironment::Testnet,
        source_key: "verifier".to_owned(),
        provider_group: "verifier".to_owned(),
        kind: SourceKind::OwnNode,
        db_principal: "gateway_verifier".to_owned(),
        state: SourceState::Active,
    }
}

fn service<K: ChainReader>(
    repository: Arc<FakeRepository>,
    reader: Arc<K>,
) -> VerificationService<FakeRepository, K, FixedClock> {
    VerificationService::new(
        repository,
        reader,
        FixedClock,
        verifier_source(),
        "verifier-test",
        "parser-test",
    )
}

#[tokio::test]
async fn the_verifier_reads_the_chain_itself_before_deciding() -> TestResult {
    let repository = FakeRepository::with(
        vec![
            reading(101, PROVIDER_A, "group-a", ObservationKind::CursorScan)?,
            reading(102, PROVIDER_B, "group-b", ObservationKind::CursorScan)?,
        ],
        Some(policy()),
    )?;
    let reader = ScriptedReader::answering(transfer()?);
    let service = service(Arc::clone(&repository), Arc::clone(&reader));

    let report = service.verify_pending(&lease(), 10).await?;

    assert_eq!(reader.calls(), 1);
    assert_eq!(report.rereads_performed, 1);
    assert_eq!(report.verified, 1);
    assert_eq!(repository.verdicts(), vec!["verified".to_owned()]);
    let recorded = repository.recorded();
    let own_reading = recorded.first().ok_or("the re-read was not stored")?;
    assert_eq!(own_reading.kind, ObservationKind::TargetedLookup);
    assert_eq!(own_reading.fence_token, 4);
    assert_eq!(own_reading.asset_id, Some(ASSET_ID));
    Ok(())
}

#[tokio::test]
async fn evidence_the_chain_does_not_confirm_never_becomes_a_fact() -> TestResult {
    let repository = FakeRepository::with(
        vec![
            reading(101, PROVIDER_A, "group-a", ObservationKind::CursorScan)?,
            reading(102, PROVIDER_B, "group-b", ObservationKind::CursorScan)?,
        ],
        Some(policy()),
    )?;
    let reader = ScriptedReader::blind();
    let service = service(Arc::clone(&repository), Arc::clone(&reader));

    let report = service.verify_pending(&lease(), 10).await?;

    assert_eq!(report.rereads_failed, 1);
    assert_eq!(report.verified, 0);
    assert_eq!(report.insufficient, 1);
    assert_eq!(repository.verdicts(), vec!["insufficient".to_owned()]);
    assert!(repository.recorded().is_empty());
    Ok(())
}

#[tokio::test]
async fn an_unreachable_chain_postpones_the_decision_instead_of_making_one() -> TestResult {
    let repository = FakeRepository::with(
        vec![reading(
            101,
            PROVIDER_A,
            "group-a",
            ObservationKind::CursorScan,
        )?],
        Some(policy()),
    )?;
    let reader = ScriptedReader::unreachable();
    let service = service(Arc::clone(&repository), Arc::clone(&reader));

    let report = service.verify_pending(&lease(), 10).await?;

    assert_eq!(report.examined, 1);
    assert_eq!(report.rereads_failed, 1);
    assert_eq!(report.verified, 0);
    assert!(repository.verdicts().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_missing_finality_policy_closes_the_path() -> TestResult {
    let repository = FakeRepository::with(
        vec![reading(
            101,
            PROVIDER_A,
            "group-a",
            ObservationKind::CursorScan,
        )?],
        None,
    )?;
    let reader = ScriptedReader::answering(transfer()?);
    let service = service(Arc::clone(&repository), Arc::clone(&reader));

    let report = service.verify_pending(&lease(), 10).await?;

    assert_eq!(report.missing_policy, 1);
    assert_eq!(report.verified, 0);
    assert_eq!(reader.calls(), 0);
    assert!(repository.verdicts().is_empty());
    Ok(())
}

#[tokio::test]
async fn the_verifier_does_not_re_read_what_it_already_read() -> TestResult {
    let mut own = reading(
        103,
        VERIFIER_SOURCE,
        "verifier",
        ObservationKind::TargetedLookup,
    )?;
    own.source_principal = "gateway_verifier".to_owned();
    own.declared_principal = "gateway_verifier".to_owned();
    let repository = FakeRepository::with(
        vec![
            reading(101, PROVIDER_A, "group-a", ObservationKind::CursorScan)?,
            own,
        ],
        Some(policy()),
    )?;
    let reader = ScriptedReader::answering(transfer()?);
    let service = service(Arc::clone(&repository), Arc::clone(&reader));

    let report = service.verify_pending(&lease(), 10).await?;

    assert_eq!(reader.calls(), 0);
    assert_eq!(report.verified, 1);
    Ok(())
}

#[tokio::test]
async fn an_invalid_batch_limit_is_refused() -> TestResult {
    let repository = FakeRepository::with(Vec::new(), Some(policy()))?;
    let reader = ScriptedReader::blind();
    let service = service(repository, reader);

    assert!(service.verify_pending(&lease(), 0).await.is_err());
    assert!(service.verify_pending(&lease(), 5_000).await.is_err());
    Ok(())
}
