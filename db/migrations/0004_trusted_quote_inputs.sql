CREATE TABLE quote_policies (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    fiat_currency CHAR(3) NOT NULL CHECK (fiat_currency = upper(fiat_currency)),
    version TEXT NOT NULL CHECK (char_length(version) BETWEEN 1 AND 100),
    status TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'disabled')),
    quote_ttl_seconds BIGINT NOT NULL CHECK (quote_ttl_seconds > 0),
    late_payment_window_seconds BIGINT NOT NULL CHECK (late_payment_window_seconds > 0),
    amount_slot_count INTEGER NOT NULL CHECK (amount_slot_count BETWEEN 1 AND 10000),
    max_price_age_seconds BIGINT NOT NULL CHECK (max_price_age_seconds >= 0),
    max_policy_age_seconds BIGINT NOT NULL CHECK (max_policy_age_seconds >= 0),
    max_rail_health_age_seconds BIGINT NOT NULL CHECK (max_rail_health_age_seconds >= 0),
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (asset_id, fiat_currency, version)
);

CREATE UNIQUE INDEX quote_policies_one_active_idx
    ON quote_policies (asset_id, fiat_currency)
    WHERE status = 'active';

CREATE TABLE price_snapshots (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    fiat_currency CHAR(3) NOT NULL CHECK (fiat_currency = upper(fiat_currency)),
    rate_numerator NUMERIC(78, 0) NOT NULL CHECK (rate_numerator > 0),
    rate_denominator NUMERIC(78, 0) NOT NULL CHECK (rate_denominator > 0),
    sources JSONB NOT NULL CHECK (
        jsonb_typeof(sources) = 'array' AND jsonb_array_length(sources) >= 2
    ),
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX price_snapshots_lookup_idx
    ON price_snapshots (asset_id, fiat_currency, observed_at DESC, id DESC);

CREATE TABLE rail_health_snapshots (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    health TEXT NOT NULL CHECK (health IN ('healthy', 'degraded', 'unavailable')),
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX rail_health_snapshots_lookup_idx
    ON rail_health_snapshots (asset_id, observed_at DESC, id DESC);

ALTER TABLE payment_quotes
    ADD COLUMN price_snapshot_id UUID NOT NULL REFERENCES price_snapshots(id),
    ADD COLUMN quote_policy_id UUID NOT NULL REFERENCES quote_policies(id),
    ADD COLUMN rail_health_snapshot_id UUID NOT NULL REFERENCES rail_health_snapshots(id);

