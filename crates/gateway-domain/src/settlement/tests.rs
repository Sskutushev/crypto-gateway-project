use std::{error::Error, str::FromStr};

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    AttemptCandidate, AttemptStatus, HoldReason, ManualReason, MatchOutcome, MatchStrategy,
    RiskDecision, SettlementEvidence, SettlementOutcome, SettlementPolicy, SettlementTier,
    TransferFacts, decide_settlement, match_transfer,
};

#[test]
fn manual_resolution_names_are_stable_and_explicitly_external() {
    use super::{ManualResolutionAction, RemainderDisposition};

    assert_eq!(ManualResolutionAction::Honor.as_str(), "honor");
    assert_eq!(ManualResolutionAction::Reject.as_str(), "reject");
    assert_eq!(
        ManualResolutionAction::RecordRemainderDisposition.as_str(),
        "record_remainder_disposition"
    );
    assert_eq!(
        RemainderDisposition::RefundedExternally.as_str(),
        "refunded_externally"
    );
}
use crate::{Memo, RawAmount, TransferState};

type TestResult = Result<(), Box<dyn Error>>;

const COLLECTOR: Uuid = Uuid::from_u128(1);
const OTHER_COLLECTOR: Uuid = Uuid::from_u128(2);
const ATTEMPT_ONE: Uuid = Uuid::from_u128(11);
const ATTEMPT_TWO: Uuid = Uuid::from_u128(12);
const INTENT_ONE: Uuid = Uuid::from_u128(21);
const INTENT_TWO: Uuid = Uuid::from_u128(22);
const MERCHANT: Uuid = Uuid::from_u128(31);
const TRANSFER: Uuid = Uuid::from_u128(41);

fn now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
}

fn candidate(
    attempt_id: Uuid,
    intent_id: Uuid,
    amount: &str,
    status: AttemptStatus,
    leased_from: OffsetDateTime,
    leased_until: OffsetDateTime,
) -> Result<AttemptCandidate, Box<dyn Error>> {
    Ok(AttemptCandidate {
        attempt_id,
        payment_intent_id: intent_id,
        merchant_id: MERCHANT,
        collector_address_id: COLLECTOR,
        expected_amount_raw: RawAmount::from_str(amount)?,
        memo_reference: None,
        leased_from,
        leased_until,
        status,
    })
}

fn transfer(amount: &str, block_time: OffsetDateTime) -> Result<TransferFacts, Box<dyn Error>> {
    Ok(TransferFacts {
        transfer_id: TRANSFER,
        collector_address_id: COLLECTOR,
        amount_raw: RawAmount::from_str(amount)?,
        memo: None,
        block_time,
        state: TransferState::Finalized,
    })
}

#[test]
fn an_exact_amount_on_a_live_reservation_matches() -> TestResult {
    let candidates = vec![candidate(
        ATTEMPT_ONE,
        INTENT_ONE,
        "273001427",
        AttemptStatus::AwaitingPayment,
        now() - Duration::minutes(5),
        now() + Duration::days(30),
    )?];

    let outcome = match_transfer(&transfer("273001427", now())?, &candidates);

    assert_eq!(
        outcome,
        MatchOutcome::Matched {
            attempt_id: ATTEMPT_ONE,
            payment_intent_id: INTENT_ONE,
            strategy: MatchStrategy::ExactAmount,
            late: false,
        }
    );
    Ok(())
}

#[test]
fn a_memo_outranks_the_amount() -> TestResult {
    let mut with_memo = candidate(
        ATTEMPT_TWO,
        INTENT_TWO,
        "999999999",
        AttemptStatus::AwaitingPayment,
        now() - Duration::minutes(5),
        now() + Duration::days(30),
    )?;
    with_memo.memo_reference = Some(Memo::new("RP-4FM93AZK")?);
    let candidates = vec![
        candidate(
            ATTEMPT_ONE,
            INTENT_ONE,
            "273001427",
            AttemptStatus::AwaitingPayment,
            now() - Duration::minutes(5),
            now() + Duration::days(30),
        )?,
        with_memo,
    ];
    let mut paid = transfer("273001427", now())?;
    paid.memo = Some(Memo::new("RP-4FM93AZK")?);

    let outcome = match_transfer(&paid, &candidates);

    assert_eq!(
        outcome,
        MatchOutcome::Matched {
            attempt_id: ATTEMPT_TWO,
            payment_intent_id: INTENT_TWO,
            strategy: MatchStrategy::Memo,
            late: false,
        }
    );
    Ok(())
}

