ALTER TABLE price_snapshots
    ADD CONSTRAINT price_snapshots_identity_key
        UNIQUE (id, asset_id, fiat_currency);

ALTER TABLE quote_policies
    ADD CONSTRAINT quote_policies_identity_key
        UNIQUE (id, asset_id, fiat_currency);

ALTER TABLE rail_health_snapshots
    ADD CONSTRAINT rail_health_snapshots_identity_key
        UNIQUE (id, asset_id);

ALTER TABLE payment_quotes
    ADD CONSTRAINT payment_quotes_price_snapshot_identity_fk
        FOREIGN KEY (price_snapshot_id, asset_id, fiat_currency)
        REFERENCES price_snapshots (id, asset_id, fiat_currency),
    ADD CONSTRAINT payment_quotes_policy_identity_fk
        FOREIGN KEY (quote_policy_id, asset_id, fiat_currency)
        REFERENCES quote_policies (id, asset_id, fiat_currency),
    ADD CONSTRAINT payment_quotes_rail_health_identity_fk
        FOREIGN KEY (rail_health_snapshot_id, asset_id)
        REFERENCES rail_health_snapshots (id, asset_id);

