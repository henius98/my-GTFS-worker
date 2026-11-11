-- Migration: Change stop_times primary key to (stop_id, trip_id, stop_sequence).
CREATE TABLE IF NOT EXISTS "stop_times_new" (
  route_id TEXT,
  direction_id INTEGER,
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  PRIMARY KEY (stop_id, trip_id, stop_sequence)
);

DROP INDEX IF EXISTS idx_stop_times_stop_departure;

INSERT OR REPLACE INTO "stop_times_new" (
  route_id, direction_id, trip_id, arrival_time, departure_time, stop_id, stop_sequence
)
SELECT
  route_id, direction_id, trip_id, arrival_time, departure_time, stop_id, stop_sequence
FROM "stop_times";

DROP TABLE "stop_times";

ALTER TABLE "stop_times_new" RENAME TO "stop_times";
