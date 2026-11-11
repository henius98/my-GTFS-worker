-- Migration: Rebuild stop_times and trips to eliminate 3.1M expired ghost records
CREATE TABLE IF NOT EXISTS "stop_times_clean" (
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  stop_headsign TEXT,
  shape_dist_traveled REAL,
  PRIMARY KEY (stop_id, trip_id, stop_sequence)
);

INSERT INTO "stop_times_clean" (
  trip_id, arrival_time, departure_time, stop_id, stop_sequence, stop_headsign, shape_dist_traveled
)
SELECT st.trip_id, st.arrival_time, st.departure_time, st.stop_id, st.stop_sequence, st.stop_headsign, st.shape_dist_traveled
FROM "stop_times" st
JOIN "trips" t ON st.trip_id = t.trip_id
JOIN (
  SELECT service_id, MAX(end_date) AS max_end
  FROM "calendar"
  GROUP BY service_id
) c ON t.service_id = c.service_id
WHERE c.max_end >= CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER);

DROP TABLE "stop_times";

ALTER TABLE "stop_times_clean" RENAME TO "stop_times";

CREATE TABLE IF NOT EXISTS "trips_clean" (
  route_id TEXT,
  service_id TEXT,
  trip_id VARCHAR(20) PRIMARY KEY,
  trip_headsign TEXT,
  direction_id INTEGER,
  shape_id TEXT
);

INSERT INTO "trips_clean" (
  route_id, service_id, trip_id, trip_headsign, direction_id, shape_id
)
SELECT t.route_id, t.service_id, t.trip_id, t.trip_headsign, t.direction_id, t.shape_id
FROM "trips" t
JOIN (
  SELECT service_id, MAX(end_date) AS max_end
  FROM "calendar"
  GROUP BY service_id
) c ON t.service_id = c.service_id
WHERE c.max_end >= CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)
  AND t.trip_id IN (SELECT DISTINCT trip_id FROM "stop_times");

DROP TABLE "trips";

ALTER TABLE "trips_clean" RENAME TO "trips";

DELETE FROM "calendar" WHERE service_id NOT IN (SELECT DISTINCT service_id FROM "trips");
