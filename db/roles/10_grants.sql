-- Table privileges, least privilege, derived from what each crate reads and
-- writes. Applied after every migration, safe to apply again.
--
-- The lists below follow the SQL in crates/gateway-storage/src:
--   observations.rs   observer intake, cursors, component leases
--   verification.rs   the verifier: canonical transfers, verdicts, conflicts
--   settlement.rs     matching and settlement, the outbox write
--   outbox.rs         webhook delivery
--   postgres.rs       merchant API, quotes, leases, idempotency, audit, expiry
--   operations.rs     operator writes: prices, rail health, rail stops, risk
--   operator_reads.rs operator reads
--   oversight.rs      component health and reconciliation
--   self_check.rs     start-up invariants, read by every process
--
-- A privilege that is not on this list is a privilege a process does not have,
-- and a new table gets nothing until it is added here. That is the point: an
-- observer that is compromised can lie about what it saw, and nothing else.

-- Nothing is granted by being a database user.
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM PUBLIC;

-- The migrator owns every table. Migrations applied earlier by another owner
-- (a development superuser, a first deployment) are moved under it so the
-- ownership, and therefore FORCE ROW LEVEL SECURITY, means the same thing in
-- every environment.
GRANT ALL ON SCHEMA public TO gateway_migrator;
DO $$
DECLARE
    table_name TEXT;
BEGIN
    FOR table_name IN
        SELECT tablename FROM pg_tables WHERE schemaname = 'public'
    LOOP
        EXECUTE format('ALTER TABLE public.%I OWNER TO gateway_migrator', table_name);
    END LOOP;
END
$$;

GRANT USAGE ON SCHEMA public TO
    gateway_api, gateway_observer, gateway_verifier, gateway_payment,
    gateway_reconciler, gateway_readonly;

-- Every process proves its rails at start-up (self_check.rs) and every leased
-- worker holds its lease in component_leases.
GRANT SELECT ON
    chain_assets, collector_addresses, chain_sources, chain_finality_policies,
    chain_cursors, chain_observations
TO gateway_api, gateway_observer, gateway_verifier, gateway_payment, gateway_reconciler;

GRANT SELECT, INSERT, UPDATE ON component_leases
TO gateway_observer, gateway_verifier, gateway_payment, gateway_reconciler;

-- Observer: it may say what it saw and where it stopped reading. Nothing else.
-- Which rows of chain_observations and chain_cursors it may write is decided
-- by row level security against session_user.
GRANT INSERT ON chain_observations TO gateway_observer;
GRANT INSERT, UPDATE ON chain_cursors TO gateway_observer;

-- Verifier: turns independent readings plus its own re-read into canonical
-- facts. Its re-read is itself an observation under its own source.
GRANT INSERT ON chain_observations TO gateway_verifier;
GRANT SELECT ON chain_event_verdicts, chain_transfers, chain_transfer_state_current,
    chain_transfer_attestations
TO gateway_verifier;
GRANT INSERT ON
    chain_transfers, chain_transfer_attestations, chain_transfer_state_events,
    chain_transfer_state_current, chain_transfer_processing,
    chain_observation_conflicts, chain_observation_conflict_items, chain_event_verdicts
TO gateway_verifier;
GRANT UPDATE ON chain_transfer_state_current, chain_event_verdicts TO gateway_verifier;

-- Payment: the expiry sweep, matching and settlement, and outbox delivery.
-- It reads canonical facts and never writes one.
GRANT SELECT ON
    merchants, payment_intents, payment_attempts, payment_quotes,
    amount_leases, amount_lease_history,
    chain_transfers, chain_transfer_state_current, chain_transfer_attestations,
    chain_transfer_processing, chain_transfer_intent_claims,
    payment_allocations, payment_fulfillments, payment_settlement_decisions,
    payment_risk_evaluations, payment_settlement_policies, payment_settlement_policy_tiers,
    domain_events, webhook_endpoints
