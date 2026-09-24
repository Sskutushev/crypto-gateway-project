-- Chain evidence intake: sources, append-only observations, durable cursors,
-- and component leases with fencing.
--
-- An observation records what one source claimed. It is never a fact the
-- settlement path may trust on its own.

ALTER TABLE chain_assets
    ADD COLUMN pinned_sha256 TEXT,
    ADD COLUMN approved_by TEXT;

UPDATE chain_assets
   SET pinned_sha256 = encode(sha256(contract_address_key), 'hex'),
       approved_by = COALESCE(approved_by, 'migration:0006');

ALTER TABLE chain_assets
    ALTER COLUMN pinned_sha256 SET NOT NULL,
    ALTER COLUMN approved_by SET NOT NULL,
    ADD CONSTRAINT chain_assets_pinned_sha256_format
        CHECK (char_length(pinned_sha256) = 64 AND pinned_sha256 ~ '^[0-9a-f]+$');

ALTER TABLE collector_addresses
    ADD COLUMN pinned_sha256 TEXT,
    ADD COLUMN approved_by TEXT;

UPDATE collector_addresses
   SET pinned_sha256 = encode(sha256(address_key), 'hex'),
       approved_by = COALESCE(approved_by, 'migration:0006');

ALTER TABLE collector_addresses
    ALTER COLUMN pinned_sha256 SET NOT NULL,
    ALTER COLUMN approved_by SET NOT NULL,
    ADD CONSTRAINT collector_addresses_pinned_sha256_format
        CHECK (char_length(pinned_sha256) = 64 AND pinned_sha256 ~ '^[0-9a-f]+$');

-- A source is immutable except for its state. Repointing an existing row at a
-- different upstream would retroactively change which independent sources
-- confirmed historical payments.
CREATE TABLE chain_sources (
    id UUID PRIMARY KEY,
    chain TEXT NOT NULL CHECK (char_length(chain) BETWEEN 1 AND 32),
    network TEXT NOT NULL CHECK (char_length(network) BETWEEN 1 AND 64),
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    source_key TEXT NOT NULL CHECK (char_length(source_key) BETWEEN 1 AND 100),
    provider_group TEXT NOT NULL CHECK (char_length(provider_group) BETWEEN 1 AND 100),
    kind TEXT NOT NULL CHECK (kind IN ('own_node', 'hosted_rpc', 'indexed_api')),
    db_principal TEXT NOT NULL CHECK (char_length(db_principal) BETWEEN 1 AND 100),
    -- A source that carries weight in a settlement decision must own its
    -- database role; sharing one would make the recorded principal
    -- meaningless. A source may opt out explicitly, which is how one local
    -- process may poll several public APIs during development.
    requires_dedicated_principal BOOLEAN NOT NULL DEFAULT TRUE,
    state TEXT NOT NULL CHECK (state IN ('active', 'degraded', 'disabled')),
    valid_from TIMESTAMPTZ NOT NULL,
    retired_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (chain, network, chain_environment, source_key),
    CHECK ((state = 'disabled') OR retired_at IS NULL)
);

CREATE UNIQUE INDEX chain_sources_dedicated_principal_idx
    ON chain_sources (db_principal)
    WHERE requires_dedicated_principal;

