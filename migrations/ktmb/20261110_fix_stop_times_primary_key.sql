-- Migration: Change stop_times primary key to (stop_id, trip_id, stop_sequence).
CREATE TABLE IF NOT EXISTS "stop_times_new" (
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  shape_dist_traveled REAL,
  PRIMARY KEY (stop_id, trip_id, stop_sequence)
);

DROP INDEX IF EXISTS idx_stop_times_stop_departure;

INSERT OR REPLACE INTO "stop_times_new" (
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, shape_dist_traveled
)
SELECT
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, shape_dist_traveled
FROM "stop_times";

DROP TABLE "stop_times";

ALTER TABLE "stop_times_new" RENAME TO "stop_times";
