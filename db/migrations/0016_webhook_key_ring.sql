-- A webhook secret is rotated per endpoint, with a transition period.
--
-- While the previous version is still valid, every delivery is signed with
-- both the new and the previous secret (`t=...,v1=<new>,v1=<previous>`), so a
-- merchant switches its verifier at its own pace and no event is refused in
-- between. One merchant's rotation never depends on another's.

ALTER TABLE webhook_endpoints
    ADD COLUMN previous_secret_version INTEGER,
    ADD COLUMN previous_secret_fingerprint BYTEA
        CHECK (octet_length(previous_secret_fingerprint) = 32),
    ADD COLUMN previous_valid_until TIMESTAMPTZ,
    ADD CONSTRAINT webhook_endpoints_previous_secret_complete CHECK (
        (previous_secret_version IS NULL) = (previous_secret_fingerprint IS NULL)
        AND (previous_secret_version IS NULL) = (previous_valid_until IS NULL)
    ),
    ADD CONSTRAINT webhook_endpoints_previous_secret_older CHECK (
        previous_secret_version IS NULL OR previous_secret_version < secret_version
    );
