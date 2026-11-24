-- Frequency-capable departures query (providers with a frequencies table).
-- Dynamic template placeholders substituted by Rust:
--   ROUTE_COLUMNS - Selected route metadata columns (or NULL aliases).
--   HEADSIGN      - Coalesced headsign expression.
--   EXACT_TIMES   - f.exact_times or NULL.
--   AGENCY_JOIN   - Primary-key agency join (or single-row agency fallback).
--   PICKUP_FILTER - Optional pickup_type filter.
--
-- Performance note:
-- stop_times has PRIMARY KEY (trip_id, stop_id, stop_sequence) WITHOUT ROWID
-- and no secondary stop_id index. Computing first_stops via a gated CTE avoids
-- N correlated lookups (0 lookups when no frequency trips match, PK prefix
-- lookups on trip_id otherwise).
--
-- Parameters:
--   ?1: stop_id, ?2: route_id or NULL, ?3: direction_id or NULL
WITH
  matched AS MATERIALIZED (
    SELECT
      st.trip_id,
      t.route_id,
      __ROUTE_COLUMNS__,
      t.service_id,
      __HEADSIGN__ AS trip_headsign,
      t.direction_id,
      st.stop_sequence,
      st.arrival_time,
      st.departure_time,
      f.start_time,
      f.end_time,
      f.headway_secs,
      f.trip_id AS frequency_trip_id,
      __EXACT_TIMES__ AS exact_times,
      a.agency_timezone
    FROM
      stop_times st
      JOIN trips t ON t.trip_id = st.trip_id
      LEFT JOIN frequencies f ON f.trip_id = t.trip_id
      LEFT JOIN routes r ON r.route_id = t.route_id
      __AGENCY_JOIN__
    WHERE
      st.stop_id = ?1 __PICKUP_FILTER__
      AND (?2 IS NULL OR t.route_id = ?2)
      AND (?3 IS NULL OR t.direction_id = ?3)
  ),
  freq_trips AS MATERIALIZED (
    SELECT DISTINCT trip_id
    FROM matched
    WHERE frequency_trip_id IS NOT NULL
  ),
  first_stops AS (
    SELECT trip_id, departure_time AS first_departure_time
    FROM (
      SELECT stop_times.trip_id, stop_times.departure_time, MIN(stop_times.stop_sequence)
      FROM (SELECT 1 FROM freq_trips LIMIT 1) AS has_freq
      CROSS JOIN stop_times
      WHERE stop_times.trip_id IN (SELECT trip_id FROM freq_trips)
      GROUP BY stop_times.trip_id
    )
  )
SELECT
  matched.trip_id,
  matched.route_id,
  matched.route_short_name,
  matched.route_long_name,
  matched.route_color,
  matched.route_text_color,
  matched.service_id,
  matched.trip_headsign,
  matched.direction_id,
  matched.stop_sequence,
  matched.arrival_time,
  matched.departure_time,
  first_stops.first_departure_time,
  matched.start_time,
  matched.end_time,
  matched.headway_secs,
  matched.frequency_trip_id,
  matched.exact_times,
  matched.agency_timezone
FROM
  matched
  LEFT JOIN first_stops ON first_stops.trip_id = matched.trip_id;
