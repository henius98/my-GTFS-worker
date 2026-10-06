-- Migration number: 20260831 	 2026-09-24T07:58:49.995Z
-- Calculated departures; empty route_id and direction_id -1 mean no filter.
CREATE TABLE IF NOT EXISTS departure_cache (
  stop_id TEXT NOT NULL,
  route_id TEXT NOT NULL DEFAULT '',
  direction_id INTEGER NOT NULL DEFAULT -1 CHECK (direction_id IN (-1, 0, 1)),
  feed_revision TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY (stop_id, route_id, direction_id)
);
