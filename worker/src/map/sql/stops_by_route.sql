WITH
  trip_stop_times AS (
    SELECT
      json_extract(input.value, '$[0]') AS route_id,
      stop_times.stop_id,
      GROUP_CONCAT(stop_times.arrival_time, ', ') AS arrival_times
    FROM json_each(?1) AS input
    JOIN stop_times ON stop_times.trip_id = json_extract(input.value, '$[1]')
    GROUP BY route_id, stop_times.stop_id
  )
SELECT
  trip_stop_times.route_id,
  stops.stop_id,
  stops.stop_name,
  stops.stop_lat AS lat,
  stops.stop_lon AS lng,
  trip_stop_times.arrival_times
FROM trip_stop_times
JOIN stops ON stops.stop_id = trip_stop_times.stop_id
ORDER BY trip_stop_times.route_id, trip_stop_times.stop_id
