\set ON_ERROR_STOP on

-- Usage:
-- psql "$GATEWAY_DATABASE_URL" \
--   -v merchant_id="00000000-0000-7000-8000-000000000001" \
--   -v key_id="00000000-0000-7000-8000-000000000002" \
--   -v api_key_prefix="cg_dev" \
--   -v api_key_sha256_hex="<64 lowercase hex characters>" \
--   -f scripts/create-dev-merchant.sql
--
-- Hash the full random development key outside PostgreSQL. Never use this
-- script or a deterministic key in production.

INSERT INTO merchants (id, external_id, display_name, status)
VALUES (:'merchant_id'::uuid, 'local-development', 'Local development', 'active')
ON CONFLICT (external_id) DO NOTHING;

INSERT INTO merchant_api_keys (
    id, merchant_id, key_prefix, secret_hash, label
)
VALUES (
    :'key_id'::uuid,
    :'merchant_id'::uuid,
    :'api_key_prefix',
    decode(:'api_key_sha256_hex', 'hex'),
    'local development'
)
ON CONFLICT (secret_hash) DO NOTHING;

