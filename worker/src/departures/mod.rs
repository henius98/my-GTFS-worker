mod cache;
mod query;
mod schedule;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use schedule::{Departure, next_departures};
use serde::{Deserialize, Serialize};
use worker::{Env, Method, Request, Response, Result, Url};

use crate::database;

const MAX_SEARCH_DAYS: i64 = 3_650;

struct DepartureConfig {
  max_limit: usize,
  default_limit: usize,
  search_days: i64,
  cache_ttl: Duration,
  cache_window_seconds: i64,
  default_timezone: Tz,
}

impl DepartureConfig {
  fn from_env(env: &Env) -> Result<Self> {
    let max_limit = crate::parse_required_var(env, "WORKER_DEPARTURES_MAX_LIMIT", |raw| crate::parse_int_in_range(raw, 1, usize::MAX), "must be a positive integer")?;
    let default_limit = crate::parse_required_var(env, "WORKER_DEPARTURES_DEFAULT_LIMIT", |raw| crate::parse_int_in_range(raw, 1, max_limit), "must be between 1 and WORKER_DEPARTURES_MAX_LIMIT")?;
    let search_days = crate::parse_required_var(env, "WORKER_DEPARTURES_SEARCH_DAYS", |raw| crate::parse_int_in_range(raw, 1, MAX_SEARCH_DAYS), &format!("must be between 1 and {MAX_SEARCH_DAYS}"))?;
    let cache_ttl_seconds = crate::feed_cache_ttl_seconds(env)?;
    let cache_ttl = Duration::try_seconds(cache_ttl_seconds).ok_or_else(|| worker::Error::RustError("Departure cache lifetime is invalid".into()))?;
    let cache_window_seconds =
      crate::parse_required_var(env, "WORKER_DEPARTURE_CACHE_WINDOW_SECONDS", |raw| crate::parse_int_in_range(raw, 0, crate::MAX_TTL_SECONDS), "must be a non-negative integer")?;
    let default_timezone = crate::default_timezone(env)?;

    Ok(Self { max_limit, default_limit, search_days, cache_ttl, cache_window_seconds, default_timezone })
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
    let stop_id = stop_id_raw.ok_or_else(|| "stop_id is required".to_owned())?.trim().to_owned();
    let route_id = route_id_raw.map(|s| s.trim().to_owned());
    for id in std::iter::once(&stop_id).chain(route_id.as_ref()) {
      if id.is_empty() || id.len() > crate::MAX_IDENTIFIER_BYTES {
        return Err(format!("Identifiers must contain 1 to {} bytes", crate::MAX_IDENTIFIER_BYTES));
      }
    }
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
    if when.checked_add_signed(Duration::days(config.search_days) + config.cache_ttl).is_none() {
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

#[derive(Serialize)]
struct DeparturesResponse<'a> {
  stop: &'a Stop,
  route_id: Option<&'a str>,
  direction_id: Option<i32>,
  requested_at: DateTime<Utc>,
  search_until: DateTime<Utc>,
  limit: usize,
  is_estimate: bool,
  estimate_method: Option<&'a str>,
  realtime: bool,
  departures: &'a [Departure],
}

pub async fn handle(req: &Request, env: &Env, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return crate::method_not_allowed("GET", true);
  }
  let now = crate::current_time(env)?;
  let config = DepartureConfig::from_env(env)?;
  let params = match Parameters::parse(url, now, &config) {
    Ok(params) => params,
    Err(message) => return crate::json_error(400, &message),
  };
  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };
  match calculate(&db, provider, params, now, &config).await {
    Ok(response) => Ok(response),
    Err(error) if crate::is_d1_row_read_limit_error(&error) => Err(error),
    Err(error) => {
      worker::console_error!("Departure lookup failed: {}", error);
      crate::json_error(503, "Departure data is unavailable")
    }
  }
}

async fn calculate(db: &worker::D1Database, provider: &str, params: Parameters, now: DateTime<Utc>, config: &DepartureConfig) -> Result<Response> {
  let until = params.when + Duration::days(config.search_days);
  let state = match cache::read_with_state(db, &params, now).await {
    Ok((feed_state, Some(entry))) => {
      if let Some(departures) = entry.select(params.when, until, params.limit, config.max_limit) {
        return response(&params, &entry.stop, &departures, "HIT", until);
      }
      Some(feed_state)
    }
    Ok((feed_state, None)) => Some(feed_state),
    Err(error) if crate::is_d1_row_read_limit_error(&error) => return Err(error),
    Err(error) => {
      worker::console_error!("Departure cache read failed: {}", error);
      None
    }
  };
  let Some((stop, templates)) = query::load_stop_and_templates(db, provider, &params).await? else {
    return crate::json_error(404, "Unknown stop_id");
  };
  // Extend coverage by the cache lifetime so a later request still has a full
  // search horizon, including when this stop has very few departures.
  let cache_until = until.checked_add_signed(config.cache_ttl).ok_or_else(|| worker::Error::RustError("Departure cache horizon is out of range".into()))?;
  let departures = next_departures(&templates, params.when, cache_until, config.max_limit, config.default_timezone.name()).map_err(|message| worker::Error::RustError(message.into()))?;
  let entry = cache::Entry { stop, from: params.when, until: cache_until, departures };
  let mut cache_status = "BYPASS";
  if let Some(state) = &state
    && !templates.is_empty()
    && (params.when - now).num_seconds().abs() < config.cache_window_seconds
  {
    match cache::write(db, &params, state, now, config.cache_ttl, &entry).await {
      Ok(()) => cache_status = "MISS",
      Err(error) => worker::console_error!("Departure cache write failed: {}", error),
    }
  }
  let departures = entry.select(params.when, until, params.limit, config.max_limit).ok_or_else(|| worker::Error::RustError("Incomplete departure results".into()))?;
  response(&params, &entry.stop, &departures, cache_status, until)
}

fn response(params: &Parameters, stop: &Stop, departures: &[Departure], cache_status: &str, search_until: DateTime<Utc>) -> Result<Response> {
  let method = departures.first().map(|departure| departure.estimate_method.as_str());
  let method = if departures.iter().all(|departure| Some(departure.estimate_method.as_str()) == method) { method } else { Some("mixed") };
  let mut response = Response::from_json(&DeparturesResponse {
    stop,
    route_id: params.route_id.as_deref(),
    direction_id: params.direction_id,
    requested_at: params.when,
    search_until,
    limit: params.limit,
    is_estimate: true,
    estimate_method: method,
    realtime: false,
    departures,
  })?;
  response.headers_mut().set("Cache-Control", "no-store")?;
  response.headers_mut().set("X-Departure-Cache", cache_status)?;
  Ok(response)
}
