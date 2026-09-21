use std::{
    error::Error,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use gateway_domain::{
    AttemptCandidate, AttemptStatus, CurrencyCode, HoldReason, ManualReason, MatchStrategy,
    RawAmount, RiskDecision, SettlementOutcome, SettlementPolicy, SettlementTier, TransferFacts,
    TransferState,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    AttemptSnapshot, PendingTransfer, SettlementCommand, SettlementRecord, SettlementRepository,
    SettlementService, UnresolvedTransfer,
};
use crate::{Clock, ComponentLease, RepositoryError};

type TestResult = Result<(), Box<dyn Error>>;

const TRANSFER: Uuid = Uuid::from_u128(1);
const ATTEMPT_ONE: Uuid = Uuid::from_u128(2);
const ATTEMPT_TWO: Uuid = Uuid::from_u128(3);
const INTENT: Uuid = Uuid::from_u128(4);
const MERCHANT: Uuid = Uuid::from_u128(5);
const COLLECTOR: Uuid = Uuid::from_u128(6);

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
    transfers: Vec<PendingTransfer>,
    candidates: Vec<AttemptCandidate>,
    snapshot: Option<AttemptSnapshot>,
    policy: Option<SettlementPolicy>,
    risk: RiskDecision,
    commands: Mutex<Vec<SettlementCommand>>,
    unresolved: Mutex<Vec<String>>,
}

impl FakeRepository {
    fn new(
        transfers: Vec<PendingTransfer>,
        candidates: Vec<AttemptCandidate>,
        snapshot: Option<AttemptSnapshot>,
    ) -> Arc<Self> {
        Arc::new(Self {
            transfers,
            candidates,
            snapshot,
            policy: Some(policy()),
            risk: RiskDecision::Allow,
            commands: Mutex::new(Vec::new()),
            unresolved: Mutex::new(Vec::new()),
        })
    }

    fn commands(&self) -> Vec<SettlementCommand> {
        self.commands
            .lock()
            .map(|commands| commands.clone())
            .unwrap_or_default()
    }

