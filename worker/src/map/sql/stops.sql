WITH
  route_stops AS (
    SELECT trips.route_id, stop_times.stop_id
    FROM stop_times
    JOIN trips ON trips.trip_id = stop_times.trip_id
    GROUP BY trips.route_id, stop_times.stop_id
  )
SELECT
  route_stops.route_id,
  stops.stop_id,
  stops.stop_name,
  stops.stop_lat AS lat,
  stops.stop_lon AS lng
FROM route_stops
JOIN stops ON stops.stop_id = route_stops.stop_id
ORDER BY route_stops.route_id, route_stops.stop_id
