WITH
__ACTIVE_SERVICES__
SELECT
  routes.route_id,
  __ROUTE_COLUMNS__,
  json_group_array(DISTINCT trips.trip_id) FILTER (WHERE trips.trip_id IS NOT NULL) AS trip_ids_json,
  __SHAPE_IDS_EXPR__
FROM routes
INNER JOIN trips
  ON routes.route_id IN (SELECT value FROM json_each(?1))
  AND trips.route_id = routes.route_id
  AND trips.service_id IN (SELECT service_id FROM active_services)
GROUP BY routes.route_id
ORDER BY routes.route_id
