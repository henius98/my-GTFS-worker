-- Migration: Fix KTMB sequence duplicates, missing stop 37200, zero-stop trips, and expired records
-- 1. Insert missing stop 37200
INSERT OR IGNORE INTO stops (stop_id, stop_name, stop_lat, stop_lon)
VALUES ('37200', 'LARKIN / TAMPOI HALTE', 1.5050, 103.7400);

-- 2. Delete legacy stop_times sequence numbers (< 100) when sequence >= 100 exists
DELETE FROM stop_times
WHERE stop_sequence < 100
  AND trip_id IN (
    SELECT trip_id FROM stop_times WHERE stop_sequence >= 100
  );

-- 3. Delete trips with zero stop_times
DELETE FROM trips
WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times);

-- 4. Delete expired calendar services
DELETE FROM stop_times
WHERE trip_id IN (
  SELECT trip_id FROM trips WHERE service_id IN (
    SELECT service_id FROM (
      SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id
      UNION ALL
      SELECT service_id, MAX(date) AS max_end FROM calendar_dates WHERE exception_type = 1 GROUP BY service_id
    ) GROUP BY service_id HAVING MAX(max_end) < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)
  )
);

DELETE FROM trips
WHERE service_id IN (
  SELECT service_id FROM (
    SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id
    UNION ALL
    SELECT service_id, MAX(date) AS max_end FROM calendar_dates WHERE exception_type = 1 GROUP BY service_id
  ) GROUP BY service_id HAVING MAX(max_end) < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)
);

DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips);
DELETE FROM calendar_dates WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips);
