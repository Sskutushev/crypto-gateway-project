-- Canonical transfers: the only chain facts the payment path may act on.
--
-- A canonical row is produced by the verifier from independent observations
-- and its own re-read. An observer can never write one.

CREATE TABLE chain_finality_policies (
    id UUID PRIMARY KEY,
    chain TEXT NOT NULL,
    network TEXT NOT NULL,
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    version TEXT NOT NULL CHECK (char_length(version) BETWEEN 1 AND 100),
    status TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'disabled')),
    -- Blocks that must sit on top of the transfer's block before the chain is
    -- considered settled for this network.
    min_confirmations BIGINT NOT NULL CHECK (min_confirmations >= 0),
    -- The weakest claim a source may make for the transfer to count as final.
    required_source_finality TEXT NOT NULL
        CHECK (required_source_finality IN ('seen', 'confirmed', 'finalized')),
    -- Independent provider groups required before a fact becomes canonical.
    min_independent_groups INTEGER NOT NULL CHECK (min_independent_groups BETWEEN 1 AND 10),
    max_evidence_age_seconds BIGINT NOT NULL CHECK (max_evidence_age_seconds > 0),
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (chain, network, chain_environment, version)
);

CREATE UNIQUE INDEX chain_finality_policies_one_active_idx
    ON chain_finality_policies (chain, network, chain_environment)
    WHERE status = 'active';

-- Immutable. The verifier only ever inserts.
CREATE TABLE chain_transfers (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    collector_address_id UUID NOT NULL REFERENCES collector_addresses(id),
    chain TEXT NOT NULL,
    network TEXT NOT NULL,
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    tx_hash TEXT NOT NULL,
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    block_number BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash TEXT NOT NULL,
    block_time TIMESTAMPTZ NOT NULL,
    token_key BYTEA NOT NULL,
    from_address_key BYTEA NOT NULL,
    from_address_text TEXT NOT NULL,
    to_address_key BYTEA NOT NULL,
    to_address_text TEXT NOT NULL,
    amount_raw NUMERIC(78, 0) NOT NULL CHECK (amount_raw > 0),
    decimals SMALLINT NOT NULL CHECK (decimals BETWEEN 0 AND 77),
    memo TEXT,
    canonicalization_policy TEXT NOT NULL,
    verifier_version TEXT NOT NULL,
    canonicalized_at TIMESTAMPTZ NOT NULL,
    -- One Ethereum transaction carries many transfer events, so identity is the
    -- event, not the transaction.
    UNIQUE (chain, network, chain_environment, tx_hash, event_index),
    UNIQUE (id, collector_address_id),
    UNIQUE (id, asset_id)
);

CREATE INDEX chain_transfers_match_idx
    ON chain_transfers (collector_address_id, amount_raw, block_time);

CREATE INDEX chain_transfers_block_time_idx ON chain_transfers (block_time);

-- Which readings, from which independent groups, supported the fact. The
-- provider group and kind are snapshots: a source may later be retired, but
-- the decision was made under the values of that moment.
CREATE TABLE chain_transfer_attestations (
    id UUID PRIMARY KEY,
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    observation_id UUID NOT NULL UNIQUE REFERENCES chain_observations(id),
    source_id UUID NOT NULL REFERENCES chain_sources(id),
    provider_group TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    attestation_role TEXT NOT NULL
        CHECK (attestation_role IN ('detection', 'reverify', 'finality', 'canonicality')),
    verifier_version TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (transfer_id, observation_id)
);

CREATE INDEX chain_transfer_attestations_transfer_idx
    ON chain_transfer_attestations (transfer_id);

CREATE TABLE chain_transfer_state_events (
    id UUID PRIMARY KEY,
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    previous_state TEXT,
    new_state TEXT NOT NULL
        CHECK (new_state IN ('observed', 'canonical', 'confirmed', 'finalized', 'invalidated')),
    state_version BIGINT NOT NULL CHECK (state_version > 0),
    source_key TEXT NOT NULL,
    block_hash TEXT,
    head_height BIGINT,
    evidence_sha256 TEXT,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (transfer_id, state_version)
);

CREATE INDEX chain_transfer_state_events_history_idx
    ON chain_transfer_state_events (transfer_id, created_at);

-- The current state is advanced by compare-and-swap, never by "the newest row
-- wins": a delayed `confirmed` arriving after `finalized` must not regress it.
CREATE TABLE chain_transfer_state_current (
    transfer_id UUID PRIMARY KEY REFERENCES chain_transfers(id),
    state TEXT NOT NULL
        CHECK (state IN ('observed', 'canonical', 'confirmed', 'finalized', 'invalidated')),
    state_version BIGINT NOT NULL CHECK (state_version > 0),
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX chain_transfer_state_current_state_idx
    ON chain_transfer_state_current (state, updated_at);

-- Mutable processing lives in its own table because privileges are granted per
-- table: the verifier may insert the row, only the payment path may change it.
CREATE TABLE chain_transfer_processing (
    transfer_id UUID PRIMARY KEY REFERENCES chain_transfers(id),
    allocated_raw NUMERIC(78, 0) NOT NULL DEFAULT 0 CHECK (allocated_raw >= 0),
    processing_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (processing_state IN ('pending', 'matched', 'settled', 'unmatched', 'held')),
    claim_holder TEXT,
    claim_until TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    version BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chain_transfer_processing_queue_idx
    ON chain_transfer_processing (processing_state, updated_at);

-- Sources that disagree about the same event. The fact is not created, the
-- disagreement is recorded, and the rail degrades.
CREATE TABLE chain_observation_conflicts (
    id UUID PRIMARY KEY,
    chain TEXT NOT NULL,
    network TEXT NOT NULL,
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    tx_hash TEXT NOT NULL,
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    field TEXT NOT NULL,
    resolution TEXT,
    resolved_by TEXT,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (chain, network, chain_environment, tx_hash, event_index, field),
    CHECK ((resolution IS NULL) = (resolved_at IS NULL))
);

CREATE INDEX chain_observation_conflicts_open_idx
    ON chain_observation_conflicts (resolution, created_at);

CREATE TABLE chain_observation_conflict_items (
    conflict_id UUID NOT NULL REFERENCES chain_observation_conflicts(id),
    observation_id UUID NOT NULL REFERENCES chain_observations(id),
    field_value JSONB NOT NULL,
    PRIMARY KEY (conflict_id, observation_id)
);

-- What the verifier decided about each chain event, so an event is not
-- re-decided forever and an operator can see why a payment did not settle.
CREATE TABLE chain_event_verdicts (
    chain TEXT NOT NULL,
    network TEXT NOT NULL,
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    tx_hash TEXT NOT NULL,
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    verdict TEXT NOT NULL
        CHECK (verdict IN ('verified', 'conflicted', 'insufficient', 'rejected')),
    reason TEXT,
    evidence_count INTEGER NOT NULL CHECK (evidence_count >= 0),
    independent_groups INTEGER NOT NULL CHECK (independent_groups >= 0),
    discarded_count INTEGER NOT NULL DEFAULT 0 CHECK (discarded_count >= 0),
    transfer_id UUID REFERENCES chain_transfers(id),
    verifier_version TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 1 CHECK (attempts > 0),
    decided_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (chain, network, chain_environment, tx_hash, event_index),
    -- A verified event always names its transfer. A later conflict about an
    -- already canonical event keeps the reference, so the row still says what
    -- the disagreement is about.
    CHECK (verdict <> 'verified' OR transfer_id IS NOT NULL)
);

CREATE INDEX chain_event_verdicts_open_idx ON chain_event_verdicts (verdict, decided_at);
