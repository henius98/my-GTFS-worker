use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

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
  pub(super) monday: u8,
  pub(super) tuesday: u8,
  pub(super) wednesday: u8,
  pub(super) thursday: u8,
  pub(super) friday: u8,
  pub(super) saturday: u8,
  pub(super) sunday: u8,
  pub(super) start_date: i32,
  pub(super) end_date: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Exception {
  pub service_id: String,
  pub date: i32,
  pub exception_type: u8,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedCalendar {
  flags: [u8; 7],
  pub start_date: NaiveDate,
  pub end_date: NaiveDate,
}

impl ParsedCalendar {
  fn active(&self, date: NaiveDate) -> bool {
    self.flags[date.weekday().num_days_from_monday() as usize] == 1
  }
}

#[derive(Debug, Clone)]
pub(super) struct ParsedService {
  pub calendars: Vec<ParsedCalendar>,
  pub exceptions: BTreeMap<NaiveDate, u8>,
  pub calendar_start: NaiveDate,
  pub calendar_end: NaiveDate,
}

impl Calendar {
  fn flags(&self) -> [u8; 7] {
    [self.monday, self.tuesday, self.wednesday, self.thursday, self.friday, self.saturday, self.sunday]
  }
}

impl ParsedService {
  pub fn build(calendars: &[Calendar], exceptions: &[Exception]) -> Result<Option<Self>, &'static str> {
    if calendars.is_empty() && exceptions.is_empty() {
      return Ok(None);
    }
    let mut parsed_calendars = Vec::with_capacity(calendars.len());
    let mut dates = Vec::new();
    for calendar in calendars {
      let flags = calendar.flags();
      if flags.iter().any(|flag| !matches!(flag, 0 | 1)) {
        return Err("Calendar weekday flags must be 0 or 1");
      }
      let start = parse_date(calendar.start_date)?;
      let end = parse_date(calendar.end_date)?;
      if end < start {
        return Err("Reversed calendar date range");
      }
      dates.extend([start, end]);
      parsed_calendars.push(ParsedCalendar { flags, start_date: start, end_date: end });
    }
    let mut parsed_exceptions = BTreeMap::new();
    for exception in exceptions {
      let date = parse_date(exception.date)?;
      if !matches!(exception.exception_type, 1 | 2) {
        return Err("Invalid calendar exception type");
      }
      parsed_exceptions.insert(date, exception.exception_type);
      if exception.exception_type == 1 {
        dates.push(date);
      }
    }
    let (Some(calendar_start), Some(calendar_end)) = (dates.iter().min().copied(), dates.iter().max().copied()) else {
      return Ok(None);
    };
    Ok(Some(Self {
      calendars: parsed_calendars,
      exceptions: parsed_exceptions,
      calendar_start,
      calendar_end,
    }))
  }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Template {
  pub(crate) trip_id: String,
  pub(crate) route_id: String,
  #[serde(default)]
  pub(crate) route_short_name: Option<String>,
  #[serde(default)]
  pub(crate) route_long_name: Option<String>,
  #[serde(default)]
  pub(crate) route_color: Option<String>,
  #[serde(default)]
  pub(crate) route_text_color: Option<String>,
  pub(super) service_id: String,
  pub(crate) trip_headsign: Option<String>,
  pub(crate) direction_id: Option<u8>,
  pub(crate) stop_sequence: u32,
  pub(crate) arrival_time: String,
  pub(crate) departure_time: String,
  pub(crate) first_departure_time: Option<String>,
  pub(crate) start_time: Option<String>,
  pub(crate) end_time: Option<String>,
  pub(crate) headway_secs: Option<u32>,
  pub(crate) frequency_trip_id: Option<String>,
  pub(crate) exact_times: Option<u8>,
  pub(crate) agency_timezone: Option<String>,
  #[serde(skip)]
  pub(super) service: Option<Arc<ParsedService>>,
}

/// Resolve a service date's origin in the agency timezone. GTFS defines the
/// origin as local noon minus twelve elapsed hours, including on DST days.
fn service_origin(date: NaiveDate, timezone: Tz) -> Result<DateTime<Tz>, &'static str> {
  let noon_time = date.and_hms_opt(12, 0, 0).ok_or("Invalid agency service date")?;
  let noon = timezone.from_local_datetime(&noon_time).single().ok_or("Invalid agency service date")?;
  Ok(noon - Duration::hours(12))
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
  if !(10000101..=99991231).contains(&value) {
    return Err("Calendar date must be YYYYMMDD");
  }
  let year = value / 10000;
  let month = ((value % 10000) / 100) as u32;
  let day = (value % 100) as u32;
  NaiveDate::from_ymd_opt(year, month, day).ok_or("Invalid calendar date")
}

