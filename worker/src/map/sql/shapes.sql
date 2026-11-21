SELECT trip_shapes.route_id, trip_shapes.shape_id, shapes.shape_pt_lat AS lat, shapes.shape_pt_lon AS lng
FROM (SELECT DISTINCT route_id, shape_id FROM trips WHERE shape_id IS NOT NULL__SHAPE_FILTER__) AS trip_shapes
JOIN shapes ON shapes.shape_id = trip_shapes.shape_id
ORDER BY trip_shapes.route_id, trip_shapes.shape_id, shapes.shape_pt_sequence
