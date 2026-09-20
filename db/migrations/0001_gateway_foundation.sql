CREATE TABLE merchants (
    id UUID PRIMARY KEY,
    external_id TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 200),
    status TEXT NOT NULL CHECK (status IN ('active', 'suspended', 'closed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE merchant_api_keys (
    id UUID PRIMARY KEY,
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    key_prefix TEXT NOT NULL CHECK (char_length(key_prefix) BETWEEN 6 AND 32),
    secret_hash BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    UNIQUE (secret_hash)
);

CREATE INDEX merchant_api_keys_active_merchant_idx
    ON merchant_api_keys (merchant_id)
    WHERE revoked_at IS NULL;

CREATE TABLE payment_intents (
    id UUID PRIMARY KEY,
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    currency CHAR(3) NOT NULL CHECK (currency = upper(currency)),
    status TEXT NOT NULL CHECK (status IN (
        'requires_quote', 'awaiting_payment', 'partially_paid', 'risk_hold',
        'paid', 'expired', 'cancelled'
    )),
    reference TEXT NOT NULL CHECK (char_length(reference) BETWEEN 1 AND 128),
    description TEXT CHECK (char_length(description) <= 500),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata) = 'object'),
    version BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (merchant_id, reference)
);

CREATE INDEX payment_intents_merchant_created_idx
    ON payment_intents (merchant_id, created_at DESC);

CREATE TABLE api_idempotency_records (
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    route TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (char_length(idempotency_key) BETWEEN 16 AND 128),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    resource_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (merchant_id, route, idempotency_key)
);

CREATE TABLE audit_events (
    id UUID PRIMARY KEY,
    merchant_id UUID REFERENCES merchants(id),
    actor_type TEXT NOT NULL CHECK (actor_type IN ('api_key', 'operator', 'system')),
    actor_id UUID,
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id UUID,
    reason TEXT,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(payload) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_resource_idx
    ON audit_events (resource_type, resource_id, created_at DESC);

