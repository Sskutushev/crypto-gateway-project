-- The money path: what a transfer may pay for, what was decided, what was
-- allocated, and what the merchant is told.

-- Chains that carry a comment let a payer name the obligation directly.
ALTER TABLE payment_attempts
    ADD COLUMN memo_reference TEXT
        CHECK (memo_reference IS NULL OR char_length(memo_reference) BETWEEN 1 AND 200);

CREATE UNIQUE INDEX payment_attempts_memo_reference_idx
    ON payment_attempts (collector_address_id, memo_reference)
    WHERE memo_reference IS NOT NULL AND status = 'awaiting_payment';

-- Settlement policy: how much independent evidence an amount of a given size
-- needs before money may move without a person.
CREATE TABLE payment_settlement_policies (
    id UUID PRIMARY KEY,
    fiat_currency CHAR(3) NOT NULL CHECK (fiat_currency = upper(fiat_currency)),
    version TEXT NOT NULL CHECK (char_length(version) BETWEEN 1 AND 100),
    status TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'disabled')),
    approved_by TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (fiat_currency, version)
);

CREATE UNIQUE INDEX payment_settlement_policies_one_active_idx
    ON payment_settlement_policies (fiat_currency)
    WHERE status = 'active';

CREATE TABLE payment_settlement_policy_tiers (
    policy_id UUID NOT NULL REFERENCES payment_settlement_policies(id),
    max_fiat_minor BIGINT NOT NULL CHECK (max_fiat_minor > 0),
    min_independent_groups INTEGER NOT NULL CHECK (min_independent_groups BETWEEN 1 AND 10),
    require_own_node BOOLEAN NOT NULL,
    require_risk_allow BOOLEAN NOT NULL,
    auto_settle BOOLEAN NOT NULL,
    PRIMARY KEY (policy_id, max_fiat_minor)
);

-- What a screening provider said about the source of funds. An absent
-- screening is recorded as `skipped`, which is never the same as `allow`.
CREATE TABLE payment_risk_evaluations (
    id UUID PRIMARY KEY,
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    provider TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('allow', 'review', 'deny', 'skipped')),
    score INTEGER,
    reasons JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(reasons) = 'object'),
    evaluated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (transfer_id, provider, evaluated_at)
);

CREATE INDEX payment_risk_evaluations_transfer_idx
    ON payment_risk_evaluations (transfer_id, evaluated_at DESC);

