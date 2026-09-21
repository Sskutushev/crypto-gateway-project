ALTER TABLE payment_intents
    ADD CONSTRAINT payment_intents_id_merchant_key UNIQUE (id, merchant_id);

ALTER TABLE collector_addresses
    ADD CONSTRAINT collector_addresses_id_asset_key UNIQUE (id, asset_id);

ALTER TABLE payment_quotes
    ADD CONSTRAINT payment_quotes_intent_merchant_fk
        FOREIGN KEY (payment_intent_id, merchant_id)
        REFERENCES payment_intents (id, merchant_id),
    ADD CONSTRAINT payment_quotes_collector_asset_fk
        FOREIGN KEY (collector_address_id, asset_id)
        REFERENCES collector_addresses (id, asset_id),
    ADD CONSTRAINT payment_quotes_identity_key
        UNIQUE (id, merchant_id, payment_intent_id, collector_address_id);

ALTER TABLE payment_attempts
    ADD CONSTRAINT payment_attempts_quote_identity_fk
        FOREIGN KEY (quote_id, merchant_id, payment_intent_id, collector_address_id)
        REFERENCES payment_quotes (id, merchant_id, payment_intent_id, collector_address_id),
    ADD CONSTRAINT payment_attempts_id_collector_key UNIQUE (id, collector_address_id);

ALTER TABLE amount_leases
    ADD CONSTRAINT amount_leases_attempt_collector_fk
        FOREIGN KEY (attempt_id, collector_address_id)
        REFERENCES payment_attempts (id, collector_address_id);

ALTER TABLE amount_lease_history
    ADD CONSTRAINT amount_lease_history_attempt_collector_fk
        FOREIGN KEY (attempt_id, collector_address_id)
        REFERENCES payment_attempts (id, collector_address_id);

