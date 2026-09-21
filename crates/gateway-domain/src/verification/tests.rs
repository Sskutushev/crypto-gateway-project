use std::{error::Error, str::FromStr};

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    AttestationRole, ConflictField, DiscardReason, EvidenceReading, FinalityPolicy,
    InsufficientReason, RejectionReason, Verdict, verify,
};
use crate::{
    AddressKey, ChainEnvironment, ExecutionStatus, Memo, ObservationKind, ObservedTransfer,
    RawAmount, SourceFinality, TransferState, TxHash,
};

type TestResult = Result<(), Box<dyn Error>>;

const ASSET_ID: Uuid = Uuid::from_u128(1);
const COLLECTOR_ID: Uuid = Uuid::from_u128(2);

fn now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
}

fn policy() -> FinalityPolicy {
    FinalityPolicy {
        id: Uuid::from_u128(3),
        version: "finality-v1".to_owned(),
        min_confirmations: 19,
        required_source_finality: SourceFinality::Finalized,
        min_independent_groups: 2,
        max_evidence_age_seconds: 3_600,
        observed_at: now(),
    }
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
    id: u128,
    group: &str,
    kind: ObservationKind,
    transfer: ObservedTransfer,
) -> EvidenceReading {
    EvidenceReading {
        observation_id: Uuid::from_u128(id),
        source_id: Uuid::from_u128(id + 1_000),
        provider_group: group.to_owned(),
        source_kind: "indexed_api".to_owned(),
        source_principal: format!("principal_{group}"),
        declared_principal: format!("principal_{group}"),
        kind,
        asset_id: Some(ASSET_ID),
        collector_address_id: Some(COLLECTOR_ID),
        transfer,
        observed_at: now() - Duration::minutes(1),
    }
}

fn agreeing_evidence() -> Result<Vec<EvidenceReading>, Box<dyn Error>> {
    Ok(vec![
        reading(10, "group-a", ObservationKind::CursorScan, transfer()?),
        reading(20, "group-b", ObservationKind::CursorScan, transfer()?),
        reading(30, "group-a", ObservationKind::TargetedLookup, transfer()?),
    ])
}

#[test]
fn agreeing_independent_evidence_becomes_a_canonical_fact() -> TestResult {
    let verdict = verify(&agreeing_evidence()?, &policy(), now())?;

    let Verdict::Verified(verified) = verdict else {
        return Err(format!("expected a verified transfer, got {verdict:?}").into());
    };
    assert_eq!(verified.independent_groups, 2);
    assert_eq!(verified.state, TransferState::Finalized);
    assert_eq!(verified.confirmations, 30);
    assert_eq!(verified.transfer.amount_raw.to_string(), "273001427");
    assert_eq!(verified.transfer.block_hash, "block-100");
    assert!(!verified.had_own_node);
    assert_eq!(verified.attestations.len(), 3);
    assert_eq!(
        verified
            .attestations
            .iter()
            .filter(|(_, role)| *role == AttestationRole::Reverify)
            .count(),
        1
    );
    assert_eq!(
        verified
            .attestations
            .iter()
            .filter(|(_, role)| *role == AttestationRole::Finality)
            .count(),
        2
    );
    Ok(())
}

