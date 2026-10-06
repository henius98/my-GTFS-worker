use super::*;

fn dt(s: &str) -> DateTime<Utc> {
  DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn next_departures(templates: &[Template], when: DateTime<Utc>, limit: usize) -> Result<Vec<Departure>, &'static str> {
  super::next_departures(templates, when, when + Duration::days(SEARCH_DAYS), limit)
}

fn template() -> Template {
  let value = serde_json::json!({
      "trip_id": "opaque-trip", "route_id": "KJL", "service_id": "weekday-id",
      "trip_headsign": "Gombak", "direction_id": 0, "stop_sequence": 10,
      "arrival_time": "6:09:42", "departure_time": "6:10:00", "first_departure_time": "6:00:00",
      "start_time": "6:00:00", "end_time": "7:00:00", "headway_secs": 600, "frequency_trip_id": "opaque-trip",
      "agency_timezone": "Asia/Kuala_Lumpur", "monday": 1, "tuesday": 1,
      "wednesday": 1, "thursday": 1, "friday": 1, "saturday": 1, "sunday": 1,
      "start_date": 20260101, "end_date": 20261231
  });
  let mut template: Template = serde_json::from_value(value.clone()).unwrap();
  template.service.calendars.push(serde_json::from_value(value).unwrap());
  template
}

#[test]
fn weekday_uses_calendar_flags() {
  // 2026-08-27 is a Thursday.
  let mut t = template();
  t.service.calendars[0].thursday = 0;
  let rows = next_departures(&[t], dt("2026-08-27T06:00:00+08:00"), 1).unwrap();
  assert_eq!(rows[0].service_date.to_string(), "2026-08-28");
  assert_eq!(rows[0].trip_id, "opaque-trip");
}

#[test]
fn before_rollover_includes_both_service_days() {
  // 00:40 Friday can still be Thursday's service day, at t=24:40.
  let mut yesterday = template();
  yesterday.start_time = Some("24:30:00".into());
  yesterday.end_time = Some("24:50:00".into());
  let mut today = template();
  today.trip_id = "early-service".into();
  today.start_time = Some("0:35:00".into());
  today.end_time = Some("0:45:00".into());
  let rows = next_departures(&[yesterday, today], dt("2026-08-28T00:40:00+08:00"), 3).unwrap();
  assert_eq!(rows.iter().map(|d| d.wait_seconds).collect::<Vec<_>>(), [0, 300, 600]);
  assert_eq!(rows[0].service_date.to_string(), "2026-08-27");
  assert_eq!(rows[1].service_date.to_string(), "2026-08-28");
}

#[test]
fn rollover_advances_from_each_service_date() {
  // 01:15 Sunday can include Saturday's service day; the next service
  // day must be Sunday, not Monday.
  let timezone = chrono_tz::Asia::Kuala_Lumpur;
  let ctx = ServiceContext::resolve(NaiveDate::from_ymd_opt(2026, 8, 29).unwrap(), timezone).unwrap();
  assert_eq!(ctx.next_day(timezone).unwrap().date.to_string(), "2026-08-30");
  let rows = next_departures(&[template()], dt("2026-08-30T01:15:00+08:00"), 1).unwrap();
  assert_eq!(rows[0].service_date.to_string(), "2026-08-30");
  assert_eq!(rows[0].wait_seconds, 4 * 3600 + 55 * 60);
}

#[test]
fn times_validate_and_retain_past_midnight_hours() {
  assert_eq!(parse_time("  6:00:00  ").unwrap(), 21600);
  assert_eq!(parse_time("49:13:52").unwrap(), 49 * 3600 + 13 * 60 + 52);
  assert_eq!(fmt_secs(24 * 3600 + 13 * 60 + 52), "24:13:52");
  assert_eq!(fmt_secs(23 * 3600 + 59 * 60), "23:59:00");
  for time in ["", "garbage", "-1:00:00", "6:0:00", "6:60:00", "6:00:60", "6:00:00x", "6:00:00:00"] {
    assert!(parse_time(time).is_err(), "{time}");
  }
}

