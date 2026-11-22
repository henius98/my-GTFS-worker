-- Schedule-only departures query (providers without a frequencies table).
-- Dynamic template placeholders substituted by Rust:
--   ROUTE_COLUMNS - Selected route metadata columns (or NULL aliases).
--   HEADSIGN      - Coalesced headsign expression.
--   AGENCY_JOIN   - Primary-key agency join (or single-row agency fallback).
--   PICKUP_FILTER - Optional pickup_type filter.
--
-- Performance note:
-- Relies on WITHOUT ROWID primary keys:
--   stop_times(stop_id, trip_id, stop_sequence) -> prefix lookup on stop_id = ?1
--   trips(trip_id), routes(route_id), agency(agency_id) -> PK point lookups.
--
-- Parameters:
--   ?1: stop_id, ?2: route_id or NULL, ?3: direction_id or NULL
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
  NULL AS first_departure_time,
  NULL AS start_time,
  NULL AS end_time,
  NULL AS headway_secs,
  NULL AS frequency_trip_id,
  NULL AS exact_times,
  a.agency_timezone
FROM
  stop_times st
  JOIN trips t ON t.trip_id = st.trip_id
  LEFT JOIN routes r ON r.route_id = t.route_id
  __AGENCY_JOIN__
WHERE
  st.stop_id = ?1 __PICKUP_FILTER__
  AND (?2 IS NULL OR t.route_id = ?2)
  AND (?3 IS NULL OR t.direction_id = ?3);
