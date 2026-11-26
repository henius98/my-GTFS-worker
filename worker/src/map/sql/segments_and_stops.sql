WITH
__ACTIVE_SERVICES__,
  trip_stop_times AS MATERIALIZED (
    SELECT
      trips.route_id,
      trips.trip_id,
      stop_times.stop_id,
      stop_times.stop_sequence,
      stop_times.arrival_time,
      LAG(stop_times.stop_id) OVER (PARTITION BY trips.trip_id ORDER BY stop_times.stop_sequence) AS prev_stop_id
    FROM trips
    JOIN active_services ON active_services.service_id = trips.service_id
    JOIN stop_times ON stop_times.trip_id = trips.trip_id
  ),
  distinct_segments AS (
    SELECT
      route_id,
      MIN(prev_stop_id, stop_id) AS from_stop_id,
      MAX(prev_stop_id, stop_id) AS to_stop_id
    FROM trip_stop_times
    WHERE prev_stop_id IS NOT NULL
      AND prev_stop_id <> stop_id
    GROUP BY route_id, MIN(prev_stop_id, stop_id), MAX(prev_stop_id, stop_id)
  ),
  route_stops AS (
    SELECT
      route_id,
      stop_id,
      json_group_array(trip_id ORDER BY trip_id, stop_sequence) AS trip_ids_json,
      GROUP_CONCAT(arrival_time, ', ' ORDER BY trip_id, stop_sequence) AS arrival_times
    FROM trip_stop_times
    GROUP BY route_id, stop_id
  )
SELECT
  'segment' AS row_kind,
  distinct_segments.route_id,
  NULL AS stop_id,
  NULL AS stop_name,
  from_stop.stop_lat AS lat,
  from_stop.stop_lon AS lng,
  to_stop.stop_lat AS to_lat,
  to_stop.stop_lon AS to_lng,
  NULL AS trip_ids_json,
  NULL AS arrival_times
FROM distinct_segments
JOIN stops AS from_stop ON from_stop.stop_id = distinct_segments.from_stop_id
JOIN stops AS to_stop ON to_stop.stop_id = distinct_segments.to_stop_id
UNION ALL
SELECT
  'stop' AS row_kind,
  route_stops.route_id,
  stops.stop_id,
  stops.stop_name,
  stops.stop_lat AS lat,
  stops.stop_lon AS lng,
  NULL AS to_lat,
  NULL AS to_lng,
  route_stops.trip_ids_json,
  route_stops.arrival_times
FROM route_stops
JOIN stops ON stops.stop_id = route_stops.stop_id
ORDER BY row_kind, route_id, stop_id, lat, lng, to_lat, to_lng
