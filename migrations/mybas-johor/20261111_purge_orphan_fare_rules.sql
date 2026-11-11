-- Migration: Purge orphan fare rules, zero-stop trips, and expired records
DELETE FROM fare_leg_rules
WHERE fare_product_id NOT IN (SELECT fare_product_id FROM fare_products);

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

DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips);
