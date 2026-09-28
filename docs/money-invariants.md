# Money invariants

Each invariant is a sentence a test can falsify, followed by the mechanism
that enforces it and the tests that exercise it. Where the coverage has a gap,
the gap is stated.

Test locations use these short names:

| Short name | File |
|---|---|
| domain | `crates/gateway-domain/src/settlement/tests.rs` |
| settlement | `crates/gateway-storage/src/settlement/tests.rs` |
| operations | `crates/gateway-storage/src/operations/tests.rs` |
| postgres | `crates/gateway-storage/src/postgres.rs` (`mod tests`) |
| outbox | `crates/gateway-application/src/outbox/tests.rs` |
| webhook | `crates/gateway-webhook/src/lib.rs` (`mod tests`) |

Tests in `settlement`, `operations` and `postgres` are `#[ignore]`d PostgreSQL
scenarios; they run with `GATEWAY_TEST_DATABASE_URL` set and
`cargo test -- --ignored`.

## 1. A transfer pays only an attempt on the same collector, asset and rail

**Invariant.** No allocation or claim links a transfer to an attempt unless the
transfer arrived at that attempt's collector address, in its quote's asset, on
its quote's chain, network and environment.

**Mechanism.**

- Automatic path: `match_candidates` (storage `settlement.rs`) selects only
  leases on the transfer's `collector_address_id`, and `match_transfer`
  (domain `settlement.rs`) filters candidates to that collector again.
- Manual path: `honor_transfer` (storage `operations.rs`) reads the binding
  from locked rows, never from the request: `collector_address_id`,
  `asset_id`, `chain`, `network` and `chain_environment` must all equal the
  attempt's quote (`bound_to_attempt`, checked in `honor_is_bound`).
- Database: migration `0014_allocation_collector_binding.sql` adds
  `chain_transfers (collector_address_id, asset_id) → collector_addresses (id,
  asset_id)`, and composite foreign keys that make both `payment_allocations`
  and `chain_transfer_intent_claims` name one collector that the attempt and
  the transfer must both be on. A violation (`23503`) becomes
  `TransitionRefused` in `binding_refused`, which is never retried.

**Tests.** operations: `honor_refuses_money_that_arrived_in_another_asset`,
`honor_refuses_money_that_arrived_at_another_collector`,
`the_database_itself_refuses_an_allocation_across_collectors` (a hand-written
`INSERT` is refused by the foreign key), and
`honor_refuses_an_attempt_paired_with_another_merchants_intent`. domain:
`an_attempt_on_another_collector_address_is_not_a_candidate`.

**Gap.** The chain, network and environment comparison is explicit only in the
manual path. The automatic path relies on the collector fixing the asset
(0014) and the asset fixing the chain; no automatic-path test submits a
transfer in another asset.

## 2. An intent becomes paid or partially paid only from an explicit state, by exactly one row

**Invariant.** `payment_intents.status` moves to `paid` or `partially_paid`
only from `awaiting_payment`, `partially_paid`, `risk_hold` or `expired`, and
the attempt only from `awaiting_payment` or `expired`. If either update does
not change exactly one row, the whole settlement transaction is abandoned
before a fulfilment claim, an event or a webhook is written.

**Mechanism.** `PAYABLE_INTENT_STATES`, `ALLOCATABLE_ATTEMPT_STATES` and
`require_one_row` in `crates/gateway-storage/src/settlement.rs`, used by
`advance_to_paid` and `advance_to_partially_paid`. A refusal is
`TransitionRefused`: the automatic path rolls back and `park_refused` moves the
transfer to `held` with outcome `manual_required` and reason
`payment_not_payable: …`; the manual path answers `409
manual_resolution_conflict`. `honor_is_bound` applies the same state lists
before any write.

**Tests.** operations: `honor_never_reopens_a_cancelled_or_paid_intent`.
settlement: `money_for_an_intent_cancelled_before_settlement_goes_to_a_person`
(no allocation, claim, fulfilment or `payment_intent.*` event is written; the
transfer is held).

**Gap.** No test drives the refusal through `advance_to_partially_paid`
specifically.

## 3. One transfer never pays two obligations

**Invariant.** A transfer is claimed by at most one payment intent, and the sum
of its allocations never exceeds its amount.

