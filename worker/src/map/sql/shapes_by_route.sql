SELECT
  shape_id,
  shape_pt_lat AS lat,
  shape_pt_lon AS lng
FROM shapes
WHERE shape_id IN (SELECT value FROM json_each(?1))
ORDER BY shape_id, shape_pt_sequence
