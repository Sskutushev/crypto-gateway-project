\set ON_ERROR_STOP on

-- A rail for the chain simulator (tools/chain-simulator). Testing only.
--
-- Usage, on a database of its own (never the one a real rail lives in):
--   psql "$GATEWAY_DATABASE_URL" -f scripts/seed-simulator-rail.sql
--
-- Network 'simulator' in the 'testnet' environment. Nothing here names a real
-- token: the asset is the simulator's own contract (base58check
-- TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN, derived from the default seed
-- "chain-simulator"), and the collector is an address derived the same way
-- (TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe) that nobody holds a key for. The
-- simulator refuses to mint the real USDT contracts, so a deployment that
-- points at it cannot be told that real USDT arrived.
--
-- The processes that read this rail pin it:
--   GATEWAY_NETWORK=simulator
--   GATEWAY_CHAIN_ENVIRONMENT=testnet
--   GATEWAY_EXPECTED_COLLECTORS=TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe
--   GATEWAY_EXPECTED_ASSETS=tron:simulator:TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN
--
-- As with seed-dev-rail.sql, price and rail-health evidence are not seeded:
-- they arrive through the operator API.

INSERT INTO chain_assets (
    id, chain, network, chain_environment, contract_address_key,
    display_symbol, decimals, status, pinned_sha256, approved_by
) VALUES (
    '00000000-0000-7000-8000-000000000111'::uuid, 'tron', 'simulator', 'testnet',
    decode('41acf95468a56e374053a5cf509f8e33ae342fd8ea', 'hex'),
    'USDT-SIM', 6, 'active',
    encode(sha256(decode('41acf95468a56e374053a5cf509f8e33ae342fd8ea', 'hex')), 'hex'),
    'seed-simulator-rail'
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO collector_addresses (
    id, asset_id, address_key, address_text, state, valid_from,
    pinned_sha256, approved_by
) VALUES (
    '00000000-0000-7000-8000-000000000211'::uuid,
    '00000000-0000-7000-8000-000000000111'::uuid,
    decode('412ff148ea78250de8062335c29c3c50a5140cbdfa', 'hex'),
    'TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe', 'active', now(),
    encode(sha256(decode('412ff148ea78250de8062335c29c3c50a5140cbdfa', 'hex')), 'hex'),
    'seed-simulator-rail'
)
ON CONFLICT (id) DO NOTHING;

-- Finality is the source's solidified head: a transfer counts only once a
-- source reports its block as solidified, which the simulator does 19 blocks
-- after the head, as TRON does. min_confirmations is 0 because a reading's
-- confirmations are counted from that solidified head, and every lane reads a
-- block about when it solidifies and does not read it again: a larger value
-- is never reached by fresh evidence, and the payment would wait at
-- 'confirmed' indefinitely. See tools/chain-simulator/README.md.
INSERT INTO chain_finality_policies (
    id, chain, network, chain_environment, version, status, min_confirmations,
    required_source_finality, min_independent_groups, max_evidence_age_seconds, observed_at
) VALUES (
    '00000000-0000-7000-8000-000000000311'::uuid, 'tron', 'simulator', 'testnet',
    'finality-v1', 'active', 0, 'finalized', 2, 3600, now()
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO quote_policies (
    id, asset_id, fiat_currency, version, status, quote_ttl_seconds,
    late_payment_window_seconds, amount_slot_count, max_price_age_seconds,
    max_policy_age_seconds, max_rail_health_age_seconds, observed_at
) VALUES (
    '00000000-0000-7000-8000-000000000312'::uuid,
    '00000000-0000-7000-8000-000000000111'::uuid,
    'USD', 'quote-v1', 'active', 900, 2592000, 100, 3600, 2592000, 3600, now()
)
ON CONFLICT (id) DO NOTHING;

-- The settlement policy is per fiat currency, not per rail, so it shares its
-- id with seed-dev-rail.sql: either seed may run first.
INSERT INTO payment_settlement_policies (
    id, fiat_currency, version, status, approved_by, observed_at
) VALUES (
    '00000000-0000-7000-8000-000000000303'::uuid, 'USD', 'settlement-v1',
    'active', 'seed-simulator-rail', now()
)
ON CONFLICT (id) DO NOTHING;

INSERT INTO payment_settlement_policy_tiers (
    policy_id, max_fiat_minor, min_independent_groups, require_own_node,
    require_risk_allow, auto_settle
) VALUES
    ('00000000-0000-7000-8000-000000000303'::uuid, 50000, 2, FALSE, FALSE, TRUE),
    ('00000000-0000-7000-8000-000000000303'::uuid, 600000, 2, TRUE, TRUE, FALSE)
ON CONFLICT (policy_id, max_fiat_minor) DO NOTHING;

-- Two observer sources in different provider groups and the verifier's own
-- source. All three read the same simulator, so their independence is
-- simulated: the provider groups exercise the gateway's counting, not any
-- real disagreement between providers.
INSERT INTO chain_sources (
    id, chain, network, chain_environment, source_key, provider_group,
    kind, db_principal, requires_dedicated_principal, state, valid_from
) VALUES
    ('00000000-0000-7000-8000-000000000411'::uuid, 'tron', 'simulator', 'testnet',
     'sim-index', 'sim-provider-a', 'indexed_api', 'gateway', FALSE, 'active', now()),
    ('00000000-0000-7000-8000-000000000412'::uuid, 'tron', 'simulator', 'testnet',
     'sim-node', 'sim-provider-b', 'hosted_rpc', 'gateway', FALSE, 'active', now()),
    ('00000000-0000-7000-8000-000000000413'::uuid, 'tron', 'simulator', 'testnet',
     'sim-verifier', 'sim-verifier', 'hosted_rpc', 'gateway', FALSE, 'active', now())
ON CONFLICT (id) DO NOTHING;
