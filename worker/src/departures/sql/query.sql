-- ---------------------------------------------------------------------------
-- 4. THE "PROCEDURE": templates for the next N departures from a stop.
--
--    Bind ?1 = stop_id, ?2 = optional route_id, ?3 = optional direction_id.
--    Rust supplies the time cutoff in each agency's timezone and evaluates
--    calendar dates, including previous service days whose trains still run
--    after midnight. D1 supports numbered parameters rather than named ones.
--
--    Closed form instead of iterating: for a stop at offset `off`, the trip
--    that serves it must have started at or after (now - off). The k-th trip
--    in a frequency window starts at start_secs + k * headway, so
--        k = ceil((now - off - start_secs) / headway), floored at 0
--    and the window is exhausted once that start reaches end_secs.
--
--    k alone yields only the FIRST departure in each frequency window, which
--    is not the same as the next N trains. Rust expands k .. k+N-1 per active
--    service date, merges the results, and retains the earliest N estimates.
--    Trips without frequencies use their scheduled stop times once per active
--    date. Rust fills the marked SQL fragments from the database's columns;
--    calendar rows and exceptions are loaded separately by service_id.
-- ---------------------------------------------------------------------------
-- ?1: stop_id, ?2: route_id or NULL, ?3: direction_id or NULL
SELECT
  t.trip_id,
  t.route_id,
  t.service_id,
  __HEADSIGN__ AS trip_headsign,
  t.direction_id,
  st.stop_sequence,
  st.arrival_time,
  st.departure_time,
  __FREQUENCY_COLUMNS__,
  a.agency_timezone
FROM stop_times st
JOIN trips t ON t.trip_id = st.trip_id __FREQUENCY_JOIN__
LEFT JOIN routes r ON r.route_id = t.route_id __AGENCY_JOIN__
WHERE st.stop_id = ? 1 __PICKUP_FILTER__
AND (? 2 IS NULL OR t.route_id = ? 2)
AND (? 3 IS NULL OR t.direction_id = ? 3);
