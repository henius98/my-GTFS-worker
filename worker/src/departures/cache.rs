use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use worker::{D1Type, Result};

use super::schedule::Departure;
use super::{Parameters, Stop};
use crate::database::ProviderDb;

const FEED_STATE_SQL: &str = include_str!("sql/feed_state.sql");

pub(super) struct FeedState {
  revision: String,
}

#[derive(Deserialize)]
struct CacheReadRow {
  revision: String,
  payload: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Entry {
  pub stop: Stop,
  pub from: DateTime<Utc>,
  pub until: DateTime<Utc>,
  pub departures: Vec<Departure>,
}

impl Entry {
  pub fn select(&self, when: DateTime<Utc>, until: DateTime<Utc>, limit: usize, max_limit: usize) -> Option<Vec<Departure>> {
    if when < self.from || until > self.until {
      return None;
    }
    let mut departures: Vec<_> = self.departures.iter().filter(|departure| departure.estimated_departure_at >= when && departure.estimated_departure_at < until).take(limit).cloned().collect();
    // A truncated cache can lose early departures as time advances. Recompute
    // when it no longer contains enough results to answer the whole request.
    if departures.len() < limit && self.departures.len() == max_limit && self.departures.last().is_some_and(|departure| departure.estimated_departure_at < until) {
      return None;
    }
    for departure in &mut departures {
      let wait = departure.estimated_departure_at.signed_duration_since(when);
      departure.wait_seconds = u32::try_from(wait.num_seconds() + i64::from(wait.subsec_nanos() > 0)).ok()?;
    }
    Some(departures)
  }
}

pub(super) async fn read_with_state(db: &ProviderDb, params: &Parameters, now: DateTime<Utc>) -> Result<(FeedState, Option<Entry>)> {
  let sql = include_str!("sql/cache_read.sql").replace("__FEED_STATE__", FEED_STATE_SQL);
  let stmt = db.prepare(sql).bind_refs(&[
    D1Type::Text(&params.stop_id),
    D1Type::Text(params.route_id.as_deref().unwrap_or("")),
    D1Type::Integer(params.direction_id.unwrap_or(-1)),
    D1Type::Real(now.timestamp() as f64),
  ])?;
  let result = db.all("cache_read", &stmt).await?;
  if !result.success() {
    return Err(worker::Error::RustError("Departure cache read failed".into()));
  }
  let row = result.results::<CacheReadRow>()?.into_iter().next().ok_or_else(|| worker::Error::RustError("Import state is unavailable".into()))?;
  let entry = row.payload.map(|payload| serde_json::from_str(&payload)).transpose()?;
  Ok((FeedState { revision: row.revision }, entry))
}

pub(super) async fn write(db: &ProviderDb, params: &Parameters, state: &FeedState, now: DateTime<Utc>, ttl: Duration, entry: &Entry) -> Result<()> {
  // Check the import revision in the same statement as the upsert so an import
  // starting during calculation cannot publish results under an old revision.
  let sql = include_str!("sql/cache_write.sql").replace("__FEED_STATE__", FEED_STATE_SQL);
  let expires_at = now.checked_add_signed(ttl).ok_or_else(|| worker::Error::RustError("Departure cache expiry is out of range".into()))?.timestamp();
  let payload = serde_json::to_string(entry)?;
  let stmt = db.prepare(sql).bind_refs(&[
    D1Type::Text(&params.stop_id),
    D1Type::Text(params.route_id.as_deref().unwrap_or("")),
    D1Type::Integer(params.direction_id.unwrap_or(-1)),
    D1Type::Text(&state.revision),
    D1Type::Real(expires_at as f64),
    D1Type::Text(&payload),
  ])?;
  let result = db.run("cache_write", &stmt).await?;
  if !result.success() {
    return Err(worker::Error::RustError("Departure cache write failed".into()));
  }
  Ok(())
}

#[cfg(test)]
#[path = "../../../test/unit/worker/departures/cache.rs"]
mod tests;
