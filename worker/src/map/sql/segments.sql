WITH consecutive AS (
  SELECT trips.route_id, stop_times.stop_id AS to_stop_id,
         LAG(stop_times.stop_id) OVER (PARTITION BY trips.trip_id ORDER BY stop_times.stop_sequence) AS from_stop_id
  FROM trips JOIN stop_times ON stop_times.trip_id = trips.trip_id__TRIP_FILTER__
)
SELECT DISTINCT consecutive.route_id, consecutive.from_stop_id, consecutive.to_stop_id,
       from_stop.stop_lat AS from_lat, from_stop.stop_lon AS from_lng,
       to_stop.stop_lat AS to_lat, to_stop.stop_lon AS to_lng
FROM consecutive
JOIN stops AS from_stop ON from_stop.stop_id = consecutive.from_stop_id
JOIN stops AS to_stop ON to_stop.stop_id = consecutive.to_stop_id
WHERE consecutive.from_stop_id <> consecutive.to_stop_id
ORDER BY consecutive.route_id, consecutive.from_stop_id, consecutive.to_stop_id
