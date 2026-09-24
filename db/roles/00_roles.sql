-- Group roles, one per kind of process. Applied once per database, safe to
-- apply again.
--
-- Nobody connects as a group. Each process connects as a LOGIN role that is a
-- member of exactly one group, so a leaked credential names the process it
-- belonged to and carries only that process's privileges. Passwords are never
-- in this repository: the operator creates the login roles with the command
-- below, taking the password from a secret manager.
--
--   CREATE ROLE gateway_api_prod       LOGIN PASSWORD '<from secrets>' IN ROLE gateway_api;
--   CREATE ROLE gateway_verifier_prod  LOGIN PASSWORD '<from secrets>' IN ROLE gateway_verifier;
--   CREATE ROLE gateway_payment_prod   LOGIN PASSWORD '<from secrets>' IN ROLE gateway_payment;
--   CREATE ROLE gateway_reconciler_prod LOGIN PASSWORD '<from secrets>' IN ROLE gateway_reconciler;
--   CREATE ROLE gateway_migrator_prod  LOGIN PASSWORD '<from secrets>' IN ROLE gateway_migrator;
--   CREATE ROLE gateway_readonly_prod  LOGIN PASSWORD '<from secrets>' IN ROLE gateway_readonly;
--
-- Observers are different: every chain source gets its own login role, named
-- exactly as the source's `db_principal`, because row level security ties an
-- observation and a cursor to `session_user`. See
-- 20_observer_source_role.sql.template.
--
-- The verifier also speaks as a chain source (its own re-read is recorded as
-- evidence), so its login role name must equal the verifier source's
-- `db_principal` as well.

DO $$
DECLARE
    role_name TEXT;
BEGIN
    FOREACH role_name IN ARRAY ARRAY[
        'gateway_migrator',
        'gateway_api',
        'gateway_observer',
        'gateway_verifier',
        'gateway_payment',
        'gateway_reconciler',
        'gateway_readonly'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = role_name) THEN
            EXECUTE format('CREATE ROLE %I NOLOGIN', role_name);
        END IF;
        EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I', current_database(), role_name);
    END LOOP;
END
$$;
