use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use worker::{D1Database, Env, Result};

use super::schedule::Departure;
use super::{MAX_LIMIT, Parameters, Stop};

const DEFAULT_TTL_SECONDS: i64 = 24 * 60 * 60;
const MAX_TTL_SECONDS: i64 = i64::MAX / 1_000;
const FEED_STATE_SQL: &str = include_str!("sql/feed_state.sql");

pub(super) fn ttl_seconds(env: &Env) -> i64 {
  parse_ttl_seconds(env.var("WORKER_DEPARTURE_CACHE_TTL_SECONDS").ok().map(|value| value.to_string()).as_deref())
}

fn parse_ttl_seconds(value: Option<&str>) -> i64 {
  value.and_then(|value| value.trim().parse::<i64>().ok()).filter(|seconds| (1..=MAX_TTL_SECONDS).contains(seconds)).unwrap_or(DEFAULT_TTL_SECONDS)
}

#[derive(Deserialize)]
pub(super) struct FeedState {
  revision: String,
  importing: u8,
}

impl FeedState {
  pub fn cacheable(&self) -> bool {
    self.importing == 0
  }
}

#[derive(Deserialize, Serialize)]
pub(super) struct Entry {
  pub stop: Stop,
  pub from: DateTime<Utc>,
  pub until: DateTime<Utc>,
  pub departures: Vec<Departure>,
}

impl Entry {
  pub fn select(&self, when: DateTime<Utc>, until: DateTime<Utc>, limit: usize) -> Option<Vec<Departure>> {
    if when < self.from || until > self.until {
      return None;
    }
    let mut departures: Vec<_> = self.departures.iter().filter(|departure| departure.estimated_departure_at >= when && departure.estimated_departure_at < until).take(limit).cloned().collect();
    // A truncated cache can lose early departures as time advances. Recompute
    // when it no longer contains enough results to answer the whole request.
    if departures.len() < limit && self.departures.len() == MAX_LIMIT && self.departures.last().is_some_and(|departure| departure.estimated_departure_at < until) {
      return None;
    }
    for departure in &mut departures {
      let wait = departure.estimated_departure_at.signed_duration_since(when);
      departure.wait_seconds = u32::try_from(wait.num_seconds() + i64::from(wait.subsec_nanos() > 0)).ok()?;
    }
    Some(departures)
  }
}

pub(super) async fn feed_state(db: &D1Database) -> Result<FeedState> {
  db.prepare(FEED_STATE_SQL).first(None).await?.ok_or_else(|| worker::Error::RustError("Import state is unavailable".into()))
}

pub(super) async fn read(db: &D1Database, params: &Parameters, state: &FeedState, now: DateTime<Utc>) -> Result<Option<Entry>> {
  let payload = db
    .prepare(include_str!("sql/cache_read.sql"))
    .bind(&[params.stop_id.clone().into(), params.route_id.as_deref().unwrap_or("").into(), params.direction_id.unwrap_or(-1).into(), state.revision.clone().into(), (now.timestamp() as f64).into()])?
    .first::<String>(Some("payload"))
    .await?;
  Ok(payload.map(|payload| serde_json::from_str(&payload)).transpose()?)
}

pub(super) async fn write(db: &D1Database, params: &Parameters, state: &FeedState, now: DateTime<Utc>, ttl_seconds: i64, entry: &Entry) -> Result<()> {
  // Check the import revision in the same statement as the upsert so an import
  // starting during calculation cannot publish results under an old revision.
  let sql = include_str!("sql/cache_write.sql").replace("__FEED_STATE__", FEED_STATE_SQL);
  let ttl = Duration::try_seconds(ttl_seconds).ok_or_else(|| worker::Error::RustError("Departure cache lifetime is invalid".into()))?;
  let expires_at = now.checked_add_signed(ttl).ok_or_else(|| worker::Error::RustError("Departure cache expiry is out of range".into()))?.timestamp();
  let result = db
    .prepare(sql)
    .bind(&[
      params.stop_id.clone().into(),
      params.route_id.as_deref().unwrap_or("").into(),
      params.direction_id.unwrap_or(-1).into(),
      state.revision.clone().into(),
      (expires_at as f64).into(),
      serde_json::to_string(entry)?.into(),
    ])?
    .run()
    .await?;
  if !result.success() {
    return Err(worker::Error::RustError("Departure cache write failed".into()));
  }
  Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
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
}
