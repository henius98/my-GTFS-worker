SELECT DISTINCT trips.route_id, stops.stop_id, stops.stop_name, stops.stop_lat AS lat, stops.stop_lon AS lng
FROM trips JOIN stop_times ON stop_times.trip_id = trips.trip_id
JOIN stops ON stops.stop_id = stop_times.stop_id__TRIP_FILTER__
ORDER BY trips.route_id, stops.stop_id
