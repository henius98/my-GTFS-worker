-- Migration: Change trips primary key to (route_id, service_id, trip_id).
CREATE TABLE IF NOT EXISTS "trips_new" (
  route_id TEXT,
  service_id TEXT,
  trip_id VARCHAR(20),
  direction_id INTEGER,
  PRIMARY KEY (route_id, service_id, trip_id)
) WITHOUT ROWID;

INSERT OR REPLACE INTO "trips_new" (
  route_id, service_id, trip_id, direction_id
)
SELECT
  route_id, service_id, trip_id, direction_id
FROM "trips"
WHERE route_id IS NOT NULL AND service_id IS NOT NULL AND trip_id IS NOT NULL;

DROP TABLE "trips";

ALTER TABLE "trips_new" RENAME TO "trips";