#[test]
fn expands_next_runs_and_respects_exclusive_end() {
  let t = template();
  let rows = next_departures(&[t], dt("2026-08-27T06:45:00+08:00"), 4).unwrap();
  assert_eq!(rows.iter().map(|d| d.service_departure_time.as_str()).collect::<Vec<_>>(), ["06:50:00", "07:00:00", "06:10:00", "06:20:00"]);
  assert_eq!(rows[2].service_date.to_string(), "2026-08-28");
  assert_eq!(rows[2].wait_seconds, 23 * 3600 + 25 * 60);
}

#[test]
fn calendar_ranges_are_inclusive_and_duplicates_do_not_repeat_trains() {
  let mut t = template();
  t.service.calendars[0].start_date = 20260827;
  t.service.calendars[0].end_date = 20260827;
  let rows = next_departures(&[t.clone(), t.clone()], dt("2026-08-27T06:00:00+08:00"), 20).unwrap();
  assert_eq!(rows.len(), 6);
  assert!(next_departures(&[t.clone()], dt("2026-08-28T06:00:00+08:00"), 5).unwrap().is_empty());
  assert!(next_departures(&[t], dt("2026-08-19T06:00:00+08:00"), 5).unwrap().is_empty());
}

#[test]
fn seven_day_horizon_is_exclusive_and_seconds_are_not_rounded_down() {
  let mut t = template();
  t.service.calendars[0].start_date = 20260903;
  t.service.calendars[0].end_date = 20260903;
  assert!(next_departures(&[t.clone()], dt("2026-08-27T06:10:00+08:00"), 5).unwrap().is_empty());
  assert_eq!(next_departures(&[t], dt("2026-08-27T06:10:00.001+08:00"), 5).unwrap().len(), 1);
  let rows = next_departures(&[template()], dt("2026-08-27T06:10:00.001+08:00"), 1).unwrap();
  assert_eq!(rows[0].service_departure_time, "06:20:00");
  assert_eq!(rows[0].wait_seconds, 600);
}

#[test]
fn multi_day_offsets_and_negative_first_arrivals() {
  let mut t = template();
  t.start_time = Some("49:00:00".into());
  t.end_time = Some("49:10:00".into());
  t.arrival_time = "5:59:42".into();
  t.departure_time = "6:00:00".into();
  let rows = next_departures(&[t], dt("2026-08-29T01:00:00+08:00"), 1).unwrap();
  assert_eq!(rows[0].service_date.to_string(), "2026-08-27");
  assert_eq!(rows[0].estimated_arrival_at, dt("2026-08-29T00:59:42+08:00"));
  assert_eq!(rows[0].wait_seconds, 0);
}

#[test]
fn rejects_invalid_feed_instead_of_inventing_departures() {
  let now = dt("2026-08-27T06:00:00+08:00");
  let mut variants = Vec::new();
  let mut t = template();
  t.headway_secs = Some(0);
  variants.push(t);
  let mut t = template();
  t.end_time = t.start_time.clone();
  variants.push(t);
  let mut t = template();
  t.start_time = Some("nonsense".into());
  variants.push(t);
  let mut t = template();
  t.agency_timezone = None;
  variants.push(t);
  let mut t = template();
  t.service.calendars[0].end_date = 20260230;
  variants.push(t);
  let mut t = template();
  t.service.calendars[0].end_date = 20250101;
  variants.push(t);
  let mut t = template();
  t.service.calendars[0].thursday = 2;
  variants.push(t);
  for t in variants {
    assert!(next_departures(&[t], now, 5).is_err());
  }
}

#[test]
fn dst_uses_noon_minus_twelve_hours() {
  let ctx = ServiceContext::resolve(NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(), chrono_tz::America::New_York).unwrap();
  assert_eq!(ctx.origin.with_timezone(&Utc), dt("2026-03-08T04:00:00Z"));
}

