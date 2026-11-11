-- Migration: Change stop_times primary key to (stop_id, trip_id, stop_sequence).
CREATE TABLE IF NOT EXISTS "stop_times_new" (
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  stop_headsign TEXT,
  shape_dist_traveled REAL,
  pickup_type INTEGER,
  drop_off_type INTEGER,
  PRIMARY KEY (stop_id, trip_id, stop_sequence)
);

DROP INDEX IF EXISTS idx_stop_times_stop_departure;

INSERT OR REPLACE INTO "stop_times_new" (
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, stop_headsign, shape_dist_traveled, pickup_type, drop_off_type
)
SELECT
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, stop_headsign, shape_dist_traveled, pickup_type, drop_off_type
FROM "stop_times";

DROP TABLE "stop_times";

ALTER TABLE "stop_times_new" RENAME TO "stop_times";
