-- Money is only ever tied to an obligation through the collector it arrived at.
--
-- The application already checks that a transfer belongs to the attempt it
-- pays (same collector, same asset, same rail). These constraints make the
-- database refuse the same mistake from any path, including a future bug or a
-- hand-written statement: an allocation or a claim names one collector, and
-- both the attempt and the transfer must be on it.

-- A transfer is in the asset of the collector it arrived at
-- (collector_addresses (id, asset_id) is unique since 0003).
ALTER TABLE chain_transfers
    ADD CONSTRAINT chain_transfers_collector_asset_fk
        FOREIGN KEY (collector_address_id, asset_id)
        REFERENCES collector_addresses (id, asset_id);

-- Existing rows take the collector of their attempt. A row whose transfer is
-- on another collector fails the constraints below: that is a real historical
-- defect to investigate, not something to paper over.
ALTER TABLE payment_allocations ADD COLUMN collector_address_id UUID;
UPDATE payment_allocations AS allocation
   SET collector_address_id = attempt.collector_address_id
  FROM payment_attempts AS attempt
 WHERE attempt.id = allocation.attempt_id;
ALTER TABLE payment_allocations ALTER COLUMN collector_address_id SET NOT NULL;
ALTER TABLE payment_allocations
    ADD CONSTRAINT payment_allocations_attempt_collector_fk
        FOREIGN KEY (attempt_id, collector_address_id)
        REFERENCES payment_attempts (id, collector_address_id),
    ADD CONSTRAINT payment_allocations_transfer_collector_fk
        FOREIGN KEY (transfer_id, collector_address_id)
        REFERENCES chain_transfers (id, collector_address_id);

ALTER TABLE chain_transfer_intent_claims ADD COLUMN collector_address_id UUID;
UPDATE chain_transfer_intent_claims AS claim
   SET collector_address_id = attempt.collector_address_id
  FROM payment_attempts AS attempt
 WHERE attempt.id = claim.attempt_id;
ALTER TABLE chain_transfer_intent_claims ALTER COLUMN collector_address_id SET NOT NULL;
ALTER TABLE chain_transfer_intent_claims
    ADD CONSTRAINT chain_transfer_intent_claims_attempt_collector_fk
        FOREIGN KEY (attempt_id, collector_address_id)
        REFERENCES payment_attempts (id, collector_address_id),
    ADD CONSTRAINT chain_transfer_intent_claims_transfer_collector_fk
        FOREIGN KEY (transfer_id, collector_address_id)
        REFERENCES chain_transfers (id, collector_address_id);