-- Append-only. No UPDATE, no DELETE: a corrected reading is a new row.
CREATE TABLE chain_observations (
    id UUID PRIMARY KEY,
    source_id UUID NOT NULL REFERENCES chain_sources(id),
    -- Filled by the database, never by the writer, so a compromised observer
    -- cannot claim another source's identity.
    source_principal TEXT NOT NULL DEFAULT session_user,
    asset_id UUID REFERENCES chain_assets(id),
    collector_address_id UUID REFERENCES collector_addresses(id),
    chain TEXT NOT NULL,
    network TEXT NOT NULL,
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    observation_kind TEXT NOT NULL
        CHECK (observation_kind IN ('fast_detect', 'cursor_scan', 'targeted_lookup')),
    tx_hash TEXT NOT NULL CHECK (char_length(tx_hash) BETWEEN 1 AND 200),
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    block_number BIGINT CHECK (block_number IS NULL OR block_number >= 0),
    block_hash TEXT,
    parent_hash TEXT,
    -- The block's own time, never the observer's clock.
    block_time TIMESTAMPTZ,
    token_key BYTEA NOT NULL CHECK (octet_length(token_key) > 0),
    token_display TEXT NOT NULL,
    from_address_key BYTEA NOT NULL CHECK (octet_length(from_address_key) > 0),
    from_address_text TEXT NOT NULL,
    to_address_key BYTEA NOT NULL CHECK (octet_length(to_address_key) > 0),
    to_address_text TEXT NOT NULL,
    amount_raw NUMERIC(78, 0) NOT NULL CHECK (amount_raw > 0),
    decimals SMALLINT NOT NULL CHECK (decimals BETWEEN 0 AND 77),
    memo TEXT CHECK (memo IS NULL OR char_length(memo) <= 200),
    execution_status TEXT NOT NULL CHECK (execution_status IN ('success', 'failed')),
    source_finality TEXT NOT NULL CHECK (source_finality IN ('seen', 'confirmed', 'finalized')),
    source_head BIGINT CHECK (source_head IS NULL OR source_head >= 0),
    evidence_sha256 TEXT NOT NULL
        CHECK (char_length(evidence_sha256) = 64 AND evidence_sha256 ~ '^[0-9a-f]+$'),
    evidence_uri TEXT,
    observer_version TEXT NOT NULL,
    parser_version TEXT NOT NULL,
    fence_token BIGINT NOT NULL CHECK (fence_token > 0),
    semantic_hash BYTEA NOT NULL CHECK (octet_length(semantic_hash) = 32),
    observed_at TIMESTAMPTZ NOT NULL,
    UNIQUE (source_id, semantic_hash)
);

CREATE INDEX chain_observations_tx_idx
    ON chain_observations (chain, network, tx_hash, event_index);

CREATE INDEX chain_observations_match_idx
    ON chain_observations (chain, network, to_address_key, amount_raw);

CREATE INDEX chain_observations_intake_idx
    ON chain_observations (observed_at DESC);

ALTER TABLE chain_observations ENABLE ROW LEVEL SECURITY;
ALTER TABLE chain_observations FORCE ROW LEVEL SECURITY;

-- The writer may only write under its own authenticated principal. Superusers
-- bypass row level security entirely, so no production role may be one.
CREATE POLICY chain_observations_insert_identity
    ON chain_observations
    FOR INSERT
    TO PUBLIC
    WITH CHECK (source_principal = session_user);

CREATE POLICY chain_observations_read
    ON chain_observations
    FOR SELECT
    TO PUBLIC
    USING (true);

-- One cursor row per source, lane and collector: one observer can never move
-- another observer's recovery point.
CREATE TABLE chain_cursors (
    source_id UUID NOT NULL REFERENCES chain_sources(id),
    observation_kind TEXT NOT NULL
        CHECK (observation_kind IN ('fast_detect', 'cursor_scan', 'targeted_lookup')),
    collector_address_id UUID NOT NULL REFERENCES collector_addresses(id),
    cursor_kind TEXT NOT NULL CHECK (cursor_kind IN ('block', 'logical_time', 'event_position')),
    cursor_value TEXT NOT NULL CHECK (char_length(cursor_value) BETWEEN 1 AND 200),
    last_block_hash TEXT,
    fence_token BIGINT NOT NULL CHECK (fence_token > 0),
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (source_id, observation_kind, collector_address_id)
);

-- Leadership plus fencing. A frozen holder that wakes up after a takeover
-- carries a stale token and its writes are refused.
CREATE TABLE component_leases (
    component TEXT PRIMARY KEY CHECK (char_length(component) BETWEEN 1 AND 120),
    holder TEXT NOT NULL CHECK (char_length(holder) BETWEEN 1 AND 200),
    fence_token BIGINT NOT NULL CHECK (fence_token > 0),
    lease_until TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
