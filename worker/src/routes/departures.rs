mod schedule;

use chrono::{DateTime, Duration, Utc};
use schedule::{Template, next_departures};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use worker::wasm_bindgen::JsValue;
use worker::{Date, Env, Method, Request, Response, Result, Url};

use crate::database;

const QUERY: &str = include_str!("departures/query.sql");
const MAX_LIMIT: usize = 100;

struct Parameters {
  stop_id: String,
  route_id: Option<String>,
  when: DateTime<Utc>,
  direction_id: Option<i32>,
  limit: usize,
}

impl Parameters {
  fn parse(url: &Url, now: DateTime<Utc>) -> std::result::Result<Self, &'static str> {
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
    if when.checked_add_signed(Duration::days(schedule::SEARCH_DAYS)).is_none() {
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
  let params = match Parameters::parse(url, now) {
    Ok(params) => params,
    Err(message) => return json_error(400, message),
  };
  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };
  match query(&db, params).await {
    Ok(response) => Ok(response),
    Err(error) => {
      worker::console_error!("Departure lookup failed: {}", error);
      json_error(503, "Departure data is unavailable")
    }
  }
}

async fn query(db: &worker::D1Database, params: Parameters) -> Result<Response> {
  let supported = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='frequencies'").first::<String>(Some("name")).await?;
  if supported.is_none() {
    return json_error(404, "Frequency departures are unavailable for this provider");
  }
  let stop = db.prepare("SELECT stop_id, stop_name FROM stops WHERE stop_id = ?1").bind(&[params.stop_id.clone().into()])?.first::<Stop>(None).await?;
  let Some(stop) = stop else {
    return json_error(404, "Unknown stop_id");
  };
  let result = db
    .prepare(QUERY)
    .bind(&[params.stop_id.clone().into(), params.route_id.clone().map(JsValue::from).unwrap_or(JsValue::NULL), params.direction_id.map(JsValue::from).unwrap_or(JsValue::NULL)])?
    .all()
    .await?;
  if !result.success() {
    return json_error(503, "Departure data is unavailable");
  }
  // worker 0.8.x unwraps typed D1 deserialization internally. Decode plain JSON
  // first so nullable/malformed feed fields return an error instead of a panic.
  let templates = result.results::<serde_json::Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<Template>, _>>()?;
  let departures = next_departures(&templates, params.when, params.limit).map_err(|message| worker::Error::RustError(message.into()))?;
  let mut response = Response::from_json(&json!({
    "stop": stop,
    "route_id": params.route_id,
    "direction_id": params.direction_id,
    "requested_at": params.when,
    "search_until": params.when + Duration::days(schedule::SEARCH_DAYS),
    "limit": params.limit,
    "is_estimate": true,
    "estimate_method": "frequency_start_plus_headway",
    "realtime": false,
    "departures": departures,
  }))?;
  response.headers_mut().set("Cache-Control", "no-store")?;
  Ok(response)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
  use super::*;

  #[test]
  fn validates_request_before_querying() {
    let now = "2026-09-23T00:00:00Z".parse().unwrap();
    for query in [
      "",
      "stop_id=",
      "stop_id=A&limit=0",
      "stop_id=A&limit=-1",
      "stop_id=A&limit=101",
      "stop_id=A&limit=1.5",
      "stop_id=A&limit=",
      "stop_id=A&direction_id=2",
      "stop_id=A&at=2026-09-23T08:00:00",
      "stop_id=A&stop_id=B",
      "stop_id=A&api_key=secret",
      "stop_id=A&route_id=",
    ] {
      let url = Url::parse(&format!("https://example.com/departures?{query}")).unwrap();
      assert!(Parameters::parse(&url, now).is_err(), "{query}");
    }
    let url = Url::parse("https://example.com/departures?stop_id=KJ10&route_id=KJL&direction_id=1&limit=100&at=2026-09-23T08:00:00%2B08:00").unwrap();
    let params = Parameters::parse(&url, now).unwrap();
    assert_eq!(params.when, now);
    assert_eq!(params.limit, 100);
    assert_eq!(params.route_id.as_deref(), Some("KJL"));
  }
}
