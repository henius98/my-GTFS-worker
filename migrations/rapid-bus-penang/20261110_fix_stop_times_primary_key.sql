-- Migration: Change stop_times primary key to (stop_id, trip_id, stop_sequence).
-- Rebuild is handled by 20261111_purge_ghost_data.sql to avoid exceeding D1 500MB storage limit.
DROP INDEX IF EXISTS idx_stop_times_stop_departure;
