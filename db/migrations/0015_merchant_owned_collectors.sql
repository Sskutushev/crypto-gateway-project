-- Whose address receives a merchant's money.
--
-- 'own'    : quotes use only collectors registered to that merchant. The
--            merchant holds the key; the gateway only watches the address and
--            can never move the funds. This is the default for every merchant
--            created from now on.
-- 'shared' : quotes use only operator collectors (merchant_id IS NULL). The
--            operator then owes the merchant the money and must account for it
--            outside this system. Merchants that existed before this migration
--            were quoted this way, and keep it until someone decides otherwise.
--
-- There is no fallback between the two: a merchant on 'own' with no active
-- collector of its own gets no quote, rather than a quote on someone else's
-- address.

ALTER TABLE merchants
    ADD COLUMN collector_policy TEXT NOT NULL DEFAULT 'own'
        CHECK (collector_policy IN ('own', 'shared'));
UPDATE merchants SET collector_policy = 'shared';

ALTER TABLE collector_addresses ADD COLUMN merchant_id UUID REFERENCES merchants (id);
CREATE INDEX collector_addresses_merchant_asset_idx
    ON collector_addresses (merchant_id, asset_id)
    WHERE merchant_id IS NOT NULL;

-- The quote path checks this in its own statements; the trigger makes the
-- database refuse the same mistake from any other path.
CREATE FUNCTION payment_quote_collector_tenancy() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM collector_addresses AS collector
          JOIN merchants AS merchant ON merchant.id = NEW.merchant_id
         WHERE collector.id = NEW.collector_address_id
           AND ((merchant.collector_policy = 'own' AND collector.merchant_id = merchant.id)
             OR (merchant.collector_policy = 'shared' AND collector.merchant_id IS NULL))
    ) THEN
        RAISE EXCEPTION 'collector % does not receive money for merchant %',
            NEW.collector_address_id, NEW.merchant_id
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER payment_quotes_collector_tenancy
    BEFORE INSERT ON payment_quotes
    FOR EACH ROW EXECUTE FUNCTION payment_quote_collector_tenancy();
