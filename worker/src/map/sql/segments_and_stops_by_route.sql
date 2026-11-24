WITH
  trip_stop_times AS MATERIALIZED (
    SELECT
      stop_times.stop_id,
      stop_times.arrival_time,
      stop_times.departure_time,
      LAG(stop_times.stop_id) OVER (PARTITION BY stop_times.trip_id ORDER BY stop_times.stop_sequence) AS prev_stop_id
    FROM stop_times
    WHERE stop_times.trip_id IN (SELECT value FROM json_each(?1))
  ),
  distinct_segments AS (
    SELECT
      MIN(prev_stop_id, stop_id) AS from_stop_id,
      MAX(prev_stop_id, stop_id) AS to_stop_id
    FROM trip_stop_times
    WHERE prev_stop_id IS NOT NULL
      AND prev_stop_id <> stop_id
    GROUP BY MIN(prev_stop_id, stop_id), MAX(prev_stop_id, stop_id)
  ),
  route_stops AS (
    SELECT
      stop_id,
      GROUP_CONCAT(arrival_time, ', ') AS arrival_times,
      GROUP_CONCAT(departure_time, ', ') AS departure_times
    FROM trip_stop_times
    GROUP BY stop_id
  )
SELECT
  'segment' AS row_kind,
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
ORDER BY row_kind, stop_id, lat, lng, to_lat, to_lng