**Mechanism.** `chain_transfer_intent_claims.transfer_id` is the primary key
(migration 0008); `claim_transfer` inserts with `ON CONFLICT DO NOTHING`,
reads the owner back, and a foreign owner parks the transfer as `held` with
decision `rejected` / `claimed_by_another_intent`. The allocation update is
guarded in SQL: `allocated_raw + x <= transfer.amount_raw`, and zero updated
rows is an error. `payment_allocations` is unique on `(attempt_id,
transfer_id)` and checks `allocated_raw > 0`.

**Tests.** settlement: `one_transfer_can_never_pay_two_obligations` (a forced
second settlement against another intent returns `ForeignClaim`),
`a_verified_payment_settles_once_and_only_once`. operations:
`a_second_honor_under_a_new_key_is_refused_and_pays_nothing_twice`. domain:
`two_attempts_with_the_same_amount_are_never_guessed_between`.

## 4. A late payment is honoured only inside its window

**Invariant.** A transfer whose attempt is no longer `awaiting_payment`, or
whose block time is at or after the lease end, is never settled
automatically. An operator can honour it only if its block time is not after
the attempt's `late_payment_until`.

**Mechanism.** The amount lease lasts until `late_payment_until`
(`amount_lease.allocated` in `postgres.rs`). `match_transfer` marks a match
`late` when the attempt is not `awaiting_payment` or the block is at or after
the lease end; a slot already released is matched by the block's time against
`amount_lease_history`, so it is never credited to whoever holds the slot
today. `decide_settlement` turns `late` into `ManualRequired { LatePayment }`.
`honor_transfer` requires `t.block_time <= a.late_payment_until`
(`within_late_window`).

**Tests.** operations: `honor_takes_a_late_payment_only_inside_its_window`
(outside: `409`, nothing moves; inside: intent `paid`, attempt `settled`, one
`payment_intent.paid`). domain: `a_late_payment_is_a_decision_for_a_person`,
`a_payment_after_the_window_matches_the_attempt_that_held_the_slot_then`.

**Notes.** "Late" is decided by the attempt's status at settlement time, not
by comparing the block time with `expires_at`: an exact payment sent before
`expires_at` but finalized after the expiry worker marked the attempt
`expired` goes to an operator. The automatic match uses `block_time <
lease_until`; the honor check uses `block_time <= late_payment_until`, so a
block at exactly `late_payment_until` is unmatched automatically but can be
honoured.

## 5. A merchant on collector policy `own` is quoted only on its own address

**Invariant.** A quote for a merchant whose `collector_policy` is `own` names a
collector registered to that merchant; for `shared`, an operator collector
(`merchant_id IS NULL`). A merchant on `own` with no active collector of its
own gets no quote.

**Mechanism.** Migration `0015_merchant_owned_collectors.sql`: the
`collector_policy` column (default `own` for new merchants; merchants that
existed before the migration were set to `shared`), `collector_addresses.
merchant_id`, and the `BEFORE INSERT` trigger `payment_quotes_collector_tenancy`
that raises `23514` for any other pairing. The quote path applies the same rule
in `load_quote_context` and fails with `CollectorUnavailable`.

