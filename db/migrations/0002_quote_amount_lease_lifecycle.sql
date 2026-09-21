CREATE TABLE chain_assets (
    id UUID PRIMARY KEY,
    chain TEXT NOT NULL CHECK (char_length(chain) BETWEEN 1 AND 32),
    network TEXT NOT NULL CHECK (char_length(network) BETWEEN 1 AND 64),
    chain_environment TEXT NOT NULL CHECK (chain_environment IN ('testnet', 'mainnet')),
    contract_address_key BYTEA NOT NULL CHECK (octet_length(contract_address_key) > 0),
    display_symbol TEXT NOT NULL CHECK (char_length(display_symbol) BETWEEN 1 AND 20),
    decimals SMALLINT NOT NULL CHECK (decimals BETWEEN 0 AND 77),
    status TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (chain, network, chain_environment, contract_address_key)
);

CREATE TABLE collector_addresses (
    id UUID PRIMARY KEY,
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    address_key BYTEA NOT NULL CHECK (octet_length(address_key) > 0),
    address_text TEXT NOT NULL CHECK (char_length(address_text) BETWEEN 1 AND 200),
    state TEXT NOT NULL CHECK (state IN ('active', 'receiving_only', 'retired')),
    valid_from TIMESTAMPTZ NOT NULL,
    retired_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (asset_id, address_key),
    CHECK ((state = 'retired') = (retired_at IS NOT NULL))
);

CREATE TABLE payment_quotes (
    id UUID PRIMARY KEY,
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    asset_id UUID NOT NULL REFERENCES chain_assets(id),
    collector_address_id UUID NOT NULL REFERENCES collector_addresses(id),
    fiat_currency CHAR(3) NOT NULL CHECK (fiat_currency = upper(fiat_currency)),
    fiat_amount_minor BIGINT NOT NULL CHECK (fiat_amount_minor > 0),
    base_amount_raw NUMERIC(78, 0) NOT NULL CHECK (base_amount_raw > 0),
    amount_raw NUMERIC(78, 0) NOT NULL CHECK (amount_raw > 0),
    rate_numerator NUMERIC(78, 0) NOT NULL CHECK (rate_numerator > 0),
    rate_denominator NUMERIC(78, 0) NOT NULL CHECK (rate_denominator > 0),
    price_sources JSONB NOT NULL CHECK (
        jsonb_typeof(price_sources) = 'array' AND jsonb_array_length(price_sources) > 0
    ),
    price_observed_at TIMESTAMPTZ NOT NULL,
    policy_version TEXT NOT NULL CHECK (char_length(policy_version) BETWEEN 1 AND 100),
    rail_health_observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    late_payment_until TIMESTAMPTZ NOT NULL,
    UNIQUE (payment_intent_id),
    CHECK (expires_at > created_at),
    CHECK (late_payment_until > expires_at)
);

CREATE INDEX payment_quotes_merchant_created_idx
    ON payment_quotes (merchant_id, created_at DESC);

CREATE TABLE payment_attempts (
    id UUID PRIMARY KEY,
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    quote_id UUID NOT NULL UNIQUE REFERENCES payment_quotes(id),
    collector_address_id UUID NOT NULL REFERENCES collector_addresses(id),
    expected_amount_raw NUMERIC(78, 0) NOT NULL CHECK (expected_amount_raw > 0),
    status TEXT NOT NULL CHECK (status IN ('awaiting_payment', 'expired', 'cancelled', 'settled')),
    quote_expires_at TIMESTAMPTZ NOT NULL,
    late_payment_until TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK (late_payment_until > quote_expires_at)
);

CREATE INDEX payment_attempts_status_expiry_idx
    ON payment_attempts (status, quote_expires_at);

CREATE TABLE amount_leases (
    id UUID PRIMARY KEY,
    collector_address_id UUID NOT NULL REFERENCES collector_addresses(id),
    amount_raw NUMERIC(78, 0) NOT NULL CHECK (amount_raw > 0),
    attempt_id UUID NOT NULL UNIQUE REFERENCES payment_attempts(id),
    leased_from TIMESTAMPTZ NOT NULL,
    lease_until TIMESTAMPTZ NOT NULL,
    UNIQUE (collector_address_id, amount_raw),
    CHECK (lease_until > leased_from)
);

CREATE INDEX amount_leases_expiry_idx ON amount_leases (lease_until);

CREATE TABLE amount_lease_history (
    id UUID PRIMARY KEY,
    lease_id UUID NOT NULL UNIQUE,
    collector_address_id UUID NOT NULL REFERENCES collector_addresses(id),
    amount_raw NUMERIC(78, 0) NOT NULL CHECK (amount_raw > 0),
    attempt_id UUID NOT NULL UNIQUE REFERENCES payment_attempts(id),
    leased_from TIMESTAMPTZ NOT NULL,
    leased_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ NOT NULL,
    release_reason TEXT NOT NULL CHECK (release_reason IN ('expired', 'settled', 'cancelled')),
    CHECK (released_at >= leased_from)
);

CREATE INDEX amount_lease_history_match_idx
    ON amount_lease_history (collector_address_id, amount_raw, leased_from, leased_until);

