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
    let timezone_name = t.agency_timezone.as_deref().ok_or("Missing agency timezone")?.trim();
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

// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
