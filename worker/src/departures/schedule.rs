use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// GTFS service days may run past 24:00:00. Search all dates whose frequency
/// windows reach the request, instead of assigning a fixed rollover hour.
pub(crate) const SEARCH_DAYS: i64 = 7;

/// Service days come from calendar, not from a trip_id such as KJL_MonFri_0.
///
/// Providers with calendar_dates apply those overrides to the base calendar,
/// including services defined entirely by added dates.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Calendar {
  pub(super) service_id: String,
  monday: u8,
  tuesday: u8,
  wednesday: u8,
  thursday: u8,
  friday: u8,
  saturday: u8,
  sunday: u8,
  start_date: i32,
  end_date: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Exception {
  pub service_id: String,
  date: i32,
  exception_type: u8,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Service {
  pub calendars: Vec<Calendar>,
  pub exceptions: Vec<Exception>,
}

impl Calendar {
  fn flags(&self) -> [u8; 7] {
    [self.monday, self.tuesday, self.wednesday, self.thursday, self.friday, self.saturday, self.sunday]
  }

  fn active(&self, date: NaiveDate) -> bool {
    self.flags()[date.weekday().num_days_from_monday() as usize] == 1
  }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Template {
  trip_id: String,
  route_id: String,
  pub(super) service_id: String,
  trip_headsign: Option<String>,
  direction_id: Option<u8>,
  stop_sequence: u32,
  arrival_time: String,
  departure_time: String,
  first_departure_time: Option<String>,
  start_time: Option<String>,
  end_time: Option<String>,
  headway_secs: Option<u32>,
  frequency_trip_id: Option<String>,
  exact_times: Option<u8>,
  agency_timezone: Option<String>,
  #[serde(skip)]
  pub(super) service: Service,
}

/// A service date and its origin in the agency timezone. GTFS defines the
/// origin as local noon minus twelve elapsed hours, including on DST days.
#[derive(Debug, Clone, Copy)]
struct ServiceContext {
  date: NaiveDate,
  origin: DateTime<Tz>,
}

impl ServiceContext {
  fn resolve(date: NaiveDate, timezone: Tz) -> Result<Self, &'static str> {
    let noon_time = date.and_hms_opt(12, 0, 0).ok_or("Invalid agency service date")?;
    let noon = timezone.from_local_datetime(&noon_time).single().ok_or("Invalid agency service date")?;
    Ok(Self { date, origin: noon - Duration::hours(12) })
  }

  /// Start of the following service date, resolved again in the agency zone.
  fn next_day(self, timezone: Tz) -> Result<Self, &'static str> {
    Self::resolve(self.date.succ_opt().ok_or("Service date overflow")?, timezone)
  }
}

/// Format seconds into a service day, retaining hours past midnight.
fn fmt_secs(secs: u32) -> String {
  format!("{:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn parse_time(value: &str) -> Result<u32, &'static str> {
  let parts: Vec<_> = value.trim().split(':').collect();
  if parts.len() != 3 || parts[0].is_empty() || parts[1].len() != 2 || parts[2].len() != 2 || parts.iter().any(|part| !part.bytes().all(|b| b.is_ascii_digit())) {
    return Err("Malformed GTFS time");
  }
  let hours = parts[0].parse::<u32>().map_err(|_| "GTFS hours out of range")?;
  let minutes = parts[1].parse::<u8>().map_err(|_| "Invalid GTFS minutes")?;
  let seconds = parts[2].parse::<u8>().map_err(|_| "Invalid GTFS seconds")?;
  if minutes >= 60 || seconds >= 60 {
    return Err("GTFS minutes and seconds must be below 60");
  }
  hours.checked_mul(3600).and_then(|value| value.checked_add(u32::from(minutes) * 60 + u32::from(seconds))).ok_or("GTFS hours out of range")
}

fn parse_date(value: i32) -> Result<NaiveDate, &'static str> {
  let text = value.to_string();
  if text.len() != 8 {
    return Err("Calendar date must be YYYYMMDD");
  }
  NaiveDate::parse_from_str(&text, "%Y%m%d").map_err(|_| "Invalid calendar date")
}

// ---------------------------------------------------------------------------

