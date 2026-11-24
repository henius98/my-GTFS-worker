#![allow(clippy::unwrap_used)]

use super::*;

const TEST_MAX_LIMIT: usize = 100;

fn cached_entry(count: usize) -> Entry {
  let from: DateTime<Utc> = "2026-09-24T00:00:00Z".parse().unwrap();
  let departures = (0..count)
    .map(|index| {
      let at = from + Duration::seconds((index as i64 + 1) * 10);
      serde_json::from_value(serde_json::json!({
        "trip_id": format!("trip-{index}"), "route_id": "R", "service_id": "daily",
        "trip_headsign": null, "direction_id": 0, "stop_sequence": 2,
        "service_date": "2026-09-24", "agency_timezone": "Asia/Kuala_Lumpur",
        "headway_secs": null, "trip_start_secs": null, "estimate_method": "gtfs_schedule",
        "departure_secs": 28810 + index * 10, "service_departure_time": "08:00:10",
        "estimated_arrival_at": at, "estimated_departure_at": at, "wait_seconds": (index + 1) * 10
      }))
      .unwrap()
    })
    .collect();
  Entry { stop: Stop { stop_id: "S".into(), stop_name: None }, from, until: from + Duration::days(7) + Duration::days(1), departures }
}

#[test]
fn cache_ttl_parses_positive_values_and_rejects_invalid_values() {
  assert_eq!(crate::parse_int_in_range("86400", 1i64, crate::MAX_TTL_SECONDS), Some(24 * 60 * 60));
  assert_eq!(crate::parse_int_in_range(" 3600 ", 1i64, crate::MAX_TTL_SECONDS), Some(3600));
  for value in ["", "0", "-1", "one hour", "9223372036854776"] {
    assert_eq!(crate::parse_int_in_range(value, 1i64, crate::MAX_TTL_SECONDS), None);
  }
}

#[test]
fn filters_elapsed_departures_and_recalculates_fractional_waits() {
  let entry = cached_entry(3);
  let when = entry.from + Duration::seconds(10) + Duration::milliseconds(1);
  let rows = entry.select(when, when + Duration::days(7), 2, TEST_MAX_LIMIT).unwrap();
  assert_eq!(rows.len(), 2);
  assert_eq!(rows[0].wait_seconds, 10);
  assert_eq!(rows[1].wait_seconds, 20);
  assert_eq!(rows[0].estimated_departure_at, entry.from + Duration::seconds(20));
}

#[test]
fn refills_a_truncated_cache_instead_of_returning_too_few_results() {
  let entry = cached_entry(TEST_MAX_LIMIT);
  let when = entry.from + Duration::seconds(5);
  assert_eq!(entry.select(when, when + Duration::days(7), TEST_MAX_LIMIT, TEST_MAX_LIMIT).unwrap().len(), TEST_MAX_LIMIT);
  let when = entry.from + Duration::seconds(10) + Duration::milliseconds(1);
  // A departed vehicle leaves fewer than the requested number of cached results.
  assert!(entry.select(when, when + Duration::days(7), TEST_MAX_LIMIT, TEST_MAX_LIMIT).is_none());
  let until = entry.from + Duration::days(7);
  assert!(entry.select(when, until, TEST_MAX_LIMIT, TEST_MAX_LIMIT).is_none());
  assert_eq!(entry.select(when, until, 1, TEST_MAX_LIMIT).unwrap().len(), 1);
}

#[test]
fn sparse_results_cover_the_moving_horizon_and_exclude_its_end() {
  let entry = cached_entry(1);
  assert!(entry.select(entry.from, entry.from + Duration::seconds(10), 5, TEST_MAX_LIMIT).unwrap().is_empty());
  let when = entry.from + Duration::seconds(10);
  assert_eq!(entry.select(when, when + Duration::days(7), 5, TEST_MAX_LIMIT).unwrap().len(), 1);
  assert!(entry.select(entry.from - Duration::seconds(1), entry.until, 5, TEST_MAX_LIMIT).is_none());
  assert!(entry.select(when, entry.until + Duration::seconds(1), 5, TEST_MAX_LIMIT).is_none());
  let empty = cached_entry(0);
  assert!(empty.select(empty.from + Duration::seconds(30), empty.from + Duration::days(7) + Duration::seconds(30), 5, TEST_MAX_LIMIT).unwrap().is_empty());
}

#[test]
fn route_metadata_survives_cache_deserialization() {
  let mut entry = cached_entry(1);
  let departure = &mut entry.departures[0];
  departure.route_short_name = Some("KJL".into());
  departure.route_long_name = Some("Kelana Jaya".into());
  departure.route_color = Some("E51B24".into());
  departure.route_text_color = Some("FFFFFF".into());
  departure.trip_headsign = Some("Gombak".into());
  let json = serde_json::to_string(&entry).unwrap();
  let restored: Entry = serde_json::from_str(&json).unwrap();
  assert_eq!(restored.departures[0].route_short_name.as_deref(), Some("KJL"));
  assert_eq!(restored.departures[0].route_color.as_deref(), Some("E51B24"));
  assert_eq!(restored.departures[0].trip_headsign.as_deref(), Some("Gombak"));
}
