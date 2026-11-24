-- Migration: Change stop_times primary key to (trip_id, stop_id, arrival_time).
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
  PRIMARY KEY (trip_id, stop_id, stop_sequence)
) WITHOUT ROWID;

INSERT OR REPLACE INTO "stop_times_new" (
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, stop_headsign, shape_dist_traveled, pickup_type, drop_off_type
)
SELECT
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, stop_headsign, shape_dist_traveled, pickup_type, drop_off_type
FROM "stop_times"
WHERE trip_id IS NOT NULL AND stop_id IS NOT NULL AND arrival_time IS NOT NULL;

DROP TABLE "stop_times";

ALTER TABLE "stop_times_new" RENAME TO "stop_times";