#[test]
fn closed_form_matches_independent_brute_force() {
  let mut seed = 17u64;
  let mut random = || {
    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    seed >> 32
  };
  for _ in 0..120 {
    let start = (random() % (50 * 3600)) as u32;
    let headway = (random() % 900 + 30) as u32;
    let end = start + (random() % 3600 + 1) as u32;
    let offset = (random() % 7200) as u32;
    let mut t = template();
    t.start_time = Some(fmt_secs(start));
    t.end_time = Some(fmt_secs(end));
    t.headway_secs = Some(headway);
    t.departure_time = fmt_secs(21600 + offset);
    t.arrival_time = t.departure_time.clone();
    let when = dt("2026-08-27T00:00:00+08:00") + Duration::seconds((random() % 172800) as i64);
    let limit = (random() % 20 + 1) as usize;
    let actual = next_departures(&[t], when, limit).unwrap();
    let mut expected = Vec::new();
    for day in -3..10 {
      let base = dt("2026-08-27T00:00:00+08:00") + Duration::days(day);
      let mut trip_start = start;
      while trip_start < end {
        let departure = base + Duration::seconds(i64::from(trip_start + offset));
        if departure >= when && departure < when + Duration::days(7) {
          expected.push(departure);
        }
        trip_start += headway;
      }
    }
    expected.sort();
    expected.truncate(limit);
    assert_eq!(actual.iter().map(|d| d.estimated_departure_at.with_timezone(&Utc)).collect::<Vec<_>>(), expected);
  }
}

fn scheduled_template() -> Template {
  let mut t = template();
  t.frequency_trip_id = None;
  t.start_time = None;
  t.end_time = None;
  t.headway_secs = None;
  t.first_departure_time = None;
  t
}

#[test]
fn scheduled_trips_use_stop_times_once_per_service_day() {
  let mut t = scheduled_template();
  t.trip_headsign = None;
  t.arrival_time = "24:09:42".into();
  t.departure_time = "24:10:00".into();
  let rows = next_departures(&[t], dt("2026-08-28T00:05:00+08:00"), 2).unwrap();
  assert_eq!(rows[0].service_date.to_string(), "2026-08-27");
  assert_eq!(rows[0].estimated_departure_at, dt("2026-08-28T00:10:00+08:00"));
  assert_eq!(rows[0].wait_seconds, 300);
  assert_eq!(rows[0].estimate_method, "gtfs_schedule");
  assert_eq!(rows[0].headway_secs, None);
  assert_eq!(rows[0].trip_start_secs, None);
  assert_eq!(rows[1].estimated_departure_at, dt("2026-08-29T00:10:00+08:00"));
}

#[test]
fn calendar_dates_cancel_and_add_service_outside_calendar_ranges() {
  let mut t = scheduled_template();
  t.service.calendars[0].end_date = 20260827;
  t.service.exceptions = vec![Exception { service_id: t.service_id.clone(), date: 20260827, exception_type: 2 }, Exception { service_id: t.service_id.clone(), date: 20260829, exception_type: 1 }];
  let rows = next_departures(&[t.clone()], dt("2026-08-27T06:00:00+08:00"), 5).unwrap();
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0].service_date.to_string(), "2026-08-29");
  t.service.calendars.clear();
  let rows = next_departures(&[t], dt("2026-08-27T06:00:00+08:00"), 5).unwrap();
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0].service_date.to_string(), "2026-08-29");
}

#[test]
fn mixes_scheduled_and_frequency_trips_in_time_order() {
  let mut scheduled = scheduled_template();
  scheduled.trip_id = "scheduled".into();
  scheduled.arrival_time = "06:14:42".into();
  scheduled.departure_time = "06:15:00".into();
  let rows = next_departures(&[template(), scheduled], dt("2026-08-27T06:00:00+08:00"), 3).unwrap();
  assert_eq!(rows.iter().map(|row| row.service_departure_time.as_str()).collect::<Vec<_>>(), ["06:10:00", "06:15:00", "06:20:00"]);
  assert_eq!(rows[1].estimate_method, "gtfs_schedule");
}

#[test]
fn rejects_overflowing_times_and_malformed_frequency_rows() {
  assert!(parse_time("4294967295:00:00").is_err());
  let mut t = scheduled_template();
  t.frequency_trip_id = Some(t.trip_id.clone());
  assert!(next_departures(&[t], dt("2026-08-27T06:00:00+08:00"), 1).is_err());
  let mut t = template();
  t.departure_time = "05:00:00".into();
  assert!(next_departures(&[t], dt("2026-08-27T06:00:00+08:00"), 1).is_err());
}
