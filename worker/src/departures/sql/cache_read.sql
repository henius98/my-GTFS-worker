-- ?1: stop_id, ?2: route_id or '', ?3: direction_id or -1,
-- ?4: import revision, ?5: current Unix timestamp.
SELECT payload
FROM
  departure_cache
WHERE
  stop_id = ? 1
  AND route_id = ? 2
  AND direction_id = ? 3
  AND feed_revision = ? 4
  AND expires_at > ? 5;