/// Departures carry their actual service dates, which may precede the request
/// date after midnight. Waits always use the original requested instant.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Departure {
  trip_id: String,
  route_id: String,
  service_id: String,
  trip_headsign: Option<String>,
  direction_id: Option<u8>,
  stop_sequence: u32,
  service_date: NaiveDate,
  agency_timezone: String,
  headway_secs: Option<u32>,
  trip_start_secs: Option<u32>,
  pub(super) estimate_method: String,
  /// Seconds since the service-day origin; can exceed 86400 after midnight.
  departure_secs: u32,
  service_departure_time: String,
  estimated_arrival_at: DateTime<FixedOffset>,
  pub(super) estimated_departure_at: DateTime<FixedOffset>,
  pub(super) wait_seconds: u32,
}

/// Next `limit` estimated departures before the exclusive search horizon.
///
/// If today's service is exhausted or has too few results, include the next
/// active dates and merge them with previous-day trains still running.
/// Every candidate is checked against the original requested instant.
pub(crate) fn next_departures(templates: &[Template], when: DateTime<Utc>, until: DateTime<Utc>, limit: usize) -> Result<Vec<Departure>, &'static str> {
  let cutoff = when.timestamp() + i64::from(when.timestamp_subsec_nanos() > 0);
  let mut departures = Vec::new();
  for t in templates {
    let timezone_name = match t.agency_timezone.as_deref().map(str::trim) {
      Some(tz) if !tz.is_empty() => tz,
      _ => "Asia/Kuala_Lumpur",
    };
    let timezone: Tz = timezone_name.parse().map_err(|_| "Unknown agency timezone")?;
    let arrival_time = i64::from(parse_time(&t.arrival_time)?);
    let departure_time = i64::from(parse_time(&t.departure_time)?);
    // A scheduled stop has one run on each active service date. Frequency
    // trips instead shift the template by each run's start at the first stop.
    let (start, end, headway, first) = match (&t.start_time, &t.end_time, t.headway_secs) {
      (Some(start), Some(end), Some(headway)) => {
        let first = parse_time(t.first_departure_time.as_deref().ok_or("Missing first departure time")?)?;
        (i64::from(parse_time(start)?), i64::from(parse_time(end)?), i64::from(headway), i64::from(first))
      }
      (None, None, None) if t.frequency_trip_id.is_none() => (departure_time, departure_time + 1, 1, departure_time),
      _ => return Err("Incomplete GTFS frequency window"),
    };
    let arrival_offset = arrival_time - first;
    let departure_offset = departure_time - first;
    if headway == 0 || end <= start || departure_offset < 0 || arrival_offset > departure_offset {
      return Err("Invalid GTFS frequency window or stop offset");
    }
    if t.direction_id.is_some_and(|direction| !matches!(direction, 0 | 1)) {
      return Err("Invalid GTFS direction");
    }
    if t.exact_times.is_some_and(|exact| !matches!(exact, 0 | 1)) {
      return Err("Invalid GTFS exact_times");
    }
    let mut calendars = Vec::new();
    let mut dates = Vec::new();
    for calendar in &t.service.calendars {
      if calendar.flags().iter().any(|flag| !matches!(flag, 0 | 1)) {
        return Err("Calendar weekday flags must be 0 or 1");
      }
      let start = parse_date(calendar.start_date)?;
      let end = parse_date(calendar.end_date)?;
      if end < start {
        return Err("Reversed calendar date range");
      }
      dates.extend([start, end]);
      calendars.push((calendar, start, end));
    }
    let mut exceptions = BTreeMap::new();
    for exception in &t.service.exceptions {
      let date = parse_date(exception.date)?;
      if !matches!(exception.exception_type, 1 | 2) {
        return Err("Invalid calendar exception type");
      }
      exceptions.insert(date, exception.exception_type);
      if exception.exception_type == 1 {
        dates.push(date);
      }
    }
    let (Some(calendar_start), Some(calendar_end)) = (dates.iter().min().copied(), dates.iter().max().copied()) else {
      continue;
    };
    let first_departure = start + departure_offset;
    let last_departure = start + ((end - start - 1) / headway) * headway + departure_offset;
    let earliest =
      when.checked_sub_signed(Duration::seconds(last_departure)).ok_or("GTFS time overflow")?.with_timezone(&timezone).date_naive().pred_opt().ok_or("GTFS date overflow")?.max(calendar_start);
    let latest =
      until.checked_sub_signed(Duration::seconds(first_departure)).ok_or("GTFS time overflow")?.with_timezone(&timezone).date_naive().succ_opt().ok_or("GTFS date overflow")?.min(calendar_end);
    if earliest > latest {
      continue;
    }
    let mut ctx = ServiceContext::resolve(earliest, timezone)?;
    loop {
      let active = match exceptions.get(&ctx.date) {
        Some(kind) => *kind == 1,
        None => calendars.iter().any(|(calendar, start, end)| ctx.date >= *start && ctx.date <= *end && calendar.active(ctx.date)),
      };
      if active {
        let base = ctx.origin.timestamp();
        let delta = (cutoff - base - first_departure).max(0);
        let k = delta / headway + i64::from(delta % headway != 0);
        // Reject an exhausted band before multiplication, even for very large headways.
        let last_k = (end - start - 1) / headway;
        for run in k..=last_k.min(k + limit as i64 - 1) {
          let trip_start_secs = start + run * headway;
          let departure_secs = trip_start_secs + departure_offset;
          let departure = ctx.origin.checked_add_signed(Duration::seconds(departure_secs)).ok_or("Departure time overflow")?;
          if departure >= until {
            break;
          }
          let arrival = ctx.origin.checked_add_signed(Duration::seconds(trip_start_secs + arrival_offset)).ok_or("Arrival time overflow")?;
          let wait = departure.signed_duration_since(when);
          departures.push(Departure {
            trip_id: t.trip_id.clone(),
            route_id: t.route_id.clone(),
            service_id: t.service_id.clone(),
            trip_headsign: t.trip_headsign.clone(),
            direction_id: t.direction_id,
            stop_sequence: t.stop_sequence,
            service_date: ctx.date,
            agency_timezone: timezone_name.to_owned(),
            headway_secs: t.headway_secs,
            trip_start_secs: if t.headway_secs.is_some() { Some(u32::try_from(trip_start_secs).map_err(|_| "GTFS time overflow")?) } else { None },
            estimate_method: if t.headway_secs.is_some() && t.exact_times != Some(1) { "frequency_start_plus_headway" } else { "gtfs_schedule" }.into(),
            departure_secs: u32::try_from(departure_secs).map_err(|_| "GTFS time overflow")?,
            service_departure_time: fmt_secs(u32::try_from(departure_secs).map_err(|_| "GTFS time overflow")?),
            estimated_arrival_at: arrival.fixed_offset(),
            estimated_departure_at: departure.fixed_offset(),
            wait_seconds: (wait.num_seconds() + i64::from(wait.subsec_nanos() > 0)) as u32,
          });
        }
      }
      if ctx.date == latest {
        break;
      }
      // Advance from each candidate service date, not the request's wall-clock
      // date: at 01:15 Sunday, both Saturday and Sunday may supply trains.
      // The next candidate after Saturday is Sunday, not Monday.
      ctx = ctx.next_day(timezone)?;
    }
    departures.sort_by(|a, b| {
      (a.estimated_departure_at, &a.trip_id, a.service_date, a.trip_start_secs, a.stop_sequence).cmp(&(b.estimated_departure_at, &b.trip_id, b.service_date, b.trip_start_secs, b.stop_sequence))
    });
    departures.dedup_by(|a, b| a.trip_id == b.trip_id && a.service_date == b.service_date && a.trip_start_secs == b.trip_start_secs && a.stop_sequence == b.stop_sequence);
    departures.truncate(limit);
  }
  Ok(departures)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
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
    t.agency_timezone = Some("Invalid/Timezone".into());
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

  #[test]
  fn falls_back_to_asia_kuala_lumpur_when_timezone_missing_or_empty() {
    let mut t_none = template();
    t_none.agency_timezone = None;
    let rows = next_departures(&[t_none], dt("2026-08-27T06:00:00+08:00"), 1).unwrap();
    assert_eq!(rows[0].agency_timezone, "Asia/Kuala_Lumpur");

    let mut t_empty = template();
    t_empty.agency_timezone = Some("   ".into());
    let rows = next_departures(&[t_empty], dt("2026-08-27T06:00:00+08:00"), 1).unwrap();
    assert_eq!(rows[0].agency_timezone, "Asia/Kuala_Lumpur");
  }
}