// ---------------------------------------------------------------------------

/// Departures carry their actual service dates, which may precede the request
/// date after midnight. Waits always use the original requested instant.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Departure {
  trip_id: String,
  route_id: String,
  #[serde(default)]
  route_short_name: Option<String>,
  #[serde(default)]
  route_long_name: Option<String>,
  #[serde(default)]
  route_color: Option<String>,
  #[serde(default)]
  route_text_color: Option<String>,
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

struct PreparedTemplate<'a> {
  template: &'a Template,
  timezone: Tz,
  timezone_name: &'a str,
  start: i64,
  end: i64,
  headway: i64,
  first_departure: i64,
  departure_offset: i64,
  arrival_offset: i64,
  earliest: NaiveDate,
  latest: NaiveDate,
  parsed_service: &'a ParsedService,
}

#[derive(Clone, Copy)]
struct Candidate {
  template_idx: usize,
  service_date: NaiveDate,
  trip_start_secs: Option<u32>,
  departure_secs: u32,
  estimated_arrival_at: DateTime<FixedOffset>,
  estimated_departure_at: DateTime<FixedOffset>,
  wait_seconds: u32,
}

fn sort_and_dedup_candidates(candidates: &mut Vec<Candidate>, prepared: &[PreparedTemplate<'_>]) {
  candidates.sort_by(|a, b| {
    let ta = &prepared[a.template_idx].template;
    let tb = &prepared[b.template_idx].template;
    (a.estimated_departure_at, &ta.trip_id, a.service_date, a.trip_start_secs, ta.stop_sequence)
      .cmp(&(b.estimated_departure_at, &tb.trip_id, b.service_date, b.trip_start_secs, tb.stop_sequence))
  });
  candidates.dedup_by(|a, b| {
    let ta = &prepared[a.template_idx].template;
    let tb = &prepared[b.template_idx].template;
    ta.trip_id == tb.trip_id && a.service_date == b.service_date && a.trip_start_secs == b.trip_start_secs && ta.stop_sequence == tb.stop_sequence
  });
}

/// Next `limit` estimated departures before the exclusive search horizon.
///
/// If today's service is exhausted or has too few results, include the next
/// active dates and merge them with previous-day trains still running.
/// Every candidate is checked against the original requested instant.
pub(crate) fn next_departures(templates: &[Template], when: DateTime<Utc>, until: DateTime<Utc>, limit: usize) -> Result<Vec<Departure>, &'static str> {
  let cutoff = when.timestamp() + i64::from(when.timestamp_subsec_nanos() > 0);
  let mut prepared = Vec::with_capacity(templates.len());

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
    let Some(parsed_service) = t.service.as_deref() else {
      continue;
    };
    let first_departure = start + departure_offset;
    let last_departure = start + ((end - start - 1) / headway) * headway + departure_offset;
    let earliest = when
      .checked_sub_signed(Duration::seconds(last_departure))
      .ok_or("GTFS time overflow")?
      .with_timezone(&timezone)
      .date_naive()
      .pred_opt()
      .ok_or("GTFS date overflow")?
      .max(parsed_service.calendar_start);
    let latest = until
      .checked_sub_signed(Duration::seconds(first_departure))
      .ok_or("GTFS time overflow")?
      .with_timezone(&timezone)
      .date_naive()
      .succ_opt()
      .ok_or("GTFS date overflow")?
      .min(parsed_service.calendar_end);
    if earliest > latest {
      continue;
    }
    prepared.push(PreparedTemplate {
      template: t,
      timezone,
      timezone_name,
      start,
      end,
      headway,
      first_departure,
      departure_offset,
      arrival_offset,
      earliest,
      latest,
      parsed_service,
    });
  }

  if prepared.is_empty() {
    return Ok(Vec::new());
  }

  let min_date = prepared.iter().map(|p| p.earliest).min().ok_or("No template date bounds")?;
  let max_date = prepared.iter().map(|p| p.latest).max().ok_or("No template date bounds")?;

  let mut current_date = min_date;
  let mut candidates = Vec::new();

  while current_date <= max_date {
    let mut current_origin: Option<(Tz, DateTime<Tz>)> = None;

    for (template_idx, p) in prepared.iter().enumerate() {
      if current_date < p.earliest || current_date > p.latest {
        continue;
      }
      let active = match p.parsed_service.exceptions.get(&current_date) {
        Some(kind) => *kind == 1,
        None => p.parsed_service.calendars.iter().any(|calendar| {
          current_date >= calendar.start_date && current_date <= calendar.end_date && calendar.active(current_date)
        }),
      };
      if !active {
        continue;
      }
      let origin = match current_origin {
        Some((tz, orig)) if tz == p.timezone => orig,
        _ => {
          let orig = service_origin(current_date, p.timezone)?;
          current_origin = Some((p.timezone, orig));
          orig
        }
      };
      let base = origin.timestamp();
      let delta = (cutoff - base - p.first_departure).max(0);
      let k = delta / p.headway + i64::from(delta % p.headway != 0);
      let last_k = (p.end - p.start - 1) / p.headway;
      for run in k..=last_k.min(k + limit as i64 - 1) {
        let trip_start_secs = p.start + run * p.headway;
        let departure_secs = trip_start_secs + p.departure_offset;
        let departure = origin.checked_add_signed(Duration::seconds(departure_secs)).ok_or("Departure time overflow")?;
        if departure >= until {
          break;
        }
        let arrival = origin.checked_add_signed(Duration::seconds(trip_start_secs + p.arrival_offset)).ok_or("Arrival time overflow")?;
        let wait = departure.signed_duration_since(when);
        candidates.push(Candidate {
          template_idx,
          service_date: current_date,
          trip_start_secs: if p.template.headway_secs.is_some() { Some(u32::try_from(trip_start_secs).map_err(|_| "GTFS time overflow")?) } else { None },
          departure_secs: u32::try_from(departure_secs).map_err(|_| "GTFS time overflow")?,
          estimated_arrival_at: arrival.fixed_offset(),
          estimated_departure_at: departure.fixed_offset(),
          wait_seconds: u32::try_from(wait.num_seconds() + i64::from(wait.subsec_nanos() > 0)).map_err(|_| "Wait time overflow")?,
        });
      }
    }

    if candidates.len() >= limit && current_date < max_date {
      sort_and_dedup_candidates(&mut candidates, &prepared);

      if candidates.len() >= limit {
        let t_limit = candidates[limit - 1].estimated_departure_at;
        let next_date = current_date.succ_opt().ok_or("Service date overflow")?;
        let mut earliest_future_departure = None;
        let mut next_origin: Option<(Tz, DateTime<Tz>)> = None;
        for p in &prepared {
          if next_date >= p.earliest && next_date <= p.latest {
            let origin = match next_origin {
              Some((tz, orig)) if tz == p.timezone => orig,
              _ => {
                let orig = service_origin(next_date, p.timezone)?;
                next_origin = Some((p.timezone, orig));
                orig
              }
            };
            let min_dep = origin.checked_add_signed(Duration::seconds(p.first_departure)).ok_or("Departure time overflow")?.fixed_offset();
            earliest_future_departure = Some(match earliest_future_departure {
              Some(cur) if cur < min_dep => cur,
              _ => min_dep,
            });
          }
        }
        if let Some(min_future) = earliest_future_departure {
          if min_future >= t_limit {
            break;
          }
        } else {
          break;
        }
      }
    }

    current_date = match current_date.succ_opt() {
      Some(d) => d,
      None => break,
    };
  }

  sort_and_dedup_candidates(&mut candidates, &prepared);
  candidates.truncate(limit);

  let departures = candidates
    .into_iter()
    .map(|c| {
      let p = &prepared[c.template_idx];
      Departure {
        trip_id: p.template.trip_id.clone(),
        route_id: p.template.route_id.clone(),
        route_short_name: p.template.route_short_name.clone(),
        route_long_name: p.template.route_long_name.clone(),
        route_color: p.template.route_color.clone(),
        route_text_color: p.template.route_text_color.clone(),
        service_id: p.template.service_id.clone(),
        trip_headsign: p.template.trip_headsign.clone(),
        direction_id: p.template.direction_id,
        stop_sequence: p.template.stop_sequence,
        service_date: c.service_date,
        agency_timezone: p.timezone_name.to_owned(),
        headway_secs: p.template.headway_secs,
        trip_start_secs: c.trip_start_secs,
        estimate_method: if p.template.headway_secs.is_some() && p.template.exact_times != Some(1) { "frequency_start_plus_headway" } else { "gtfs_schedule" }.into(),
        departure_secs: c.departure_secs,
        service_departure_time: fmt_secs(c.departure_secs),
        estimated_arrival_at: c.estimated_arrival_at,
        estimated_departure_at: c.estimated_departure_at,
        wait_seconds: c.wait_seconds,
      }
    })
    .collect();

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

  fn default_calendar() -> Calendar {
    let value = serde_json::json!({
        "service_id": "weekday-id",
        "monday": 1, "tuesday": 1, "wednesday": 1, "thursday": 1, "friday": 1, "saturday": 1, "sunday": 1,
        "start_date": 20260101, "end_date": 20261231
    });
    serde_json::from_value(value).unwrap()
  }

  fn template_with_service(cal: Calendar, exceptions: Vec<Exception>) -> Template {
    let value = serde_json::json!({
        "trip_id": "opaque-trip", "route_id": "KJL", "service_id": "weekday-id",
        "trip_headsign": "Gombak", "direction_id": 0, "stop_sequence": 10,
        "arrival_time": "6:09:42", "departure_time": "6:10:00", "first_departure_time": "6:00:00",
        "start_time": "6:00:00", "end_time": "7:00:00", "headway_secs": 600, "frequency_trip_id": "opaque-trip",
        "agency_timezone": "Asia/Kuala_Lumpur", "monday": 1, "tuesday": 1,
        "wednesday": 1, "thursday": 1, "friday": 1, "saturday": 1, "sunday": 1,
        "start_date": 20260101, "end_date": 20261231
    });
    let mut template: Template = serde_json::from_value(value).unwrap();
    template.service = ParsedService::build(&[cal], &exceptions).unwrap().map(Arc::new);
    template
  }

  fn template() -> Template {
    template_with_service(default_calendar(), vec![])
  }

  #[test]
  fn weekday_uses_calendar_flags() {
    // 2026-08-27 is a Thursday.
    let mut cal = default_calendar();
    cal.thursday = 0;
    let t = template_with_service(cal, vec![]);
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
    let mut cal = default_calendar();
    cal.start_date = 20260827;
    cal.end_date = 20260827;
    let t = template_with_service(cal, vec![]);
    let rows = next_departures(&[t.clone(), t.clone()], dt("2026-08-27T06:00:00+08:00"), 20).unwrap();
    assert_eq!(rows.len(), 6);
    assert!(next_departures(&[t.clone()], dt("2026-08-28T06:00:00+08:00"), 5).unwrap().is_empty());
    assert!(next_departures(&[t], dt("2026-08-19T06:00:00+08:00"), 5).unwrap().is_empty());
  }

  #[test]
  fn seven_day_horizon_is_exclusive_and_seconds_are_not_rounded_down() {
    let mut cal = default_calendar();
    cal.start_date = 20260903;
    cal.end_date = 20260903;
    let t = template_with_service(cal, vec![]);
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
    for t in variants {
      assert!(next_departures(&[t], now, 5).is_err());
    }

    let mut cal = default_calendar();
    cal.end_date = 20260230;
    assert!(ParsedService::build(&[cal], &[]).is_err());
    let mut cal = default_calendar();
    cal.end_date = 20250101;
    assert!(ParsedService::build(&[cal], &[]).is_err());
    let mut cal = default_calendar();
    cal.thursday = 2;
    assert!(ParsedService::build(&[cal], &[]).is_err());
  }

  #[test]
  fn dst_uses_noon_minus_twelve_hours() {
    let origin = service_origin(NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(), chrono_tz::America::New_York).unwrap();
    assert_eq!(origin.with_timezone(&Utc), dt("2026-03-08T04:00:00Z"));
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
    let mut cal = default_calendar();
    cal.end_date = 20260827;
    let exceptions = vec![
      Exception { service_id: "weekday-id".into(), date: 20260827, exception_type: 2 },
      Exception { service_id: "weekday-id".into(), date: 20260829, exception_type: 1 },
    ];
    let mut t = scheduled_template();
    t.service = ParsedService::build(&[cal], &exceptions).unwrap().map(Arc::new);
    let rows = next_departures(&[t.clone()], dt("2026-08-27T06:00:00+08:00"), 5).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].service_date.to_string(), "2026-08-29");

    t.service = ParsedService::build(&[], &exceptions).unwrap().map(Arc::new);
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

  #[test]
  fn route_metadata_propagates_to_departure() {
    let mut t = template();
    t.route_short_name = Some("KJL".into());
    t.route_long_name = Some("Kelana Jaya Line".into());
    t.route_color = Some("E51B24".into());
    t.route_text_color = Some("FFFFFF".into());
    let rows = next_departures(&[t], dt("2026-08-27T06:00:00+08:00"), 1).unwrap();
    assert_eq!(rows[0].route_short_name.as_deref(), Some("KJL"));
    assert_eq!(rows[0].route_long_name.as_deref(), Some("Kelana Jaya Line"));
    assert_eq!(rows[0].route_color.as_deref(), Some("E51B24"));
    assert_eq!(rows[0].route_text_color.as_deref(), Some("FFFFFF"));
  }
}
