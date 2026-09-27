-- One order, several payment attempts, and a page the buyer can open.
--
-- An order whose quote ran out before any money arrived can be quoted again:
-- a new quote and a new attempt, on the same payment intent and the same
-- merchant reference. At most one attempt of an intent is waiting for money
-- at any time. The earlier attempt keeps its exact-amount reservation until
-- its late-payment window ends, so money sent late to the old amount is still
-- recognised and handed to a person rather than lost or credited elsewhere.

ALTER TABLE payment_quotes DROP CONSTRAINT payment_quotes_payment_intent_id_key;
CREATE UNIQUE INDEX payment_attempts_one_live_per_intent
    ON payment_attempts (payment_intent_id)
    WHERE status = 'awaiting_payment';
CREATE INDEX payment_quotes_intent_created_idx
    ON payment_quotes (payment_intent_id, created_at DESC);

-- The capability behind the hosted checkout page. It reveals what the buyer
-- sees anyway (address, amount, status) and authorises nothing, so it is kept
-- as is; 64 hex characters from two random UUIDs carry 244 random bits.
ALTER TABLE payment_attempts ADD COLUMN checkout_token TEXT;
UPDATE payment_attempts
   SET checkout_token = replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', '')
 WHERE checkout_token IS NULL;
ALTER TABLE payment_attempts
    ALTER COLUMN checkout_token SET NOT NULL,
    ALTER COLUMN checkout_token
        SET DEFAULT replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', ''),
    ADD CONSTRAINT payment_attempts_checkout_token_format
        CHECK (checkout_token ~ '^[0-9a-f]{64}$');
CREATE UNIQUE INDEX payment_attempts_checkout_token_key ON payment_attempts (checkout_token);
