use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

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
    Ok(Some(Self { calendars: parsed_calendars, exceptions: parsed_exceptions, calendar_start, calendar_end }))
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

fn cached_service_origin(cache: &mut Option<(Tz, DateTime<Tz>)>, date: NaiveDate, timezone: Tz) -> Result<DateTime<Tz>, &'static str> {
  if let Some((tz, orig)) = *cache
    && tz == timezone
  {
    return Ok(orig);
  }
  let orig = service_origin(date, timezone)?;
  *cache = Some((timezone, orig));
  Ok(orig)
}

/// Format seconds into a service day, retaining hours past midnight.
fn fmt_secs(secs: u32) -> String {
  format!("{:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn parse_time(value: &str) -> Result<u32, &'static str> {
  let mut parts = value.trim().split(':');
  let (Some(h), Some(m), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
    return Err("Malformed GTFS time");
  };
  if h.is_empty() || m.len() != 2 || s.len() != 2 || [h, m, s].iter().any(|part| !part.bytes().all(|b| b.is_ascii_digit())) {
    return Err("Malformed GTFS time");
  }
  let hours = h.parse::<u32>().map_err(|_| "GTFS hours out of range")?;
  let minutes = m.parse::<u8>().map_err(|_| "Invalid GTFS minutes")?;
  let seconds = s.parse::<u8>().map_err(|_| "Invalid GTFS seconds")?;
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
  pub(super) route_short_name: Option<String>,
  #[serde(default)]
  pub(super) route_long_name: Option<String>,
  #[serde(default)]
  pub(super) route_color: Option<String>,
  #[serde(default)]
  pub(super) route_text_color: Option<String>,
  service_id: String,
  pub(super) trip_headsign: Option<String>,
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
    (a.estimated_departure_at, &ta.trip_id, a.service_date, a.trip_start_secs, ta.stop_sequence).cmp(&(b.estimated_departure_at, &tb.trip_id, b.service_date, b.trip_start_secs, tb.stop_sequence))
  });
  candidates.dedup_by(|a, b| {
    let ta = &prepared[a.template_idx].template;
    let tb = &prepared[b.template_idx].template;
    ta.trip_id == tb.trip_id && a.service_date == b.service_date && a.trip_start_secs == b.trip_start_secs && ta.stop_sequence == tb.stop_sequence
  });
}

fn prepare_templates<'a>(templates: &'a [Template], when: DateTime<Utc>, until: DateTime<Utc>, default_timezone: &'a str) -> Result<Vec<PreparedTemplate<'a>>, &'static str> {
  let mut prepared = Vec::with_capacity(templates.len());
  let mut last_tz: Option<(&'a str, Tz)> = None;

  for t in templates {
    let timezone_name = match t.agency_timezone.as_deref().map(str::trim) {
      Some(tz) if !tz.is_empty() => tz,
      _ => default_timezone,
    };
    let timezone = match last_tz {
      Some((name, tz)) if name == timezone_name => tz,
      _ => {
        let tz: Tz = timezone_name.parse().map_err(|_| "Unknown agency timezone")?;
        last_tz = Some((timezone_name, tz));
        tz
      }
    };
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
    prepared.push(PreparedTemplate { template: t, timezone, timezone_name, start, end, headway, first_departure, departure_offset, arrival_offset, earliest, latest, parsed_service });
  }
  Ok(prepared)
}

fn earliest_future_departure(prepared: &[PreparedTemplate<'_>], next_date: NaiveDate) -> Result<Option<DateTime<FixedOffset>>, &'static str> {
  let mut earliest = None;
  let mut next_origin: Option<(Tz, DateTime<Tz>)> = None;
  for p in prepared {
    if next_date >= p.earliest && next_date <= p.latest {
      let origin = cached_service_origin(&mut next_origin, next_date, p.timezone)?;
      let min_dep = origin.checked_add_signed(Duration::seconds(p.first_departure)).ok_or("Departure time overflow")?.fixed_offset();
      earliest = Some(match earliest {
        Some(cur) if cur < min_dep => cur,
        _ => min_dep,
      });
    }
  }
  Ok(earliest)
}

/// GTFS service days may run past 24:00:00. Search all dates whose frequency
/// windows reach the request, instead of assigning a fixed rollover hour.
/// Next `limit` estimated departures before the exclusive search horizon.
///
/// If today's service is exhausted or has too few results, include the next
/// active dates and merge them with previous-day trains still running.
/// Every candidate is checked against the original requested instant.
pub(crate) fn next_departures(templates: &[Template], when: DateTime<Utc>, until: DateTime<Utc>, limit: usize, default_timezone: &str) -> Result<Vec<Departure>, &'static str> {
  let cutoff = when.timestamp() + i64::from(when.timestamp_subsec_nanos() > 0);
  let prepared = prepare_templates(templates, when, until, default_timezone)?;
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
        None => p.parsed_service.calendars.iter().any(|calendar| current_date >= calendar.start_date && current_date <= calendar.end_date && calendar.active(current_date)),
      };
      if !active {
        continue;
      }
      let origin = cached_service_origin(&mut current_origin, current_date, p.timezone)?;
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
        if earliest_future_departure(&prepared, next_date)?.is_none_or(|min_future| min_future >= t_limit) {
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
#[path = "../../../test/unit/worker/departures/schedule.rs"]
mod tests;
