-- Migration number: 0001 	 2026-08-26T11:47:41.572Z
-- Existing duplicate rows were cleared manually before applying this migration.
-- Duplicate cleanup is intentionally disabled; this only enforces future idempotency.
-- DELETE FROM fare_leg_rules
-- WHERE rowid NOT IN (
--     SELECT MIN(rowid)
--     FROM fare_leg_rules
--     GROUP BY leg_group_id, from_area_id, to_area_id, fare_product_id
-- );
CREATE UNIQUE INDEX IF NOT EXISTS uq_fare_leg_rules_import_key ON fare_leg_rules (
  leg_group_id,
  from_area_id,
  to_area_id,
  fare_product_id
);
