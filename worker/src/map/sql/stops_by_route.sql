SELECT
  ?1 AS route_id,
  stops.stop_id,
  stops.stop_name,
  stops.stop_lat AS lat,
  stops.stop_lon AS lng
FROM stops
WHERE EXISTS (
  SELECT 1
  FROM stop_times
  JOIN trips ON trips.trip_id = stop_times.trip_id
  WHERE stop_times.stop_id = stops.stop_id
    AND trips.route_id = ?1
)
ORDER BY stops.stop_id
