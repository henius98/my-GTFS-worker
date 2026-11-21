mod cache;
mod query;
mod schedule;

use chrono::{DateTime, Duration, Utc};
use schedule::{Departure, next_departures};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::{Date, Env, Method, Request, Response, Result, Url};

use crate::database;

const MAX_SEARCH_DAYS: i64 = 3_650;

struct DepartureConfig {
  max_limit: usize,
  default_limit: usize,
  search_days: i64,
  cache_ttl_seconds: i64,
  cache_window_seconds: i64,
  default_timezone: String,
}

impl DepartureConfig {
  fn from_env(env: &Env) -> Result<Self> {
    let raw_max_limit = crate::required_var(env, "WORKER_DEPARTURES_MAX_LIMIT")?;
    let max_limit =
      crate::parse_positive_usize(&raw_max_limit).ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_DEPARTURES_MAX_LIMIT={raw_max_limit:?}: must be a positive integer")))?;

    let raw_default_limit = crate::required_var(env, "WORKER_DEPARTURES_DEFAULT_LIMIT")?;
    let default_limit = crate::parse_positive_usize(&raw_default_limit)
      .filter(|limit| *limit <= max_limit)
      .ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_DEPARTURES_DEFAULT_LIMIT={raw_default_limit:?}: must be between 1 and WORKER_DEPARTURES_MAX_LIMIT")))?;

    let raw_search_days = crate::required_var(env, "WORKER_DEPARTURES_SEARCH_DAYS")?;
    let search_days = crate::parse_positive_i64(&raw_search_days, MAX_SEARCH_DAYS)
      .ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_DEPARTURES_SEARCH_DAYS={raw_search_days:?}: must be between 1 and {MAX_SEARCH_DAYS}")))?;

    let cache_ttl_seconds = cache::ttl_seconds(env)?;

    let raw_cache_window = crate::required_var(env, "WORKER_DEPARTURE_CACHE_WINDOW_SECONDS")?;
    let cache_window_seconds = crate::parse_non_negative_i64(&raw_cache_window, crate::MAX_TTL_SECONDS)
      .ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_DEPARTURE_CACHE_WINDOW_SECONDS={raw_cache_window:?}: must be a non-negative integer")))?;

    let raw_timezone = crate::required_var(env, "WORKER_DEFAULT_TIMEZONE")?;
    let default_timezone =
      crate::parse_timezone(&raw_timezone).map(str::to_owned).ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_DEFAULT_TIMEZONE={raw_timezone:?}: must be a valid IANA timezone")))?;

    Ok(Self { max_limit, default_limit, search_days, cache_ttl_seconds, cache_window_seconds, default_timezone })
  }
}

struct Parameters {
  stop_id: String,
  route_id: Option<String>,
  when: DateTime<Utc>,
  direction_id: Option<i32>,
  limit: usize,
}

