WITH
__ACTIVE_SERVICES__,
  trip_shapes AS (
    SELECT
      route_id,
      shape_id,
      json_group_array(DISTINCT trip_id) FILTER (WHERE trip_id IS NOT NULL) AS trip_ids_json
    FROM trips
    WHERE service_id IN(SELECT service_id FROM active_services)
      AND shape_id IS NOT NULL
      AND shape_id <> ''
    GROUP BY route_id, shape_id
  )
SELECT
  trip_shapes.route_id,
  trip_shapes.shape_id,
  CASE
    WHEN ROW_NUMBER() OVER (
      PARTITION BY trip_shapes.route_id, trip_shapes.shape_id
      ORDER BY shapes.shape_pt_sequence
    ) = 1 THEN trip_shapes.trip_ids_json
  END AS trip_ids_json,
  shapes.shape_pt_lat AS lat,
  shapes.shape_pt_lon AS lng
FROM trip_shapes
INNER JOIN shapes ON shapes.shape_id = trip_shapes.shape_id
ORDER BY trip_shapes.route_id, trip_shapes.shape_id, shapes.shape_pt_sequence
