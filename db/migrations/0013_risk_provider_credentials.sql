-- A pricing or rail-health feeder must never be able to impersonate a KYT
-- provider. Risk credentials are separately scoped and bound to the provider
-- name they are allowed to submit.

ALTER TABLE operator_api_keys
    DROP CONSTRAINT operator_api_keys_scopes_check,
    ADD CONSTRAINT operator_api_keys_scopes_check CHECK (
        cardinality(scopes) > 0
        AND scopes <@ ARRAY['ingest', 'risk_ingest', 'read', 'admin']::TEXT[]
    );

CREATE TABLE operator_risk_provider_bindings (
    operator_key_id UUID NOT NULL REFERENCES operator_api_keys(id),
    provider TEXT NOT NULL CHECK (char_length(btrim(provider)) BETWEEN 1 AND 100),
    enabled_at TIMESTAMPTZ NOT NULL,
    disabled_at TIMESTAMPTZ,
    PRIMARY KEY (operator_key_id, provider),
    CHECK (disabled_at IS NULL OR disabled_at >= enabled_at)
);

CREATE INDEX operator_risk_provider_bindings_active_idx
    ON operator_risk_provider_bindings (provider, operator_key_id)
    WHERE disabled_at IS NULL;
