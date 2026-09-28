-- Two people for money a person moves.
--
-- Honoring parked money writes an allocation, pays an intent and tells the
-- merchant. Above the configured threshold one operator key only proposes it:
-- the proposal stores the exact command, what the rows looked like, and who
-- asked, and moves no money. A second, different operator key approves it;
-- the approval re-reads and locks the same rows and runs every honor check
-- again, so a proposal cannot carry a decision past a change in the facts.
-- A proposal is approved, rejected, or expires; it is never edited.

CREATE TABLE manual_honor_proposals (
    id UUID PRIMARY KEY,
    proposer_key_id UUID NOT NULL REFERENCES operator_api_keys(id),
    proposer_label TEXT NOT NULL CHECK (char_length(proposer_label) BETWEEN 1 AND 100),
    idempotency_key TEXT NOT NULL CHECK (char_length(idempotency_key) BETWEEN 16 AND 128),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    transfer_id UUID NOT NULL REFERENCES chain_transfers(id),
    payment_intent_id UUID NOT NULL REFERENCES payment_intents(id),
    attempt_id UUID NOT NULL REFERENCES payment_attempts(id),
    merchant_id UUID NOT NULL REFERENCES merchants(id),
    allocate_raw NUMERIC(78, 0) NOT NULL CHECK (allocate_raw > 0),
    threshold_raw NUMERIC(78, 0) NOT NULL CHECK (threshold_raw >= 0),
    reason TEXT NOT NULL CHECK (char_length(btrim(reason)) BETWEEN 1 AND 1000),
    evidence JSONB NOT NULL CHECK (jsonb_typeof(evidence) = 'object'),
    status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'expired')),
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    decided_by_key_id UUID REFERENCES operator_api_keys(id),
    decided_by_label TEXT,
    decision_idempotency_key TEXT,
    decision_reason TEXT,
    decided_at TIMESTAMPTZ,
    resolution_id UUID UNIQUE REFERENCES manual_resolution_requests(id),
    UNIQUE (proposer_key_id, idempotency_key),
    CHECK (expires_at > created_at),
    CHECK ((status = 'pending') = (decided_at IS NULL)),
    CHECK ((status IN ('approved', 'rejected')) = (decided_by_key_id IS NOT NULL)),
    CHECK ((status = 'approved') = (resolution_id IS NOT NULL)),
    -- The database itself refuses a proposal approved by the key that made it.
    CHECK (status <> 'approved' OR decided_by_key_id <> proposer_key_id)
);

-- One open question per transfer: two pending proposals for the same money
-- would ask two approvers to decide the same thing.
CREATE UNIQUE INDEX manual_honor_proposals_one_pending_idx
    ON manual_honor_proposals (transfer_id)
    WHERE status = 'pending';

CREATE INDEX manual_honor_proposals_status_idx
    ON manual_honor_proposals (status, id DESC);
