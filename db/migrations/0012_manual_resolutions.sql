-- Admin decisions for exceptional inbound money. These rows record decisions;
-- they never claim that the gateway sent an on-chain refund.

ALTER TABLE chain_transfer_processing
    DROP CONSTRAINT chain_transfer_processing_processing_state_check,
    ADD CONSTRAINT chain_transfer_processing_processing_state_check
        CHECK (processing_state IN ('pending', 'matched', 'settled', 'unmatched', 'held', 'resolved'));

CREATE TABLE manual_resolution_requests (
    id UUID PRIMARY KEY,
    operator_key_id UUID NOT NULL REFERENCES operator_api_keys(id),
    idempotency_key TEXT NOT NULL CHECK (char_length(idempotency_key) BETWEEN 16 AND 128),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    action TEXT NOT NULL CHECK (action IN ('honor', 'reject', 'record_remainder_disposition')),
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    payment_intent_id UUID REFERENCES payment_intents(id),
    attempt_id UUID REFERENCES payment_attempts(id),
    merchant_id UUID REFERENCES merchants(id),
    allocated_raw NUMERIC(78, 0) CHECK (allocated_raw IS NULL OR allocated_raw > 0),
    remainder_raw NUMERIC(78, 0) CHECK (remainder_raw IS NULL OR remainder_raw > 0),
    disposition TEXT CHECK (disposition IS NULL OR disposition IN (
        'refunded_externally', 'credited_externally', 'donated_externally', 'retained_by_agreement'
    )),
    external_reference TEXT CHECK (external_reference IS NULL OR char_length(external_reference) BETWEEN 1 AND 200),
    reason TEXT NOT NULL CHECK (char_length(btrim(reason)) BETWEEN 1 AND 1000),
    actor_label TEXT NOT NULL CHECK (char_length(actor_label) BETWEEN 1 AND 100),
    result_status TEXT NOT NULL CHECK (result_status IN ('completed')),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (operator_key_id, idempotency_key)
);

CREATE INDEX manual_resolution_requests_transfer_idx
    ON manual_resolution_requests (transfer_id, created_at DESC);

CREATE TABLE overpayment_remainder_dispositions (
    id UUID PRIMARY KEY,
    resolution_id UUID NOT NULL UNIQUE REFERENCES manual_resolution_requests(id),
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    remainder_raw NUMERIC(78, 0) NOT NULL CHECK (remainder_raw > 0),
    disposition TEXT NOT NULL CHECK (disposition IN (
        'refunded_externally', 'credited_externally', 'donated_externally', 'retained_by_agreement'
    )),
    external_reference TEXT NOT NULL CHECK (char_length(btrim(external_reference)) BETWEEN 1 AND 200),
    recorded_by UUID NOT NULL REFERENCES operator_api_keys(id),
    reason TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    UNIQUE (transfer_id, payment_intent_id)
);
