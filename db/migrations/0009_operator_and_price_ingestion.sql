-- The operator's own identity, the price evidence a quote rests on, and the
-- switch that closes a rail.
--
-- Until now prices and rail health were rows somebody inserted by hand. This
-- gives them a write path with a principal, an append-only trail of what each
-- source said, and a recorded reason whenever a reading did not count.

-- An operator is not a merchant. Merchant keys buy; operator keys feed the
-- gateway evidence and close rails, and the two never share a credential.
CREATE TABLE operator_api_keys (
    id UUID PRIMARY KEY,
    key_prefix TEXT NOT NULL CHECK (char_length(key_prefix) BETWEEN 6 AND 32),
    secret_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(secret_hash) = 32),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    -- `ingest` feeds evidence, `read` sees the operator views, `admin` closes
    -- and reopens a rail. A key carries only what its holder needs.
    scopes TEXT[] NOT NULL CHECK (
        cardinality(scopes) > 0
        AND scopes <@ ARRAY['ingest', 'read', 'admin']::TEXT[]
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

-- How much independent agreement a price needs, and how far apart two honest
-- sources may be before their disagreement stops being noise.
ALTER TABLE quote_policies
    ADD COLUMN min_price_sources INTEGER NOT NULL DEFAULT 2
        CHECK (min_price_sources BETWEEN 2 AND 10),
    ADD COLUMN max_price_deviation_bps INTEGER NOT NULL DEFAULT 200
        CHECK (max_price_deviation_bps BETWEEN 1 AND 10000);

-- What each source actually said, kept whether or not it counted.
--
-- A snapshot is a decision about several readings. Keeping the readings is
-- what makes a disputed conversion answerable a year later, and what makes a
-- source that drifts visible before it is trusted again.
CREATE TABLE price_readings (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    fiat_currency CHAR(3) NOT NULL CHECK (fiat_currency = upper(fiat_currency)),
    source_key TEXT NOT NULL CHECK (char_length(source_key) BETWEEN 1 AND 100),
    -- Independence is counted by group, not by key: two keys of one vendor are
    -- one opinion.
    provider_group TEXT NOT NULL CHECK (char_length(provider_group) BETWEEN 1 AND 100),
    rate_numerator NUMERIC(78, 0) NOT NULL CHECK (rate_numerator > 0),
    rate_denominator NUMERIC(78, 0) NOT NULL CHECK (rate_denominator > 0),
    observed_at TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL,
    ingested_by UUID NOT NULL REFERENCES operator_api_keys(id),
    snapshot_id UUID REFERENCES price_snapshots(id),
    discard_reason TEXT,
    -- A reading either became part of a snapshot or says why it did not.
    CHECK ((snapshot_id IS NULL) = (discard_reason IS NOT NULL)),
    UNIQUE (asset_id, fiat_currency, source_key, observed_at)
);

CREATE INDEX price_readings_lookup_idx
    ON price_readings (asset_id, fiat_currency, observed_at DESC);

ALTER TABLE price_snapshots
    ADD COLUMN ingested_by UUID REFERENCES operator_api_keys(id),
    ADD COLUMN source_group_count INTEGER NOT NULL DEFAULT 0
        CHECK (source_group_count >= 0),
    ADD COLUMN deviation_bps INTEGER NOT NULL DEFAULT 0 CHECK (deviation_bps >= 0);

ALTER TABLE rail_health_snapshots
    ADD COLUMN ingested_by UUID REFERENCES operator_api_keys(id),
    ADD COLUMN detail TEXT;

-- A closed rail. Reconciliation opens one automatically when money does not
-- add up; only a person closes it again, because the thing that noticed the
-- discrepancy is not the thing that can say it was explained.
CREATE TABLE rail_stops (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    reason_code TEXT NOT NULL CHECK (char_length(reason_code) BETWEEN 1 AND 100),
    detail TEXT,
    opened_by TEXT NOT NULL,
    opened_at TIMESTAMPTZ NOT NULL,
    cleared_by TEXT,
    cleared_reason TEXT,
    cleared_at TIMESTAMPTZ,
    CHECK ((cleared_at IS NULL) = (cleared_by IS NULL))
);

-- One open stop per asset: reopening an already closed rail is not a second
-- incident.
CREATE UNIQUE INDEX rail_stops_one_open_idx
    ON rail_stops (asset_id)
    WHERE cleared_at IS NULL;

CREATE INDEX rail_stops_history_idx ON rail_stops (asset_id, opened_at DESC);

-- Who screened a transfer, so a pushed decision is attributable.
ALTER TABLE payment_risk_evaluations
    ADD COLUMN submitted_by UUID REFERENCES operator_api_keys(id);
