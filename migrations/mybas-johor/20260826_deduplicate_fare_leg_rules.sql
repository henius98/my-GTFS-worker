-- Migration number: 0001 	 2026-08-26T11:47:41.572Z

-- Existing imports could replay this table because it had no conflict key.
-- The one-off scripts/deduplicate-mybas-fare-leg-rules.sh rebuild preserves
-- one row per logical key in local SQLite before importing a replacement D1 database.

CREATE UNIQUE INDEX IF NOT EXISTS uq_fare_leg_rules_import_key
ON fare_leg_rules (leg_group_id, from_area_id, to_area_id, fare_product_id);
