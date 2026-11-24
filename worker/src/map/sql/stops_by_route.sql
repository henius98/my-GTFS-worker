WITH
  trip_stop_times AS (
    SELECT stop_id, GROUP_CONCAT(arrival_time, ', ') AS arrival_times, GROUP_CONCAT(departure_time, ', ') AS departure_times
    FROM stop_times
    WHERE trip_id IN (SELECT value FROM json_each(?1))
    GROUP BY stop_id
  )
SELECT
  stops.stop_id,
  stops.stop_name,
  stops.stop_lat AS lat,
  stops.stop_lon AS lng,
  trip_stop_times.arrival_times,
  trip_stop_times.departure_times
FROM trip_stop_times
JOIN stops ON stops.stop_id = trip_stop_times.stop_id
ORDER BY trip_stop_times.stop_id