#[test]
fn a_payment_after_the_window_matches_the_attempt_that_held_the_slot_then() -> TestResult {
    // The slot was released on day 30 and handed to another attempt on day 31.
    let candidates = vec![
        candidate(
            ATTEMPT_ONE,
            INTENT_ONE,
            "273001427",
            AttemptStatus::Expired,
            now() - Duration::days(35),
            now() - Duration::days(5),
        )?,
        candidate(
            ATTEMPT_TWO,
            INTENT_TWO,
            "273001427",
            AttemptStatus::AwaitingPayment,
            now() - Duration::days(4),
            now() + Duration::days(26),
        )?,
    ];

    // The block was produced on day 33: inside the first attempt's window.
    let outcome = match_transfer(
        &transfer("273001427", now() - Duration::days(7))?,
        &candidates,
    );

    assert_eq!(
        outcome,
        MatchOutcome::Matched {
            attempt_id: ATTEMPT_ONE,
            payment_intent_id: INTENT_ONE,
            strategy: MatchStrategy::HistoricalSlot,
            late: true,
        }
    );
    Ok(())
}

#[test]
fn two_attempts_with_the_same_amount_are_never_guessed_between() -> TestResult {
    let candidates = vec![
        candidate(
            ATTEMPT_ONE,
            INTENT_ONE,
            "273001427",
            AttemptStatus::AwaitingPayment,
            now() - Duration::days(1),
            now() + Duration::days(30),
        )?,
        candidate(
            ATTEMPT_TWO,
            INTENT_TWO,
            "273001427",
            AttemptStatus::AwaitingPayment,
            now() - Duration::days(1),
            now() + Duration::days(30),
        )?,
    ];

    let outcome = match_transfer(&transfer("273001427", now())?, &candidates);

    assert_eq!(
        outcome,
        MatchOutcome::Ambiguous {
            attempt_ids: vec![ATTEMPT_ONE, ATTEMPT_TWO],
        }
    );
    Ok(())
}

#[test]
fn money_that_explains_nothing_is_unmatched_not_discarded() -> TestResult {
    let candidates = vec![candidate(
        ATTEMPT_ONE,
        INTENT_ONE,
        "273001427",
        AttemptStatus::AwaitingPayment,
        now() - Duration::days(1),
        now() + Duration::days(30),
    )?];

    let outcome = match_transfer(&transfer("100000000", now())?, &candidates);

    assert_eq!(outcome, MatchOutcome::Unmatched);
    Ok(())
}

#[test]
fn an_attempt_on_another_collector_address_is_not_a_candidate() -> TestResult {
    let mut elsewhere = candidate(
        ATTEMPT_ONE,
        INTENT_ONE,
        "273001427",
        AttemptStatus::AwaitingPayment,
        now() - Duration::days(1),
        now() + Duration::days(30),
    )?;
    elsewhere.collector_address_id = OTHER_COLLECTOR;

    let outcome = match_transfer(&transfer("273001427", now())?, &[elsewhere]);

    assert_eq!(outcome, MatchOutcome::Unmatched);
    Ok(())
}

