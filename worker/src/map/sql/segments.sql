WITH
  consecutive AS (
    SELECT
      trips.route_id,
      stop_times.stop_id AS to_stop_id,
      LAG(stop_times.stop_id) OVER (PARTITION BY trips.trip_id ORDER BY stop_times.stop_sequence) AS from_stop_id
    FROM stop_times
    JOIN trips ON trips.trip_id = stop_times.trip_id__TRIP_FILTER__
  ),
  distinct_segments AS (
    SELECT route_id, from_stop_id, to_stop_id
    FROM consecutive
    WHERE from_stop_id IS NOT NULL
      AND from_stop_id <> to_stop_id
    GROUP BY route_id, from_stop_id, to_stop_id
  )
SELECT
  distinct_segments.route_id,
  distinct_segments.from_stop_id,
  distinct_segments.to_stop_id,
  from_stop.stop_lat AS from_lat,
  from_stop.stop_lon AS from_lng,
  to_stop.stop_lat AS to_lat,
  to_stop.stop_lon AS to_lng
FROM distinct_segments
JOIN stops AS from_stop ON from_stop.stop_id = distinct_segments.from_stop_id
JOIN stops AS to_stop ON to_stop.stop_id = distinct_segments.to_stop_id
ORDER BY distinct_segments.route_id, distinct_segments.from_stop_id, distinct_segments.to_stop_id