-- One transfer belongs to at most one payment intent. Several transfers may
-- close one intent; one transfer may never close two.
CREATE TABLE chain_transfer_intent_claims (
    transfer_id UUID PRIMARY KEY REFERENCES chain_transfers(id),
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    attempt_id UUID NOT NULL REFERENCES payment_attempts(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    match_strategy TEXT NOT NULL
        CHECK (match_strategy IN ('memo', 'exact_amount', 'historical_slot', 'manual')),
    claimed_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX chain_transfer_intent_claims_intent_idx
    ON chain_transfer_intent_claims (payment_intent_id);

CREATE TABLE payment_allocations (
    id UUID PRIMARY KEY,
    attempt_id UUID NOT NULL REFERENCES payment_attempts(id),
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    allocated_raw NUMERIC(78, 0) NOT NULL CHECK (allocated_raw > 0),
    allocated_by TEXT NOT NULL,
    reason TEXT NOT NULL
        CHECK (reason IN ('exact', 'partial', 'overpay_remainder', 'late_honored', 'manual')),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (attempt_id, transfer_id)
);

CREATE INDEX payment_allocations_intent_idx ON payment_allocations (payment_intent_id);

-- Why a payment became paid, in one row, a year later: which policy applied,
-- how many independent groups confirmed it, whether an own node was among
-- them, what the risk screening said, and who decided.
CREATE TABLE payment_settlement_decisions (
    id UUID PRIMARY KEY,
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    attempt_id UUID NOT NULL REFERENCES payment_attempts(id),
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    fiat_amount_minor BIGINT NOT NULL CHECK (fiat_amount_minor > 0),
    required_policy TEXT NOT NULL,
    distinct_groups INTEGER NOT NULL CHECK (distinct_groups >= 0),
    had_own_node BOOLEAN NOT NULL,
    finality_state TEXT NOT NULL,
    risk_decision TEXT NOT NULL CHECK (risk_decision IN ('allow', 'review', 'deny', 'skipped')),
    risk_evaluation_id UUID REFERENCES payment_risk_evaluations(id),
    attestation_ids UUID[] NOT NULL,
    match_strategy TEXT NOT NULL,
    allocated_raw NUMERIC(78, 0) NOT NULL DEFAULT 0 CHECK (allocated_raw >= 0),
    remainder_raw NUMERIC(78, 0) NOT NULL DEFAULT 0 CHECK (remainder_raw >= 0),
    outcome TEXT NOT NULL
        CHECK (outcome IN ('settled', 'partial', 'overpaid', 'held', 'manual_required',
                           'rejected', 'ambiguous', 'unmatched')),
    decided_by TEXT NOT NULL,
    decided_reason TEXT,
    decided_at TIMESTAMPTZ NOT NULL,
    UNIQUE (payment_intent_id, transfer_id)
);

CREATE INDEX payment_settlement_decisions_outcome_idx
    ON payment_settlement_decisions (outcome, decided_at DESC);

-- The claim to fulfil. One row per payment intent is the lock that makes a
-- second fulfilment impossible, whatever the retry did.
CREATE TABLE payment_fulfillments (
    payment_intent_id UUID PRIMARY KEY REFERENCES payment_intents(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    status TEXT NOT NULL CHECK (status IN ('claimed', 'succeeded', 'failed', 'manual_review')),
    claimed_at TIMESTAMPTZ NOT NULL,
    fulfilled_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    CHECK ((status = 'succeeded') = (fulfilled_at IS NOT NULL))
);

-- The visible history of one payment: every transition, with its cause.
CREATE TABLE payment_events (
    id UUID PRIMARY KEY,
    merchant_id UUID REFERENCES merchants(id),
    payment_intent_id UUID REFERENCES payment_intents(id),
    attempt_id UUID REFERENCES payment_attempts(id),
    transfer_id UUID REFERENCES chain_transfers(id),
    event_type TEXT NOT NULL,
    previous_status TEXT,
    new_status TEXT,
    reason_code TEXT,
    source TEXT NOT NULL CHECK (source IN ('system', 'observer', 'verifier', 'admin')),
    actor TEXT,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(payload) = 'object'),
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX payment_events_intent_idx ON payment_events (payment_intent_id, created_at);
CREATE INDEX payment_events_type_idx ON payment_events (event_type, created_at DESC);

-- The transactional outbox. Anything that leaves this system is written here
-- inside the same transaction as the money, and delivered afterwards.
CREATE TABLE domain_events (
    id UUID PRIMARY KEY,
    merchant_id UUID REFERENCES merchants(id),
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID NOT NULL,
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    available_at TIMESTAMPTZ NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    claimed_by TEXT,
    claimed_until TIMESTAMPTZ,
    delivered_at TIMESTAMPTZ,
    dead_lettered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    -- A delivered event is never also dead-lettered.
    CHECK (delivered_at IS NULL OR dead_lettered_at IS NULL)
);

CREATE INDEX domain_events_queue_idx
    ON domain_events (available_at)
    WHERE delivered_at IS NULL AND dead_lettered_at IS NULL;

CREATE INDEX domain_events_aggregate_idx ON domain_events (aggregate_type, aggregate_id);

-- Where a merchant wants to be told, and with which secret the signature is
-- computed. The secret is stored hashed; the plaintext exists only at the
-- moment it is issued.
CREATE TABLE webhook_endpoints (
    id UUID PRIMARY KEY,
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    url TEXT NOT NULL CHECK (url LIKE 'https://%'),
    secret_hash BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    description TEXT,
    status TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL,
    disabled_at TIMESTAMPTZ,
    UNIQUE (merchant_id, url),
    CHECK ((status = 'disabled') = (disabled_at IS NOT NULL))
);

CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY,
    event_id UUID NOT NULL REFERENCES domain_events(id),
    endpoint_id UUID NOT NULL REFERENCES webhook_endpoints(id),
    attempt INTEGER NOT NULL CHECK (attempt > 0),
    response_status INTEGER,
    error TEXT,
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    delivered_at TIMESTAMPTZ NOT NULL,
    UNIQUE (event_id, endpoint_id, attempt)
);

CREATE INDEX webhook_deliveries_event_idx ON webhook_deliveries (event_id);
