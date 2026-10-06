mod cache;
mod query;
mod schedule;

use chrono::{DateTime, Duration, Utc};
use schedule::{Departure, next_departures};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use worker::{Date, Env, Method, Request, Response, Result, Url};

use crate::database;

const MAX_LIMIT: usize = 100;

struct Parameters {
  stop_id: String,
  route_id: Option<String>,
  when: DateTime<Utc>,
  direction_id: Option<i32>,
  limit: usize,
}

impl Parameters {
  fn parse(url: &Url, now: DateTime<Utc>, cache_ttl_seconds: i64) -> std::result::Result<Self, &'static str> {
    let mut values = HashMap::new();
    for (key, value) in url.query_pairs() {
      if !matches!(key.as_ref(), "stop_id" | "route_id" | "direction_id" | "limit" | "at") {
        return Err("Unknown query parameter");
      }
      if values.insert(key.into_owned(), value.into_owned()).is_some() {
        return Err("Duplicate query parameter");
      }
    }
    let stop_id = values.remove("stop_id").ok_or("stop_id is required")?;
    let route_id = values.remove("route_id");
    for id in std::iter::once(&stop_id).chain(route_id.iter()) {
      if id.trim().is_empty() || id.len() > 256 {
        return Err("Identifiers must contain 1 to 256 bytes");
      }
    }
    let direction_id = match values.remove("direction_id").as_deref() {
      None => None,
      Some("0") => Some(0),
      Some("1") => Some(1),
      _ => return Err("direction_id must be 0 or 1"),
    };
    let limit = match values.remove("limit") {
      Some(value) if value.bytes().all(|b| b.is_ascii_digit()) => value.parse().ok(),
      Some(_) => None,
      None => Some(5),
    }
    .filter(|n| (1..=MAX_LIMIT).contains(n))
    .ok_or("limit must be between 1 and 100")?;
    let when = match values.remove("at") {
      Some(value) => DateTime::parse_from_rfc3339(&value).map_err(|_| "at must be RFC3339 with a timezone, e.g. 2026-09-23T08:00:00Z")?.with_timezone(&Utc),
      None => now,
    };
    let cache_ttl = Duration::try_seconds(cache_ttl_seconds).ok_or("Departure cache lifetime is invalid")?;
    if when.checked_add_signed(Duration::days(schedule::SEARCH_DAYS) + cache_ttl).is_none() {
      return Err("at is outside the supported date range");
    }
    Ok(Self { stop_id, route_id, when, direction_id, limit })
  }
}

#[derive(Deserialize, Serialize)]
struct Stop {
  stop_id: String,
  stop_name: Option<String>,
}

fn json_error(status: u16, message: &str) -> Result<Response> {
  Ok(Response::from_json(&json!({ "error": message }))?.with_status(status))
}

pub async fn handle(req: &Request, env: &Env, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return json_error(405, "Method not allowed");
  }
  let now = DateTime::from_timestamp_millis(Date::now().as_millis() as i64).ok_or_else(|| worker::Error::RustError("Invalid system time".into()))?;
  let cache_ttl_seconds = cache::ttl_seconds(env);
  let params = match Parameters::parse(url, now, cache_ttl_seconds) {
    Ok(params) => params,
    Err(message) => return json_error(400, message),
  };
  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };
  match calculate(&db, params, now, cache_ttl_seconds).await {
    Ok(response) => Ok(response),
    Err(error) => {
      worker::console_error!("Departure lookup failed: {}", error);
      json_error(503, "Departure data is unavailable")
    }
  }
}

async fn calculate(db: &worker::D1Database, params: Parameters, now: DateTime<Utc>, cache_ttl_seconds: i64) -> Result<Response> {
  let until = params.when + Duration::days(schedule::SEARCH_DAYS);
  let mut state = match cache::feed_state(db).await {
    Ok(state) if state.cacheable() => Some(state),
    Ok(_) => None,
    Err(error) => {
      worker::console_error!("Departure cache import state unavailable: {}", error);
      None
    }
  };
  if let Some(feed_state) = &state {
    match cache::read(db, &params, feed_state, now).await {
      Ok(Some(entry)) => {
        if let Some(departures) = entry.select(params.when, until, params.limit) {
          return response(&params, &entry.stop, departures, "HIT");
        }
      }
      Ok(None) => {}
      Err(error) => {
        worker::console_error!("Departure cache read failed: {}", error);
        state = None;
      }
    }
  }
  let stop = db.prepare("SELECT stop_id, stop_name FROM stops WHERE stop_id = ?1").bind(&[params.stop_id.clone().into()])?.first::<Stop>(None).await?;
  let Some(stop) = stop else {
    return json_error(404, "Unknown stop_id");
  };
  let templates = query::load(db, &params).await?;
  // Extend coverage by the cache lifetime so a later request still has a full
  // seven-day horizon, including when this stop has very few departures.
  let cache_ttl = Duration::try_seconds(cache_ttl_seconds).ok_or_else(|| worker::Error::RustError("Departure cache lifetime is invalid".into()))?;
  let cache_until = until.checked_add_signed(cache_ttl).ok_or_else(|| worker::Error::RustError("Departure cache horizon is out of range".into()))?;
  let departures = next_departures(&templates, params.when, cache_until, MAX_LIMIT).map_err(|message| worker::Error::RustError(message.into()))?;
  let entry = cache::Entry { stop, from: params.when, until: cache_until, departures };
  let mut cache_status = "BYPASS";
  if let Some(state) = &state
    && !templates.is_empty()
  {
    match cache::write(db, &params, state, now, cache_ttl_seconds, &entry).await {
      Ok(()) => cache_status = "MISS",
      Err(error) => worker::console_error!("Departure cache write failed: {}", error),
    }
  }
  let departures = entry.select(params.when, until, params.limit).ok_or_else(|| worker::Error::RustError("Incomplete departure results".into()))?;
  response(&params, &entry.stop, departures, cache_status)
}

fn response(params: &Parameters, stop: &Stop, departures: Vec<Departure>, cache_status: &str) -> Result<Response> {
  let method = departures.first().map(|departure| departure.estimate_method.as_str());
  let method = if departures.iter().all(|departure| Some(departure.estimate_method.as_str()) == method) { method } else { Some("mixed") };
  let mut response = Response::from_json(&json!({
    "stop": stop,
    "route_id": params.route_id,
    "direction_id": params.direction_id,
    "requested_at": params.when,
    "search_until": params.when + Duration::days(schedule::SEARCH_DAYS),
    "limit": params.limit,
    "is_estimate": true,
    "estimate_method": method,
    "realtime": false,
    "departures": departures,
  }))?;
  response.headers_mut().set("Cache-Control", "no-store")?;
  response.headers_mut().set("X-Departure-Cache", cache_status)?;
  Ok(response)
}
