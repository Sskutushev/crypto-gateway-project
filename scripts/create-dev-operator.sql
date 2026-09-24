\set ON_ERROR_STOP on

-- Usage:
-- psql "$GATEWAY_DATABASE_URL" \
--   -v key_id="00000000-0000-7000-8000-000000000003" \
--   -v api_key_prefix="cgop_dev" \
--   -v api_key_sha256_hex="<64 lowercase hex characters>" \
--   -f scripts/create-dev-operator.sql
--
-- Hash the full random development key outside PostgreSQL:
--   printf '%s' "$OPERATOR_KEY" | sha256sum
-- The key itself must be between 32 and 256 characters. Never use this script
-- or a deterministic key in production. This development key can feed prices,
-- read the overview and close a rail. It intentionally cannot submit KYT
-- decisions: create a separate `risk_ingest` key and bind it in
-- operator_risk_provider_bindings, even in development.

INSERT INTO operator_api_keys (id, key_prefix, secret_hash, label, scopes)
VALUES (
    :'key_id'::uuid,
    :'api_key_prefix',
    decode(:'api_key_sha256_hex', 'hex'),
    'local development',
    ARRAY['ingest', 'read', 'admin']::TEXT[]
)
ON CONFLICT (secret_hash) DO NOTHING;