TO gateway_payment;
GRANT UPDATE ON payment_intents, payment_attempts, chain_transfer_processing, domain_events
TO gateway_payment;
GRANT DELETE ON amount_leases TO gateway_payment;
GRANT INSERT ON
    amount_lease_history, audit_events,
    chain_transfer_intent_claims, payment_allocations, payment_fulfillments,
    payment_settlement_decisions, payment_events, domain_events, webhook_deliveries
TO gateway_payment;

-- Reconciler: reads everything it compares, records what it found, publishes
-- component state, and may close a rail. It may not clear one.
GRANT SELECT ON
    chain_transfers, chain_transfer_processing, chain_transfer_state_current,
    payment_intents, payment_allocations, payment_fulfillments,
    payment_settlement_decisions, component_health, rail_stops
TO gateway_reconciler;
GRANT INSERT ON
    reconciliation_runs, reconciliation_discrepancies,
    component_health, component_health_events, rail_stops
TO gateway_reconciler;
GRANT UPDATE ON component_health TO gateway_reconciler;

-- API: merchant writes, operator writes, operator reads. The quote expiry
-- sweep is a worker role in production (GATEWAY_EXPIRY_ENABLED=false on the
-- API), so the API cannot delete a lease.
GRANT SELECT ON
    merchants, merchant_api_keys, operator_api_keys,
    payment_intents, payment_attempts, payment_quotes,
    amount_leases, amount_lease_history, api_idempotency_records,
    price_snapshots, price_readings, quote_policies, rail_health_snapshots, rail_stops,
    chain_observation_conflicts, chain_observation_conflict_items,
    chain_transfers, chain_transfer_attestations, chain_transfer_intent_claims,
    chain_transfer_processing, chain_transfer_state_current,
    payment_allocations, payment_events, payment_fulfillments, payment_settlement_decisions,
    manual_resolution_requests, overpayment_remainder_dispositions,
    operator_risk_provider_bindings,
    component_health, domain_events, webhook_deliveries,
    reconciliation_runs, reconciliation_discrepancies
TO gateway_api;
GRANT INSERT ON
    payment_intents, payment_attempts, payment_quotes,
    amount_leases, amount_lease_history, api_idempotency_records, audit_events,
    price_readings, price_snapshots, rail_health_snapshots, rail_stops,
    payment_risk_evaluations, manual_resolution_requests,
    overpayment_remainder_dispositions, chain_transfer_intent_claims,
    payment_allocations, payment_fulfillments, payment_settlement_decisions,
    payment_events, domain_events
TO gateway_api;
GRANT UPDATE ON
    payment_intents, payment_attempts, api_idempotency_records,
    merchant_api_keys, operator_api_keys, rail_stops, chain_transfer_processing,
    payment_allocations, payment_settlement_decisions
TO gateway_api;

-- Read-only: the operator views, for a person or a dashboard. No credentials,
-- no request payloads, no endpoint URLs.
GRANT SELECT ON
    merchants, payment_intents, payment_attempts, payment_quotes,
    chain_assets, collector_addresses, chain_sources, chain_finality_policies,
    chain_observations, chain_cursors, chain_event_verdicts,
    chain_observation_conflicts, chain_observation_conflict_items,
    chain_transfers, chain_transfer_attestations, chain_transfer_intent_claims,
    chain_transfer_processing, chain_transfer_state_current,
    payment_allocations, payment_events, payment_fulfillments, payment_settlement_decisions,
    payment_risk_evaluations, payment_settlement_policies, payment_settlement_policy_tiers,
    price_snapshots, price_readings, quote_policies, rail_health_snapshots, rail_stops,
    component_health, component_health_events, domain_events, webhook_deliveries,
    reconciliation_runs, reconciliation_discrepancies,
    manual_resolution_requests, overpayment_remainder_dispositions,
    operator_risk_provider_bindings
TO gateway_readonly;

-- A table the next migration creates is readable by the two roles that only
-- ever read, and by nobody else until this file names it.
ALTER DEFAULT PRIVILEGES FOR ROLE gateway_migrator IN SCHEMA public
    GRANT SELECT ON TABLES TO gateway_readonly, gateway_reconciler;
