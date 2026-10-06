use super::*;

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
fn cache_ttl_defaults_to_24_hours_and_rejects_invalid_values() {
  assert_eq!(parse_ttl_seconds(None), 24 * 60 * 60);
  assert_eq!(parse_ttl_seconds(Some("86400")), 24 * 60 * 60);
  assert_eq!(parse_ttl_seconds(Some(" 3600 ")), 3600);
  for value in ["", "0", "-1", "one hour", "9223372036854776"] {
    assert_eq!(parse_ttl_seconds(Some(value)), 24 * 60 * 60);
  }
}

#[test]
fn filters_elapsed_departures_and_recalculates_fractional_waits() {
  let entry = cached_entry(3);
  let when = entry.from + Duration::seconds(10) + Duration::milliseconds(1);
  let rows = entry.select(when, when + Duration::days(7), 2).unwrap();
  assert_eq!(rows.len(), 2);
  assert_eq!(rows[0].wait_seconds, 10);
  assert_eq!(rows[1].wait_seconds, 20);
  assert_eq!(rows[0].estimated_departure_at, entry.from + Duration::seconds(20));
}

#[test]
fn refills_a_truncated_cache_instead_of_returning_too_few_results() {
  let entry = cached_entry(MAX_LIMIT);
  let when = entry.from + Duration::seconds(5);
  assert_eq!(entry.select(when, when + Duration::days(7), MAX_LIMIT).unwrap().len(), MAX_LIMIT);
  let when = entry.from + Duration::seconds(10) + Duration::milliseconds(1);
  // A departed vehicle leaves fewer than the requested number of cached results.
  assert!(entry.select(when, when + Duration::days(7), MAX_LIMIT).is_none());
  let until = entry.from + Duration::days(7);
  assert!(entry.select(when, until, MAX_LIMIT).is_none());
  assert_eq!(entry.select(when, until, 1).unwrap().len(), 1);
}

#[test]
fn sparse_results_cover_the_moving_horizon_and_exclude_its_end() {
  let entry = cached_entry(1);
  assert!(entry.select(entry.from, entry.from + Duration::seconds(10), 5).unwrap().is_empty());
  let when = entry.from + Duration::seconds(10);
  assert_eq!(entry.select(when, when + Duration::days(7), 5).unwrap().len(), 1);
  assert!(entry.select(entry.from - Duration::seconds(1), entry.until, 5).is_none());
  assert!(entry.select(when, entry.until + Duration::seconds(1), 5).is_none());
  let empty = cached_entry(0);
  assert!(empty.select(empty.from + Duration::seconds(30), empty.from + Duration::days(7) + Duration::seconds(30), 5).unwrap().is_empty());
}