#[test]
fn two_keys_of_one_provider_are_one_source() -> TestResult {
    let same_group = vec![
        reading(10, "group-a", ObservationKind::CursorScan, transfer()?),
        reading(20, "group-a", ObservationKind::FastDetect, transfer()?),
        reading(30, "group-a", ObservationKind::TargetedLookup, transfer()?),
    ];

    let verdict = verify(&same_group, &policy(), now())?;

    assert_eq!(
        verdict,
        Verdict::Insufficient {
            reason: InsufficientReason::NotEnoughIndependentGroups { have: 1, need: 2 },
            discarded: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn a_fact_needs_the_verifiers_own_re_read() -> TestResult {
    let without_reread = vec![
        reading(10, "group-a", ObservationKind::CursorScan, transfer()?),
        reading(20, "group-b", ObservationKind::CursorScan, transfer()?),
    ];

    let verdict = verify(&without_reread, &policy(), now())?;

    assert_eq!(
        verdict,
        Verdict::Insufficient {
            reason: InsufficientReason::NoIndependentReread,
            discarded: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn a_forged_principal_is_discarded_and_leaves_too_little_evidence() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    if let Some(forged) = evidence.get_mut(1) {
        forged.source_principal = "gateway_observer_a".to_owned();
        forged.declared_principal = "gateway_observer_b".to_owned();
    }

    let verdict = verify(&evidence, &policy(), now())?;

    let Verdict::Insufficient { reason, discarded } = verdict else {
        return Err("a forged reading must not support a fact".into());
    };
    assert_eq!(
        reason,
        InsufficientReason::NotEnoughIndependentGroups { have: 1, need: 2 }
    );
    assert_eq!(discarded.len(), 1);
    assert_eq!(
        discarded.first().map(|entry| entry.reason),
        Some(DiscardReason::ImpersonatedSource)
    );
    Ok(())
}

#[test]
fn a_compromised_source_cannot_invent_a_payment_on_its_own() -> TestResult {
    let fabricated = ObservedTransfer {
        amount_raw: RawAmount::from_str("50000000000")?,
        ..transfer()?
    };
    let evidence = vec![
        reading(10, "group-a", ObservationKind::CursorScan, fabricated),
        reading(20, "group-b", ObservationKind::CursorScan, transfer()?),
        reading(30, "group-b", ObservationKind::TargetedLookup, transfer()?),
    ];

    let verdict = verify(&evidence, &policy(), now())?;

    let Verdict::Conflicted { conflicts, .. } = verdict else {
        return Err("disagreeing sources must not produce a fact".into());
    };
    assert_eq!(
        conflicts.first().map(|conflict| conflict.field),
        Some(ConflictField::AmountRaw)
    );
    assert_eq!(
        conflicts.first().map(|conflict| conflict.items.len()),
        Some(3)
    );
    Ok(())
}

#[test]
fn a_token_that_merely_calls_itself_usdt_is_rejected() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    for entry in &mut evidence {
        entry.asset_id = None;
        entry.transfer.token_key = AddressKey::new([8_u8; 20])?;
        entry.transfer.token_display = "USDT".to_owned();
    }

    let verdict = verify(&evidence, &policy(), now())?;

    assert_eq!(
        verdict,
        Verdict::Rejected {
            reason: RejectionReason::UnsupportedAsset,
            discarded: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn a_failed_transaction_is_rejected() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    for entry in &mut evidence {
        entry.transfer.execution_status = ExecutionStatus::Failed;
    }

    let verdict = verify(&evidence, &policy(), now())?;

    assert_eq!(
        verdict,
        Verdict::Rejected {
            reason: RejectionReason::FailedExecution,
            discarded: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn shallow_confirmation_depth_is_canonical_but_not_final() -> TestResult {
    let shallow = ObservedTransfer {
        source_head: Some(105),
        source_finality: SourceFinality::Confirmed,
        ..transfer()?
    };
    let evidence = vec![
        reading(10, "group-a", ObservationKind::CursorScan, shallow.clone()),
        reading(20, "group-b", ObservationKind::CursorScan, shallow.clone()),
        reading(30, "group-b", ObservationKind::TargetedLookup, shallow),
    ];

    let verdict = verify(&evidence, &policy(), now())?;

    let Verdict::Verified(verified) = verdict else {
        return Err("agreeing evidence should still produce a fact".into());
    };
    assert_eq!(verified.state, TransferState::Confirmed);
    assert_eq!(verified.confirmations, 5);
    Ok(())
}

#[test]
fn stale_evidence_does_not_become_a_fact() -> TestResult {
    let verdict = verify(&agreeing_evidence()?, &policy(), now() + Duration::hours(3))?;

    assert_eq!(
        verdict,
        Verdict::Insufficient {
            reason: InsufficientReason::StaleEvidence,
            discarded: Vec::new(),
        }
    );
    Ok(())
}

#[test]
fn a_memo_disagreement_is_a_conflict_not_a_choice() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    if let Some(entry) = evidence.get_mut(0) {
        entry.transfer.memo = Some(Memo::new("RP-4FM93AZK")?);
    }

    let verdict = verify(&evidence, &policy(), now())?;

    let Verdict::Conflicted { conflicts, .. } = verdict else {
        return Err("a memo disagreement must be recorded".into());
    };
    assert!(
        conflicts
            .iter()
            .any(|conflict| conflict.field == ConflictField::Memo)
    );
    Ok(())
}

#[test]
fn a_fast_lane_reading_without_a_block_is_incomplete_not_contradictory() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    if let Some(entry) = evidence.get_mut(0) {
        entry.transfer.block_hash = None;
        entry.transfer.block_number = None;
        entry.transfer.block_time = None;
        entry.kind = ObservationKind::FastDetect;
    }

    let verdict = verify(&evidence, &policy(), now())?;

    let Verdict::Verified(verified) = verdict else {
        return Err("an incomplete fast reading must not block a fact".into());
    };
    assert_eq!(verified.transfer.block_number, 100);
    Ok(())
}

#[test]
fn readings_about_different_events_are_a_programming_error() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    if let Some(entry) = evidence.get_mut(0) {
        entry.transfer.event_index = 7;
    }

    assert!(verify(&evidence, &policy(), now()).is_err());
    Ok(())
}

#[test]
fn an_own_node_attestation_is_visible_to_the_settlement_policy() -> TestResult {
    let mut evidence = agreeing_evidence()?;
    if let Some(entry) = evidence.get_mut(2) {
        entry.source_kind = "own_node".to_owned();
    }

    let verdict = verify(&evidence, &policy(), now())?;

    let Verdict::Verified(verified) = verdict else {
        return Err("expected a verified transfer".into());
    };
    assert!(verified.had_own_node);
    Ok(())
}