fn policy() -> SettlementPolicy {
    SettlementPolicy {
        id: Uuid::from_u128(51),
        version: "settlement-v1".to_owned(),
        tiers: vec![
            SettlementTier {
                max_fiat_minor: 50_000,
                min_independent_groups: 1,
                require_own_node: false,
                require_risk_allow: false,
                auto_settle: true,
            },
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

fn evidence(fiat_minor: i64, arriving: &str) -> Result<SettlementEvidence, Box<dyn Error>> {
    Ok(SettlementEvidence {
        fiat_amount_minor: fiat_minor,
        expected_amount_raw: RawAmount::from_str("273001427")?,
        already_allocated_raw: RawAmount::from_str("1")?,
        transfer_amount_raw: RawAmount::from_str(arriving)?,
        transfer_state: TransferState::Finalized,
        independent_groups: 2,
        had_own_node: false,
        risk: RiskDecision::Allow,
        attempt_status: AttemptStatus::AwaitingPayment,
        late: false,
    })
}

#[test]
fn a_covered_obligation_settles_for_exactly_what_is_owed() -> TestResult {
    let mut facts = evidence(400_000, "273001427")?;
    facts.already_allocated_raw = RawAmount::from_str("1")?;
    facts.transfer_amount_raw = RawAmount::from_str("273001426")?;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::Settle {
            allocate_raw: RawAmount::from_str("273001426")?,
        }
    );
    Ok(())
}

#[test]
fn an_underpayment_allocates_and_waits() -> TestResult {
    let facts = evidence(400_000, "100000000")?;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::Partial {
            allocate_raw: RawAmount::from_str("100000000")?,
        }
    );
    Ok(())
}

#[test]
fn an_overpayment_keeps_the_remainder_visible() -> TestResult {
    let mut facts = evidence(400_000, "273001527")?;
    facts.already_allocated_raw = RawAmount::from_str("1")?;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::Overpaid {
            allocate_raw: RawAmount::from_str("273001426")?,
            remainder_raw: RawAmount::from_str("101")?,
        }
    );
    Ok(())
}

#[test]
fn a_large_amount_waits_for_a_person() -> TestResult {
    let facts = evidence(1_500_000, "273001427")?;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::ManualRequired {
            reason: ManualReason::AboveAutomaticBand,
        }
    );
    Ok(())
}

#[test]
fn an_amount_no_band_covers_never_falls_into_the_smallest_band() -> TestResult {
    let facts = evidence(99_000_000, "273001427")?;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::ManualRequired {
            reason: ManualReason::UncoveredAmount,
        }
    );
    Ok(())
}

#[test]
fn one_source_is_not_enough_for_a_mid_sized_payment() -> TestResult {
    let mut facts = evidence(400_000, "273001427")?;
    facts.independent_groups = 1;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::ManualRequired {
            reason: ManualReason::NotEnoughIndependentGroups,
        }
    );
    Ok(())
}

#[test]
fn an_unscreened_payment_is_not_treated_as_a_clean_one() -> TestResult {
    let mut facts = evidence(400_000, "273001427")?;
    facts.risk = RiskDecision::Skipped;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::ManualRequired {
            reason: ManualReason::RiskReview,
        }
    );
    Ok(())
}

#[test]
fn a_denied_source_of_funds_holds_the_money() -> TestResult {
    let mut facts = evidence(10_000, "273001427")?;
    facts.risk = RiskDecision::Deny;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::Hold {
            reason: HoldReason::RiskDenied,
        }
    );
    Ok(())
}

#[test]
fn nothing_settles_before_finality() -> TestResult {
    let mut facts = evidence(10_000, "273001427")?;
    facts.transfer_state = TransferState::Confirmed;

    assert_eq!(
        decide_settlement(&facts, &policy())?,
        SettlementOutcome::Hold {
            reason: HoldReason::NotFinal,
        }
    );

    facts.transfer_state = TransferState::Invalidated;
    assert_eq!(
        decide_settlement(&facts, &policy())?,
        SettlementOutcome::Hold {
            reason: HoldReason::Invalidated,
        }
    );
    Ok(())
}

#[test]
fn an_already_covered_obligation_does_not_take_more_money() -> TestResult {
    let mut facts = evidence(10_000, "273001427")?;
    facts.already_allocated_raw = RawAmount::from_str("273001427")?;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::Hold {
            reason: HoldReason::AlreadyCovered,
        }
    );
    Ok(())
}

#[test]
fn a_late_payment_is_a_decision_for_a_person() -> TestResult {
    let mut facts = evidence(10_000, "273001427")?;
    facts.late = true;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::ManualRequired {
            reason: ManualReason::LatePayment,
        }
    );
    Ok(())
}

#[test]
fn a_cancelled_attempt_does_not_absorb_a_payment() -> TestResult {
    let mut facts = evidence(10_000, "273001427")?;
    facts.attempt_status = AttemptStatus::Cancelled;

    let outcome = decide_settlement(&facts, &policy())?;

    assert_eq!(
        outcome,
        SettlementOutcome::Hold {
            reason: HoldReason::Cancelled,
        }
    );
    Ok(())
}
