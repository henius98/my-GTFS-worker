WITH
__ACTIVE_SERVICES__,
  route_stops AS (
    SELECT
      trips.route_id,
      stop_times.stop_id,
      GROUP_CONCAT(stop_times.arrival_time, ', ') AS arrival_times,
      GROUP_CONCAT(stop_times.departure_time, ', ') AS departure_times
    FROM stop_times
    JOIN trips ON trips.trip_id = stop_times.trip_id
    JOIN active_services ON active_services.service_id = trips.service_id
    GROUP BY trips.route_id, stop_times.stop_id
  )
SELECT
  route_stops.route_id,
  stops.stop_id,
  stops.stop_name,
  stops.stop_lat AS lat,
  stops.stop_lon AS lng,
  route_stops.arrival_times,
  route_stops.departure_times
FROM route_stops
JOIN stops ON stops.stop_id = route_stops.stop_id
ORDER BY route_stops.route_id, route_stops.stop_id
