-- ?1: stop_id, ?2: route_id or '', ?3: direction_id or -1,
-- ?4: import revision, ?5: expiry Unix timestamp, ?6: calculated results JSON.
WITH
  feed_state AS (__FEED_STATE__)
INSERT INTO
  departure_cache (stop_id, route_id, direction_id, feed_revision, expires_at, payload)
SELECT ?1, ?2, ?3, ?4, ?5, ?6
FROM
  feed_state
WHERE
  revision = ?4
  AND importing = 0 ON CONFLICT (stop_id, route_id, direction_id) DO
UPDATE
SET
  feed_revision = excluded.feed_revision,
  expires_at = excluded.expires_at,
  payload = excluded.payload
WHERE
  departure_cache.expires_at <= excluded.expires_at;
