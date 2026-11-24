WITH
__ACTIVE_SERVICES__,
  trip_shapes AS (
    SELECT DISTINCT route_id, shape_id
    FROM trips
    WHERE service_id IN(SELECT service_id FROM active_services)
      AND shape_id IS NOT NULL
      AND shape_id <> ''
  )
SELECT
  trip_shapes.route_id,
  trip_shapes.shape_id,
  shapes.shape_pt_lat AS lat,
  shapes.shape_pt_lon AS lng
FROM trip_shapes
INNER JOIN shapes ON shapes.shape_id = trip_shapes.shape_id
ORDER BY trip_shapes.route_id, trip_shapes.shape_id, shapes.shape_pt_sequence
