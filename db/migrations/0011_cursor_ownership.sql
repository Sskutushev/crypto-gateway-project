-- A source's cursor is moved only by the principal that speaks for that
-- source.
--
-- Table grants say which roles may touch chain_cursors at all; they cannot say
-- which rows. Without this policy one observer's login could rewind another
-- observer's recovery point, and the fence token would not notice because the
-- write is a legitimate cursor advance from the database's point of view.
--
-- Reads stay open: the reconciler measures every source against its own head
-- and the self-check compares every cursor with it. A source that opted out of
-- a dedicated principal (development, one process polling several public APIs)
-- has opted out of this protection too, explicitly, in its own row.

ALTER TABLE chain_cursors ENABLE ROW LEVEL SECURITY;
ALTER TABLE chain_cursors FORCE ROW LEVEL SECURITY;

CREATE POLICY chain_cursors_read
    ON chain_cursors
    FOR SELECT
    TO PUBLIC
    USING (true);

CREATE POLICY chain_cursors_insert_own_source
    ON chain_cursors
    FOR INSERT
    TO PUBLIC
    WITH CHECK (
        EXISTS (
            SELECT 1
              FROM chain_sources AS source
             WHERE source.id = chain_cursors.source_id
               AND (source.db_principal = session_user
                    OR NOT source.requires_dedicated_principal)
        )
    );

CREATE POLICY chain_cursors_update_own_source
    ON chain_cursors
    FOR UPDATE
    TO PUBLIC
    USING (
        EXISTS (
            SELECT 1
              FROM chain_sources AS source
             WHERE source.id = chain_cursors.source_id
               AND (source.db_principal = session_user
                    OR NOT source.requires_dedicated_principal)
        )
    )
    WITH CHECK (
        EXISTS (
            SELECT 1
              FROM chain_sources AS source
             WHERE source.id = chain_cursors.source_id
               AND (source.db_principal = session_user
                    OR NOT source.requires_dedicated_principal)
        )
    );

CREATE POLICY chain_cursors_delete_own_source
    ON chain_cursors
    FOR DELETE
    TO PUBLIC
    USING (
        EXISTS (
            SELECT 1
              FROM chain_sources AS source
             WHERE source.id = chain_cursors.source_id
               AND (source.db_principal = session_user
                    OR NOT source.requires_dedicated_principal)
        )
    );
