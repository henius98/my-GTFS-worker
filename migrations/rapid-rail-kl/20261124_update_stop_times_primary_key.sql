-- Migration: Change stop_times primary key to (trip_id, stop_id, arrival_time).
CREATE TABLE IF NOT EXISTS "stop_times_new" (
  route_id TEXT,
  direction_id INTEGER,
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  PRIMARY KEY (trip_id, stop_id, stop_sequence)
) WITHOUT ROWID;

INSERT OR REPLACE INTO "stop_times_new" (
  route_id, direction_id, trip_id, arrival_time, departure_time, stop_id, stop_sequence
)
SELECT
  route_id, direction_id, trip_id, arrival_time, departure_time, stop_id, stop_sequence
FROM "stop_times"
WHERE trip_id IS NOT NULL AND stop_id IS NOT NULL AND arrival_time IS NOT NULL;

DROP TABLE "stop_times";

ALTER TABLE "stop_times_new" RENAME TO "stop_times";