    fn unresolved(&self) -> Vec<String> {
        self.unresolved
            .lock()
            .map(|unresolved| unresolved.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl SettlementRepository for FakeRepository {
    async fn transfers_awaiting_settlement(
        &self,
        _limit: u32,
    ) -> Result<Vec<PendingTransfer>, RepositoryError> {
        Ok(self.transfers.clone())
    }

    async fn match_candidates(
        &self,
        _transfer: &TransferFacts,
    ) -> Result<Vec<AttemptCandidate>, RepositoryError> {
        Ok(self.candidates.clone())
    }

    async fn attempt_snapshot(
        &self,
        _attempt_id: Uuid,
    ) -> Result<Option<AttemptSnapshot>, RepositoryError> {
        Ok(self.snapshot.clone())
    }

    async fn find_settlement_policy(
        &self,
        _currency: &CurrencyCode,
    ) -> Result<Option<SettlementPolicy>, RepositoryError> {
        Ok(self.policy.clone())
    }

    async fn latest_risk(
        &self,
        _transfer_id: Uuid,
    ) -> Result<(RiskDecision, Option<Uuid>), RepositoryError> {
        Ok((self.risk, None))
    }

    async fn settle(
        &self,
        _lease: &ComponentLease,
        command: &SettlementCommand,
    ) -> Result<SettlementRecord, RepositoryError> {
        self.commands
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push(command.clone());
        Ok(match command.outcome {
            SettlementOutcome::Settle { .. } => SettlementRecord::Settled,
            SettlementOutcome::Partial { .. } => SettlementRecord::PartiallyAllocated,
            SettlementOutcome::Overpaid { .. } => SettlementRecord::Overpaid,
            SettlementOutcome::Hold { .. } => SettlementRecord::Held,
            SettlementOutcome::ManualRequired { .. } => SettlementRecord::ManualRequired,
        })
    }

    async fn record_unresolved(
        &self,
        _lease: &ComponentLease,
        _transfer: &PendingTransfer,
        unresolved: &UnresolvedTransfer,
    ) -> Result<(), RepositoryError> {
        self.unresolved
            .lock()
            .map_err(|_| RepositoryError::Unavailable("poisoned test lock".to_owned()))?
            .push(unresolved.as_str().to_owned());
        Ok(())
    }
}

fn policy() -> SettlementPolicy {
    SettlementPolicy {
        id: Uuid::from_u128(9),
        version: "settlement-v1".to_owned(),
        tiers: vec![
            SettlementTier {
                max_fiat_minor: 500_000,
                min_independent_groups: 2,
                require_own_node: false,
                require_risk_allow: true,
                auto_settle: true,
            },
            SettlementTier {
                max_fiat_minor: 6_000_000,
                min_independent_groups: 2,
                require_own_node: true,
                require_risk_allow: true,
                auto_settle: false,
            },
        ],
    }
}

fn transfer(amount: &str) -> Result<PendingTransfer, Box<dyn Error>> {
    Ok(PendingTransfer {
        facts: TransferFacts {
            transfer_id: TRANSFER,
            collector_address_id: COLLECTOR,
            amount_raw: RawAmount::from_str(amount)?,
            memo: None,
            block_time: now(),
            state: TransferState::Finalized,
        },
        independent_groups: 2,
        had_own_node: false,
        attestation_ids: vec![Uuid::from_u128(7), Uuid::from_u128(8)],
        allocated_raw: RawAmount::ZERO,
    })
}

fn candidate(attempt_id: Uuid, amount: &str) -> Result<AttemptCandidate, Box<dyn Error>> {
    Ok(AttemptCandidate {
        attempt_id,
        payment_intent_id: INTENT,
        merchant_id: MERCHANT,
        collector_address_id: COLLECTOR,
        expected_amount_raw: RawAmount::from_str(amount)?,
        memo_reference: None,
        leased_from: now() - Duration::hours(1),
        leased_until: now() + Duration::days(30),
        status: AttemptStatus::AwaitingPayment,
    })
}

fn snapshot(fiat_minor: i64, expected: &str) -> Result<AttemptSnapshot, Box<dyn Error>> {
    Ok(AttemptSnapshot {
        attempt_id: ATTEMPT_ONE,
        payment_intent_id: INTENT,
        merchant_id: MERCHANT,
        currency: CurrencyCode::new("USD")?,
        fiat_amount_minor: fiat_minor,
        expected_amount_raw: RawAmount::from_str(expected)?,
        allocated_raw: RawAmount::ZERO,
        status: AttemptStatus::AwaitingPayment,
    })
}

fn lease() -> ComponentLease {
    ComponentLease {
        component: "settlement".to_owned(),
        holder: "pod-settlement:boot-1".to_owned(),
        fence_token: 2,
        lease_until: now() + Duration::seconds(30),
    }
}

#[tokio::test]
async fn a_covered_obligation_is_settled_with_its_evidence_recorded() -> TestResult {
    let repository = FakeRepository::new(
        vec![transfer("273001427")?],
        vec![candidate(ATTEMPT_ONE, "273001427")?],
        Some(snapshot(400_000, "273001427")?),
    );
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.settled, 1);
    let commands = repository.commands();
    let command = commands.first().ok_or("no settlement was attempted")?;
    assert_eq!(command.match_strategy, MatchStrategy::ExactAmount);
    assert_eq!(command.independent_groups, 2);
    assert_eq!(command.attestation_ids.len(), 2);
    assert_eq!(command.policy_version, "settlement-v1");
    assert_eq!(
        command.outcome,
        SettlementOutcome::Settle {
            allocate_raw: RawAmount::from_str("273001427")?,
        }
    );
    Ok(())
}

#[tokio::test]
async fn an_underpayment_allocates_without_fulfilling() -> TestResult {
    let repository = FakeRepository::new(
        vec![transfer("100000000")?],
        vec![candidate(ATTEMPT_ONE, "100000000")?],
        Some(snapshot(400_000, "273001427")?),
    );
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.partial, 1);
    assert_eq!(report.settled, 0);
    Ok(())
}

#[tokio::test]
async fn two_matching_obligations_are_never_guessed_between() -> TestResult {
    let repository = FakeRepository::new(
        vec![transfer("273001427")?],
        vec![
            candidate(ATTEMPT_ONE, "273001427")?,
            candidate(ATTEMPT_TWO, "273001427")?,
        ],
        Some(snapshot(400_000, "273001427")?),
    );
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.ambiguous, 1);
    assert!(repository.commands().is_empty());
    assert_eq!(repository.unresolved(), vec!["ambiguous".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn money_no_obligation_explains_is_recorded_not_dropped() -> TestResult {
    let repository = FakeRepository::new(
        vec![transfer("999999999")?],
        vec![candidate(ATTEMPT_ONE, "273001427")?],
        Some(snapshot(400_000, "273001427")?),
    );
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.unmatched, 1);
    assert_eq!(repository.unresolved(), vec!["unmatched".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn a_large_payment_stops_for_a_person() -> TestResult {
    let repository = FakeRepository::new(
        vec![transfer("273001427")?],
        vec![candidate(ATTEMPT_ONE, "273001427")?],
        Some(snapshot(1_500_000, "273001427")?),
    );
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.manual_required, 1);
    let commands = repository.commands();
    let command = commands.first().ok_or("no decision was recorded")?;
    assert_eq!(
        command.outcome,
        SettlementOutcome::ManualRequired {
            reason: ManualReason::AboveAutomaticBand,
        }
    );
    Ok(())
}

#[tokio::test]
async fn an_unfinal_transfer_is_held() -> TestResult {
    let mut pending = transfer("273001427")?;
    pending.facts.state = TransferState::Confirmed;
    let repository = FakeRepository::new(
        vec![pending],
        vec![candidate(ATTEMPT_ONE, "273001427")?],
        Some(snapshot(400_000, "273001427")?),
    );
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.held, 1);
    let commands = repository.commands();
    let command = commands.first().ok_or("no decision was recorded")?;
    assert_eq!(
        command.outcome,
        SettlementOutcome::Hold {
            reason: HoldReason::NotFinal,
        }
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_settlement_policy_stops_the_money() -> TestResult {
    let repository = Arc::new(FakeRepository {
        transfers: vec![transfer("273001427")?],
        candidates: vec![candidate(ATTEMPT_ONE, "273001427")?],
        snapshot: Some(snapshot(400_000, "273001427")?),
        policy: None,
        risk: RiskDecision::Allow,
        commands: Mutex::new(Vec::new()),
        unresolved: Mutex::new(Vec::new()),
    });
    let service = SettlementService::new(Arc::clone(&repository), FixedClock);

    let report = service.settle_pending(&lease(), 10).await?;

    assert_eq!(report.missing_policy, 1);
    assert_eq!(report.settled, 0);
    assert!(repository.commands().is_empty());
    Ok(())
}

#[tokio::test]
async fn an_invalid_batch_limit_is_refused() -> TestResult {
    let repository = FakeRepository::new(Vec::new(), Vec::new(), None);
    let service = SettlementService::new(repository, FixedClock);

    assert!(service.settle_pending(&lease(), 0).await.is_err());
    assert!(service.settle_pending(&lease(), 2_000).await.is_err());
    Ok(())
}