impl Parameters {
  fn parse(url: &Url, now: DateTime<Utc>, config: &DepartureConfig) -> std::result::Result<Self, String> {
    let mut stop_id_raw = None;
    let mut route_id_raw = None;
    let mut direction_id_raw = None;
    let mut limit_raw = None;
    let mut at_raw = None;

    for (key, value) in url.query_pairs() {
      let slot = match key.as_ref() {
        "stop_id" => &mut stop_id_raw,
        "route_id" => &mut route_id_raw,
        "direction_id" => &mut direction_id_raw,
        "limit" => &mut limit_raw,
        "at" => &mut at_raw,
        _ => return Err("Unknown query parameter".into()),
      };
      if slot.replace(value.into_owned()).is_some() {
        return Err("Duplicate query parameter".into());
      }
    }
    let stop_id = stop_id_raw.ok_or_else(|| "stop_id is required".to_owned())?;
    let route_id = route_id_raw.map(|s| s.trim().to_owned());
    for id in std::iter::once(&stop_id).chain(route_id.as_ref()) {
      if id.trim().is_empty() || id.len() > crate::MAX_IDENTIFIER_BYTES {
        return Err(format!("Identifiers must contain 1 to {} bytes", crate::MAX_IDENTIFIER_BYTES));
      }
    }
    let stop_id = stop_id.trim().to_owned();
    let direction_id = match direction_id_raw.as_deref() {
      None => None,
      Some("0") => Some(0),
      Some("1") => Some(1),
      _ => return Err("direction_id must be 0 or 1".into()),
    };
    let limit = match limit_raw {
      Some(value) if value.bytes().all(|b| b.is_ascii_digit()) => value.parse().ok(),
      Some(_) => None,
      None => Some(config.default_limit),
    }
    .filter(|n| (1..=config.max_limit).contains(n))
    .ok_or_else(|| format!("limit must be between 1 and {}", config.max_limit))?;
    let when = match at_raw {
      Some(value) => DateTime::parse_from_rfc3339(&value).map_err(|_| "at must be RFC3339 with a timezone, e.g. 2026-09-23T08:00:00Z".to_owned())?.with_timezone(&Utc),
      None => now,
    };
    let cache_ttl = Duration::try_seconds(config.cache_ttl_seconds).ok_or_else(|| "Departure cache lifetime is invalid".to_owned())?;
    if when.checked_add_signed(Duration::days(config.search_days) + cache_ttl).is_none() {
      return Err("at is outside the supported date range".into());
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
  let now = env
    .var("WORKER_TEST_NOW")
    .ok()
    .and_then(|v| DateTime::parse_from_rfc3339(&v.to_string()).ok())
    .map(|dt| dt.with_timezone(&Utc))
    .or_else(|| DateTime::from_timestamp_millis(Date::now().as_millis() as i64))
    .ok_or_else(|| worker::Error::RustError("Invalid system time".into()))?;
  let config = DepartureConfig::from_env(env)?;
  let params = match Parameters::parse(url, now, &config) {
    Ok(params) => params,
    Err(message) => return json_error(400, &message),
  };
  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };
  match calculate(&db, provider, params, now, &config).await {
    Ok(response) => Ok(response),
    Err(error) => {
      worker::console_error!("Departure lookup failed: {}", error);
      json_error(503, "Departure data is unavailable")
    }
  }
}

async fn calculate(db: &worker::D1Database, provider: &str, params: Parameters, now: DateTime<Utc>, config: &DepartureConfig) -> Result<Response> {
  let until = params.when + Duration::days(config.search_days);
  let mut state = match cache::feed_state(db).await {
    Ok(state) => Some(state),
    Err(error) => {
      worker::console_error!("Departure cache import state unavailable: {}", error);
      None
    }
  };
  if let Some(feed_state) = &state {
    match cache::read(db, &params, feed_state, now).await {
      Ok(Some(entry)) => {
        if let Some(departures) = entry.select(params.when, until, params.limit, config.max_limit) {
          return response(&params, &entry.stop, departures, "HIT", config.search_days);
        }
      }
      Ok(None) => {}
      Err(error) => {
        worker::console_error!("Departure cache read failed: {}", error);
        state = None;
      }
    }
  }
  let stop = db.prepare(include_str!("sql/stop.sql")).bind(&[params.stop_id.clone().into()])?.first::<Stop>(None).await?;
  let Some(stop) = stop else {
    return json_error(404, "Unknown stop_id");
  };
  let templates = query::load(db, provider, &params).await?;
  // Extend coverage by the cache lifetime so a later request still has a full
  // search horizon, including when this stop has very few departures.
  let cache_ttl = Duration::try_seconds(config.cache_ttl_seconds).ok_or_else(|| worker::Error::RustError("Departure cache lifetime is invalid".into()))?;
  let cache_until = until.checked_add_signed(cache_ttl).ok_or_else(|| worker::Error::RustError("Departure cache horizon is out of range".into()))?;
  let departures = next_departures(&templates, params.when, cache_until, config.max_limit, &config.default_timezone).map_err(|message| worker::Error::RustError(message.into()))?;
  let entry = cache::Entry { stop, from: params.when, until: cache_until, departures };
  let mut cache_status = "BYPASS";
  if let Some(state) = &state
    && !templates.is_empty()
    && (params.when - now).num_seconds().abs() < config.cache_window_seconds
  {
    match cache::write(db, &params, state, now, config.cache_ttl_seconds, &entry).await {
      Ok(()) => cache_status = "MISS",
      Err(error) => worker::console_error!("Departure cache write failed: {}", error),
    }
  }
  let departures = entry.select(params.when, until, params.limit, config.max_limit).ok_or_else(|| worker::Error::RustError("Incomplete departure results".into()))?;
  response(&params, &entry.stop, departures, cache_status, config.search_days)
}

fn response(params: &Parameters, stop: &Stop, departures: Vec<Departure>, cache_status: &str, search_days: i64) -> Result<Response> {
  let method = departures.first().map(|departure| departure.estimate_method.as_str());
  let method = if departures.iter().all(|departure| Some(departure.estimate_method.as_str()) == method) { method } else { Some("mixed") };
  let mut response = Response::from_json(&json!({
    "stop": stop,
    "route_id": params.route_id,
    "direction_id": params.direction_id,
    "requested_at": params.when,
    "search_until": params.when + Duration::days(search_days),
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
