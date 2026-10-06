-- Unified departures query. Supports:
-- 1. Standard schedule lookups (stop_times joined to trips and routes).
-- 2. Frequency-based lookups (joined to frequencies).
-- 3. Pickup type filtering (drop off only vs pickup).
-- Placeholders replaced by Rust at compile/runtime:
--   __HEADSIGN__          - Coalesce logic or column for headsign.
--   __FREQUENCY_COLUMNS__ - Extra columns for frequencies (or NULLs).
--   __FREQUENCY_JOIN__    - Optional JOIN frequencies.
--   __PICKUP_FILTER__     - Optional AND st.pickup_type != 1.
--   __AGENCY_JOIN__       - Optional JOIN agency.
--
-- Note on timezone fallback:
-- SQLite returns agency_timezone as NULL when no agency matches or column is absent;
-- caller falls back to "Asia/Kuala_Lumpur".
--
-- Performance note:
-- Query plan relies on stop_times(stop_id, departure_time) index for range scan.
-- Filters on route_id/direction_id apply during or after index lookup.
--
-- Parameters:
--    Bind ?1 = stop_id, ?2 = optional route_id, ?3 = optional direction_id.
--
-- Fields:
-- ?1: stop_id, ?2: route_id or NULL, ?3: direction_id or NULL
SELECT
  st.trip_id,
  t.route_id,
  t.service_id,
  __HEADSIGN__ AS trip_headsign,
  t.direction_id,
  st.stop_sequence,
  st.arrival_time,
  st.departure_time,
  __FREQUENCY_COLUMNS__,
  a.agency_timezone
FROM
  stop_times st
  JOIN trips t ON t.trip_id = st.trip_id __FREQUENCY_JOIN__
  LEFT JOIN routes r ON r.route_id = t.route_id __AGENCY_JOIN__
WHERE
  st.stop_id = ?1 __PICKUP_FILTER__
  AND (?2 IS NULL OR t.route_id = ?2)
  AND (?3 IS NULL OR t.direction_id = ?3);
