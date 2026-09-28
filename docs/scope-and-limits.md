# Scope and limits

What the gateway does today, what it does not do, and what happens in each
exceptional payment case. Every behaviour below is read from the code on this
branch; where the code has a known gap, it is stated.

## Scope

| | Supported now | Not supported |
|---|---|---|
| Chains and tokens | USDT TRC20 on TRON (mainnet or Nile testnet), one allowlisted contract per asset. | Other chains and tokens. ERC20 and TON are planned as separate adapters behind the same observer and verifier interface; none exists yet. |
| Direction | Incoming payments only. | Outgoing payouts, withdrawals, balances. |
| Keys | None held. The gateway watches addresses; it never signs. | Key custody of any kind. |
| Collectors | Merchant-owned (`collector_policy = 'own'`, the default for new merchants) or operator-owned (`'shared'`, where the operator owes the merchant outside this system). No fallback between the two. A merchant may have several addresses; quotes spread over them, least loaded first. | Per-payment deposit addresses. |
| Matching on TRON | Exact `amount_raw` at the quoted collector, inside the lease window. | Memo matching: the TRON adapter records no memo, so a memo never matches on TRON. |
| Refunds | Recording a refund or other disposition done outside the gateway. | Automatic refunds; the gateway never sends funds. |
| Intent lifecycle | One live attempt per intent. An intent whose quote ran out with no money, claim or settlement decision on it is quoted again on the same intent and reference. `POST /v1/payment-intents/{id}/cancel` cancels an intent that has no money, hold or decision on it. | Cancelling an intent that already has money, a hold or a decision on it. |
| Screening | Accepting a screening result per transfer from a bound provider key. | A built-in KYT provider integration. |

## Exceptional cases on TRON

"Held" and "unmatched" transfers are listed by
`GET /v1/operator/held-payments` and `GET /v1/operator/unmatched-transfers`
and resolved by `POST /v1/operator/manual-resolutions`. While a transfer waits
there, the intent keeps its current status: the code never sets `risk_hold`.

| Case | What the gateway does | Merchant sees |
|---|---|---|
| Quote expires unpaid | At `expires_at` the expiry worker moves the attempt and the intent from `awaiting_payment` to `expired` (audit events only, no webhook). The amount stays reserved until `late_payment_until`, then moves to lease history. While no money, claim or decision exists, a new quote on the same intent issues a new attempt; the earlier attempt keeps its reservation for late money. | `expired` on `GET`. To retry, quote the same intent again; the reference and order stay the same. |
| Underpayment | A smaller amount does not equal the reservation, so the transfer is `unmatched` (unless it happens to equal another open reservation on the same collector, in which case it matches that one). An operator may `honor` it: the allocation must equal `min(outstanding, unallocated transfer amount)`, the intent becomes `partially_paid`, and a `payment_intent.partially_paid` webhook is written. The remaining amount does not equal the reservation either, so a top-up transfer is also `unmatched` and needs another `honor`. | `partially_paid` and a `payment_intent.partially_paid` webhook only after an operator acts. Never fulfil on it. |
| Overpayment | A larger amount does not equal the reservation, so the transfer is `unmatched`. An operator may `honor` it for the outstanding amount: the intent becomes `paid`, and both `payment_intent.paid` and `OVERPAID` (with `remainder_raw`) are written in one transaction. The remainder stays on the transfer until an operator records `record_remainder_disposition`. | `paid` after the operator acts, plus an `OVERPAID` webhook naming the remainder. |
| Wrong network | Observers read only the configured chain, network and environment; a reading from another is refused (`ForeignChain`, `ForeignEnvironment`). USDT sent on another chain is never seen. | Nothing. Recovery is between the payer and whoever holds the collector's key. |
| Wrong token on TRON | A TRC20 transfer from a contract that is not the asset's allowlisted contract is stored as an observation with no asset and never becomes a canonical transfer, so it is not in the unmatched queue either. The adapter reads TRC20 `Transfer` event logs only; a native TRX transfer produces none. | Nothing. Recovery is outside the gateway. |
| Late payment inside the window | The exact amount, sent before `late_payment_until` to an attempt that is no longer `awaiting_payment`, matches that attempt and is marked late; the decision is `manual_required` / `late_payment` and the transfer is `held`. An operator may `honor` it while the intent is still payable; the intent then becomes `paid` and one `payment_intent.paid` is written. Lateness is judged by the block time against the quote's `expires_at`, so an exact payment made before `expires_at` settles automatically even when the expiry worker ran first. | `expired`, then `paid` and the webhook if an operator honours it. |
| Late payment outside the window | Sent at or after `late_payment_until`: the slot history does not cover the block time, so the transfer is `unmatched`. `honor` is refused with `409` because the block time is after `late_payment_until`. The operator can only `reject` it and handle the money outside the gateway. | Nothing; the intent stays `expired`. |
| Second payment of the exact amount | The lease is kept until `late_payment_until` even after settlement, so a second exact payment inside that window matches the already `settled` attempt, is marked late and `held`. `honor` is refused because the attempt is settled; the operator can `reject` it. After the window it is `unmatched`. | Nothing new. |
| Refund handled outside the system | The gateway sends no funds. For an overpayment remainder the operator records `record_remainder_disposition` with `refunded_externally` (or `credited_externally`, `donated_externally`, `retained_by_agreement`) and an `external_reference`; the stored remainder must match exactly. For a whole held or unmatched transfer the operator uses `reject`, which records the reason and closes the transfer without a typed disposition. | No webhook for either. |
| Cancelled order receiving money | The merchant cancels through `POST /v1/payment-intents/{id}/cancel`; the reservation stays until `late_payment_until`. If money then arrives, the paid transition refuses (`require_one_row`), everything rolls back, and the transfer is `held` with `payment_not_payable`. `honor` is refused for a `cancelled` intent. The operator can `reject` and refund outside the gateway. | Status stays `cancelled`; no webhook. |
| Money that matches two reservations | Recorded as `ambiguous`, parked as `unmatched`, operator event; no guess. | Nothing until an operator acts. |
| Not yet final, invalidated, or screening `deny` | Nothing settles before `finalized`; an invalidated transfer or a `deny` screening is `held`. | Nothing. |

## Known gaps

None of the cases above has an open gap. The expiry stall on a partially paid
intent is fixed: the attempt always expires, the intent keeps
`partially_paid` for a person, and the PostgreSQL scenario
`a_partially_paid_intent_never_stops_the_expiry_of_the_others` covers it.
