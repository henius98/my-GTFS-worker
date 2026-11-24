-- ?1: stop_id, ?2: route_id or '', ?3: direction_id or -1,
-- ?4: current Unix timestamp.
WITH
  feed_state AS (__FEED_STATE__)
SELECT
  feed_state.revision,
  dc.payload
FROM
  feed_state
  LEFT JOIN departure_cache dc
    ON dc.stop_id = ?1
    AND dc.route_id = ?2
    AND dc.direction_id = ?3
    AND dc.feed_revision = feed_state.revision
    AND dc.expires_at > ?4;
