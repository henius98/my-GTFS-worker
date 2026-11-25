WITH
  trip_stop_times AS MATERIALIZED (
    SELECT
      json_extract(input.value, '$[0]') AS route_id,
      stop_times.stop_id,
      stop_times.arrival_time,
      stop_times.departure_time,
      LAG(stop_times.stop_id) OVER (PARTITION BY json_extract(input.value, '$[0]'), stop_times.trip_id ORDER BY stop_times.stop_sequence) AS prev_stop_id
    FROM json_each(?1) AS input
    JOIN stop_times ON stop_times.trip_id = json_extract(input.value, '$[1]')
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
      GROUP_CONCAT(arrival_time, ', ') AS arrival_times,
      GROUP_CONCAT(departure_time, ', ') AS departure_times
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
  NULL AS arrival_times,
  NULL AS departure_times
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
  route_stops.arrival_times,
  route_stops.departure_times
FROM route_stops
JOIN stops ON stops.stop_id = route_stops.stop_id
ORDER BY row_kind, route_id, stop_id, lat, lng, to_lat, to_lng
