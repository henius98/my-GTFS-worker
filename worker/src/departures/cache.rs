use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use worker::{D1Database, Env, Result};

use super::schedule::Departure;
use super::{Parameters, Stop};

const FEED_STATE_SQL: &str = include_str!("sql/feed_state.sql");

pub(super) fn ttl_seconds(env: &Env) -> Result<i64> {
  crate::feed_cache_ttl_seconds(env)
}

#[derive(Deserialize)]
pub(super) struct FeedState {
  revision: String,
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
#[path = "../../../test/unit/worker/departures/cache.rs"]
mod tests;
