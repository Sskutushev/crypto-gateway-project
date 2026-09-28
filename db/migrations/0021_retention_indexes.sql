-- Retention reads the oldest delivery attempts first, a bounded batch at a
-- time. Without this index every batch would scan the whole table to find
-- them; observations and health transitions are already indexed by time.
CREATE INDEX webhook_deliveries_delivered_at_idx ON webhook_deliveries (delivered_at);
