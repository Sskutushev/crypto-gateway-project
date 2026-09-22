-- The two things that tell an operator the gateway is still telling the truth:
-- what state each component is in, and whether the money adds up.
--
-- A fallback nobody can see is the failure this schema exists to prevent. Every
-- component state change is a row, and every reconciliation run leaves a
-- record whether or not it found anything.

CREATE TABLE component_health (
    component TEXT PRIMARY KEY CHECK (char_length(component) BETWEEN 1 AND 120),
    state TEXT NOT NULL CHECK (state IN ('ok', 'degraded', 'unavailable', 'diverged', 'stopped')),
    detail TEXT,
    -- When this state began, so "degraded for six hours" is answerable without
    -- reading the event history.
    since TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX component_health_state_idx ON component_health (state, since);

-- Every transition, including the recoveries. A component that flaps is a
-- different problem from one that is simply down, and only the history tells
-- them apart.
CREATE TABLE component_health_events (
    id UUID PRIMARY KEY,
    component TEXT NOT NULL,
    previous_state TEXT,
    new_state TEXT NOT NULL
        CHECK (new_state IN ('ok', 'degraded', 'unavailable', 'diverged', 'stopped')),
    detail TEXT,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX component_health_events_component_idx
    ON component_health_events (component, created_at DESC);

CREATE TABLE reconciliation_runs (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('incremental', 'daily')),
    window_start TIMESTAMPTZ NOT NULL,
    window_end TIMESTAMPTZ NOT NULL,
    transfers_examined INTEGER NOT NULL DEFAULT 0 CHECK (transfers_examined >= 0),
    intents_examined INTEGER NOT NULL DEFAULT 0 CHECK (intents_examined >= 0),
    discrepancy_count INTEGER NOT NULL DEFAULT 0 CHECK (discrepancy_count >= 0),
    money_discrepancy_count INTEGER NOT NULL DEFAULT 0 CHECK (money_discrepancy_count >= 0),
    -- `drift` is something to explain today. `hard_stop` is money that does not
    -- add up, and it closes the rail on its own.
    status TEXT NOT NULL CHECK (status IN ('ok', 'drift', 'hard_stop')),
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ,
    CHECK (window_end > window_start)
);

CREATE INDEX reconciliation_runs_recent_idx ON reconciliation_runs (started_at DESC);

CREATE TABLE reconciliation_discrepancies (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES reconciliation_runs(id),
    kind TEXT NOT NULL CHECK (kind IN (
        'observed_not_canonical',
        'allocation_exceeds_transfer',
        'settled_not_fulfilled',
        'fulfilled_not_settled',
        'unmatched_inbound_aging',
        'allocated_on_invalidated_transfer',
        'observer_behind',
        'held_payment_aging'
    )),
    -- True when the finding is about money rather than about a counter. A
    -- money finding stops the rail; a counter finding is explained in daylight.
    money_affected BOOLEAN NOT NULL,
    transfer_id UUID REFERENCES chain_transfers(id),
    payment_intent_id UUID REFERENCES payment_intents(id),
    asset_id UUID REFERENCES chain_assets(id),
    detail JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(detail) = 'object'),
    created_at TIMESTAMPTZ NOT NULL,
    resolved_at TIMESTAMPTZ,
    resolved_by TEXT,
    resolution TEXT,
    CHECK ((resolved_at IS NULL) = (resolved_by IS NULL))
);

CREATE INDEX reconciliation_discrepancies_open_idx
    ON reconciliation_discrepancies (kind, created_at DESC)
    WHERE resolved_at IS NULL;

CREATE INDEX reconciliation_discrepancies_run_idx
    ON reconciliation_discrepancies (run_id);
