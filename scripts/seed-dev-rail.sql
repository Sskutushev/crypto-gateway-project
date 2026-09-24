\set ON_ERROR_STOP on

-- A development rail: the USDT test token the Nile faucet (nileex.io) hands
-- out (TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf), one collector address, and the
-- policies every quote and settlement decision rests on.
--
-- Usage:
--   psql "$GATEWAY_DATABASE_URL" -f scripts/seed-dev-rail.sql
--
-- The collector below is a placeholder account (0x41 followed by twenty 0x03
-- bytes, base58check TAF8dttxK5iPKbvYC626aDBytrWANpLRXp). Nobody holds its
-- key; money sent to it is lost. It exists so the start-up self-check has
-- something to pin. For a real testnet run, replace it with an address you
-- control, in both this file and GATEWAY_EXPECTED_COLLECTORS.
--
-- The price snapshot and rail-health snapshot are NOT seeded: they are
-- operator evidence and arrive through the operator API, which is how a quote
-- becomes possible. Until then every quote is refused, by design.

INSERT INTO chain_assets (
    id, chain, network, chain_environment, contract_address_key,
    display_symbol, decimals, status, pinned_sha256, approved_by
) VALUES (
    '00000000-0000-7000-8000-000000000101'::uuid, 'tron', 'nile', 'testnet',
    decode('41eca9bc828a3005b9a3b909f2cc5c2a54794de05f', 'hex'),
    'USDT', 6, 'active',
    encode(sha256(decode('41eca9bc828a3005b9a3b909f2cc5c2a54794de05f', 'hex')), 'hex'),
    'seed-dev-rail'
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO collector_addresses (
    id, asset_id, address_key, address_text, state, valid_from,
    pinned_sha256, approved_by
) VALUES (
    '00000000-0000-7000-8000-000000000201'::uuid,
    '00000000-0000-7000-8000-000000000101'::uuid,
    decode('410303030303030303030303030303030303030303', 'hex'),
    'TAF8dttxK5iPKbvYC626aDBytrWANpLRXp', 'active', now(),
    encode(sha256(decode('410303030303030303030303030303030303030303', 'hex')), 'hex'),
    'seed-dev-rail'
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO chain_finality_policies (
    id, chain, network, chain_environment, version, status, min_confirmations,
    required_source_finality, min_independent_groups, max_evidence_age_seconds, observed_at
) VALUES (
    '00000000-0000-7000-8000-000000000301'::uuid, 'tron', 'nile', 'testnet',
    'finality-v1', 'active', 19, 'finalized', 2, 3600, now()
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO quote_policies (
    id, asset_id, fiat_currency, version, status, quote_ttl_seconds,
    late_payment_window_seconds, amount_slot_count, max_price_age_seconds,
    max_policy_age_seconds, max_rail_health_age_seconds, observed_at
) VALUES (
    '00000000-0000-7000-8000-000000000302'::uuid,
    '00000000-0000-7000-8000-000000000101'::uuid,
    'USD', 'quote-v1', 'active', 900, 2592000, 100, 3600, 3600, 3600, now()
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO payment_settlement_policies (
    id, fiat_currency, version, status, approved_by, observed_at
) VALUES (
    '00000000-0000-7000-8000-000000000303'::uuid, 'USD', 'settlement-v1',
    'active', 'seed-dev-rail', now()
)
ON CONFLICT (id) DO NOTHING;

-- Small amounts settle on two independent groups; larger ones want an own
-- node and a risk decision, and above the last tier a person decides.
INSERT INTO payment_settlement_policy_tiers (
    policy_id, max_fiat_minor, min_independent_groups, require_own_node,
    require_risk_allow, auto_settle
) VALUES
    ('00000000-0000-7000-8000-000000000303'::uuid, 50000, 2, FALSE, FALSE, TRUE),
    ('00000000-0000-7000-8000-000000000303'::uuid, 600000, 2, TRUE, TRUE, FALSE)
ON CONFLICT (policy_id, max_fiat_minor) DO NOTHING;

-- Two observer sources in different provider groups plus the verifier's own
-- source. Development runs them all from one process under one principal,
-- which requires_dedicated_principal = FALSE records explicitly.
INSERT INTO chain_sources (
    id, chain, network, chain_environment, source_key, provider_group,
    kind, db_principal, requires_dedicated_principal, state, valid_from
) VALUES
    ('00000000-0000-7000-8000-000000000401'::uuid, 'tron', 'nile', 'testnet',
     'trongrid-nile', 'trongrid', 'indexed_api', 'gateway', FALSE, 'active', now()),
    ('00000000-0000-7000-8000-000000000402'::uuid, 'tron', 'nile', 'testnet',
     'nile-node', 'tron-nile-node', 'hosted_rpc', 'gateway', FALSE, 'active', now()),
    ('00000000-0000-7000-8000-000000000403'::uuid, 'tron', 'nile', 'testnet',
     'verifier-nile', 'verifier', 'hosted_rpc', 'gateway', FALSE, 'active', now())
ON CONFLICT (id) DO NOTHING;