**Tests.** postgres:
`a_merchant_on_its_own_policy_is_quoted_only_on_its_own_address` (each merchant
gets its own address, a merchant without one gets no quote, and a plan naming
another merchant's address is refused on insert).

## 6. Webhook egress never goes through a proxy or to a non-public address

**Invariant.** A signed event is sent only over HTTPS to port 443, directly,
to addresses that are all public, with no redirect followed.

**Mechanism.** `crates/gateway-webhook/src/lib.rs`: `validate_url` (https,
port 443, no credentials, query or fragment); `approve_answers` rejects the
whole DNS answer if any address is non-public (`is_public_ip`, with IPv6 as an
allow-range inside `2000::/3`); the approved addresses are pinned with
`resolve_to_addrs`; the client is built with `.no_proxy()`,
`.https_only(true)` and `redirect::Policy::none()`; DNS resolution is bounded
by `DNS_TIMEOUT`.

**Tests.** webhook: `delivery_never_goes_through_a_system_proxy` (a child
process with `HTTPS_PROXY`/`ALL_PROXY` set never reaches the trap listener),
`only_public_addresses_are_approved`,
`an_empty_or_mixed_dns_answer_is_refused_as_a_whole`,
`endpoint_url_policy_is_strict`, `a_plain_http_endpoint_is_never_called`,
`a_resolver_that_never_answers_is_cut_off`.

**Gap.** No test exercises a `3xx` answer; the refusal to follow redirects
rests on the client configuration.

## 7. `payment_intent.paid` exists only after a permitted transition, at most once per intent

**Invariant.** A `payment_intent.paid` outbox row is written only in the
transaction that moved the attempt to `settled` and the intent to `paid`
under invariant 2, and at most once per payment intent.

**Mechanism.** In `advance_to_paid` the attempt and intent updates must each
change one row before anything else is written. The fulfilment row
`payment_fulfillments` has `payment_intent_id` as its primary key and is
inserted with `ON CONFLICT DO NOTHING`; the outbox row is written only when
that insert changed one row. Everything is one transaction with the
allocation. The delivered envelope keeps the outbox id, so retries carry the
same event id.

**Tests.** settlement: `a_verified_payment_settles_once_and_only_once` (one
event, and a second run examines nothing),
`money_for_an_intent_cancelled_before_settlement_goes_to_a_person` (no event).
operations: `manual_honor_is_atomic_idempotent_and_cannot_be_retargeted`
(concurrent identical commands produce one event),
`a_second_honor_under_a_new_key_is_refused_and_pays_nothing_twice`,
`honor_takes_a_late_payment_only_inside_its_window`.

**Note.** Delivery is at least once: the same event id can reach an endpoint
more than once (outbox tests `a_refused_delivery_is_retried_later`,
`an_exhausted_event_is_dead_lettered_instead_of_retried_forever`). "At most
once" holds for the event, not for its deliveries.

## 8. Money nobody can attribute is kept, never absorbed

**Invariant.** A finalized transfer that matches no attempt, or more than one,
is recorded as `unmatched` with an operator event; it moves no intent.

**Mechanism.** `record_unresolved` in storage `settlement.rs` sets
`processing_state = 'unmatched'`, writes `UNMATCHED_INBOUND` or
`AMBIGUOUS_MATCH` to `payment_events`, and enqueues an `operator` channel
outbox row (never a merchant webhook).

**Tests.** settlement: `an_overpayment_keeps_the_remainder_for_a_person` (on
TRON an amount other than the reserved one is `unmatched`, with no
allocation). domain: `money_that_explains_nothing_is_unmatched_not_discarded`,
`two_attempts_with_the_same_amount_are_never_guessed_between`.

## 9. An overpayment remainder is never absorbed and never reported as refunded by the gateway

**Invariant.** When more arrives than is owed, only the outstanding amount is
allocated; the remainder is recorded and announced, and its later disposition
is recorded as an action taken outside the gateway.

**Mechanism.** `decide_settlement` returns `Overpaid { allocate_raw,
remainder_raw }`; `record_remainder` writes an `OVERPAID` payment event and an
`OVERPAID` webhook with `remainder_raw`. `record_remainder_disposition`
requires the exact stored remainder and one of `refunded_externally`,
`credited_externally`, `donated_externally`, `retained_by_agreement`
(migration 0012), with a non-empty `external_reference`.

**Tests.** domain: `an_overpayment_keeps_the_remainder_visible`,
`manual_resolution_names_are_stable_and_explicitly_external`. operations:
`overpayment_disposition_records_external_action_without_claiming_a_refund`.

## 10. Nothing settles before finality, and an unscreened payment is not a clean one

**Invariant.** A transfer that is not `finalized` moves no money; a missing or
expired screening counts as `skipped`, never `allow`.

**Mechanism.** `transfers_awaiting_settlement` selects only `finalized` or
`invalidated` transfers, and `decide_settlement` holds anything not
`finalized`. `latest_risk` returns `Skipped` when no evaluation from the last
hour exists; a band with `require_risk_allow` then needs a person.

**Tests.** domain: `nothing_settles_before_finality`,
`an_unscreened_payment_is_not_treated_as_a_clean_one`,
`a_denied_source_of_funds_holds_the_money`.
