# Threat model

What the design refuses, one class at a time, and the module that refuses it.
Every item here has a test that tries the attack; the gates fail when it stops
being refused.

## A provider lies, or two providers disagree

A blockchain data provider is an observation, not a fact. The observer
(`gateway-application::observations`) can only append what it saw under its
own database identity; the verifier (`gateway-application::verification`)
creates a canonical transfer only when readings from independent provider
groups agree with each other and with its own re-read through a third
provider, and the evidence is fresh. Disagreement becomes a recorded conflict
in the operator's queue, and no money moves.

## A compromised observer process

Its login role can insert observations under its own name, for its own
source, and move its own cursor. Row level security keyed on `session_user`
(`db/migrations/0006`, `0011`) and the grants in `db/roles/10_grants.sql`
refuse everything else: another source's identity, another source's cursor, a
canonical transfer, an allocation, an outbox event. The roles scenario
(`gateway-storage::roles`) connects as the observer and proves each refusal by
SQLSTATE.

## The wrong token, the wrong recipient, a failed transaction

Token identity is an allowlisted contract by canonical bytes, never a symbol;
recipients are matched by canonical address bytes, never display strings
(`gateway-tron::address`, `gateway-domain::AddressKey`). A transfer of an
unknown token is kept as evidence and never becomes a fact. A transaction
whose execution result is missing or not `SUCCESS` is refused, not assumed
(`gateway-tron::event`). The address codec is fuzzed: no mutation of a base58
address survives its checksum.

## Money as a float, money that overflows

Every amount is an integer at every boundary: `NUMERIC(78,0)` in PostgreSQL,
`U256` in Rust, decimal strings in JSON. Clippy denies float arithmetic in the
workspace. Parsers refuse signs, whitespace, decimals, zero and anything above
256 bits (`gateway-domain::money`, fuzzed).

## One payment claimed twice, one transfer used twice

A transfer belongs to at most one obligation: `chain_transfer_intent_claims`
is keyed by transfer. Settlement — claim, allocation, statuses, the fulfilment
row, the decision, the payment events and the outbox event — commits in one
transaction (`gateway-storage::settlement`), and the fulfilment row's primary
key is the lock that makes a second fulfilment impossible whatever a retry
did. Two candidates for one transfer stop the machine (`ambiguous`) instead
of guessing.

## An amount that matches nothing, or matches too much

Quotes reserve an exact amount at one collector, and no two open reservations
on a collector share an amount (`amount_leases`). A payment that matches no
reservation is recorded as unmatched and queued for a person, never absorbed.
Overpayment keeps the remainder visible; underpayment leaves the intent
`partially_paid`.

## A chain reorganisation after money moved

Transfer state advances only by compare-and-swap through `observed →
canonical → confirmed → finalized`, so a late `confirmed` cannot regress a
`finalized` transfer. `invalidated` is a terminal state the verifier can
reach, and the reconciler's `allocated_on_invalidated_transfer` check turns it
into a hard stop: the rail closes until a person decides what the product does.

## A stale price, a stale policy, a dead provider

A quote is issued only from server-owned snapshots that are present, fresh,
not future-dated and healthy; otherwise `503 quote_unavailable`
(`gateway-application::quotes`). The price feed itself needs independent
groups and a deviation ceiling (`gateway-application::operations`). Nothing
falls back to a default rate.

## A forged, replayed or misrouted webhook

The signature is HMAC-SHA256 over the timestamp and the raw body under a
per-endpoint secret derived from the deployment master key
(`gateway-domain::webhook`). The secret is never stored; the database keeps a
fingerprint, so a stolen backup cannot forge events, and a wrong master key
refuses to sign rather than sending signatures no merchant can verify. The
sender follows no redirects and speaks HTTPS only (`gateway-webhook`).

## A stuck worker that wakes up after its replacement

Every singleton role holds a lease with a fence token (`component_leases`).
A frozen holder that wakes after a takeover carries a stale token and its
writes are refused by the database, not by a hope that it noticed.

## A process attached to the wrong world

A process refuses to start, and stops answering ready, unless its database
describes the deployment it was configured for: collector and asset pins
recomputed from stored bytes and named in its allowlists, one chain
environment throughout, an active finality policy per asset, cursors not ahead
of their source's head, clocks that agree (`gateway-application::self_check`).
Mainnet is never a default.

## A book that stops adding up

Reconciliation assumes the rest of the system is wrong: it re-reads what was
observed, canonical, allocated and fulfilled and reports every disagreement.
A counter finding is drift for a person to explain; a money finding closes the
rail by itself, and only a person reopens it (`gateway-application::
reconciliation`, nine PostgreSQL scenarios).

## What is out of scope

The gateway holds no keys and signs no transactions, so a stolen host cannot
spend anything; it can lie about receipts, and the layers above are what
stops a lie from becoming a settlement. Payer privacy on a public chain, the
legal status of a payment, and sanctions screening are the operator's
responsibility; the gateway records a screening decision and treats an absent
one as `skipped`, never as clean.
