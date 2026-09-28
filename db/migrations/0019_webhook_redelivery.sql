-- Sending an event again, as the same event.
--
-- A redelivery re-queues the outbox row itself: same id, same payload, same
-- created_at, so the envelope a merchant receives is byte-for-byte the one it
-- may already have seen and deduplicates by id. Nothing is copied, so there is
-- no second event a merchant could mistake for a second payment.
--
-- Attempt numbers keep growing across redeliveries, so no earlier delivery
-- record is overwritten; `attempt_floor` remembers where the new retry budget
-- starts. `target_endpoint_id` limits the redelivery to one endpoint, and the
-- composite key below makes the database refuse an endpoint of another
-- merchant.

ALTER TABLE webhook_endpoints
    ADD CONSTRAINT webhook_endpoints_id_merchant_key UNIQUE (id, merchant_id);

ALTER TABLE domain_events
    ADD COLUMN target_endpoint_id UUID,
    ADD COLUMN attempt_floor INTEGER NOT NULL DEFAULT 0 CHECK (attempt_floor >= 0),
    ADD CONSTRAINT domain_events_attempt_floor_bounded CHECK (attempt_floor <= attempts),
    ADD CONSTRAINT domain_events_target_endpoint_fkey
        FOREIGN KEY (target_endpoint_id, merchant_id)
        REFERENCES webhook_endpoints (id, merchant_id);

-- One row per redelivery request: who asked, why, and what the event looked
-- like before. `principal` is the idempotency scope: an operator key
-- (`operator_key:<uuid>`) or a named person at the admin command line
-- (`admin:<name>`).
CREATE TABLE webhook_redeliveries (
    id UUID PRIMARY KEY,
    event_id UUID NOT NULL REFERENCES domain_events(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    endpoint_id UUID,
    principal TEXT NOT NULL CHECK (char_length(principal) BETWEEN 1 AND 200),
    operator_key_id UUID REFERENCES operator_api_keys(id),
    actor_label TEXT NOT NULL CHECK (char_length(actor_label) BETWEEN 1 AND 100),
    idempotency_key TEXT NOT NULL CHECK (char_length(idempotency_key) BETWEEN 16 AND 128),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    reason TEXT NOT NULL CHECK (char_length(btrim(reason)) BETWEEN 1 AND 1000),
    previous_state TEXT NOT NULL CHECK (previous_state IN ('delivered', 'dead_lettered')),
    previous_attempts INTEGER NOT NULL CHECK (previous_attempts >= 0),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (principal, idempotency_key),
    FOREIGN KEY (endpoint_id, merchant_id) REFERENCES webhook_endpoints (id, merchant_id)
);

CREATE INDEX webhook_redeliveries_event_idx ON webhook_redeliveries (event_id, created_at DESC);
