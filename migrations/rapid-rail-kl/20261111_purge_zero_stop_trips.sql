-- Migration: Purge zero-stop trips and expired records
DELETE FROM frequencies
WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times);

DELETE FROM trips
WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times);

DELETE FROM stop_times
WHERE trip_id IN (
  SELECT t.trip_id FROM trips t
  JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id
  WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)
);

DELETE FROM trips
WHERE trip_id IN (
  SELECT t.trip_id FROM trips t
  JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id
  WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)
);

DELETE FROM frequencies WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM trips);
DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips);

UPDATE stop_times
SET
  arrival_time = CASE
    WHEN LENGTH(arrival_time) = 7 THEN '0' || arrival_time
    ELSE arrival_time
  END,
  departure_time = CASE
    WHEN LENGTH(departure_time) = 7 THEN '0' || departure_time
    ELSE departure_time
  END
WHERE
  LENGTH(arrival_time) = 7
  OR LENGTH(departure_time) = 7;

UPDATE frequencies
SET
  start_time = CASE
    WHEN LENGTH(start_time) = 7 THEN '0' || start_time
    ELSE start_time
  END,
  end_time = CASE
    WHEN LENGTH(end_time) = 7 THEN '0' || end_time
    ELSE end_time
  END
WHERE
  LENGTH(start_time) = 7
  OR LENGTH(end_time) = 7;
