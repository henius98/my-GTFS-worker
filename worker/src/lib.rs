//! GTFS Worker — Cloudflare Worker API for GTFS static data.
//!
//! Note: Heavy GTFS import processing (ZIP download, CSV parsing) has been
//! moved to an external GitHub Actions workflow to respect Cloudflare Worker CPU limits.

mod data;
mod database;
mod departures;
mod map;
mod sql;
mod status;

use std::error::Error as StdError;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::json;
use worker::{Cache, Context, Date, Env, Method, Request, Response, Result, Url, event};

pub(crate) const MAX_IDENTIFIER_BYTES: usize = 256;
pub(crate) const MAX_TTL_SECONDS: i64 = i64::MAX / 1_000;

pub(crate) fn is_d1_row_read_limit(message: &str) -> bool {
  message.to_ascii_lowercase().contains("daily row read limit")
}

pub(crate) fn is_d1_row_read_limit_error(err: &worker::Error) -> bool {
  let mut current: Option<&(dyn StdError + 'static)> = Some(err);
  while let Some(source) = current {
    if is_d1_row_read_limit(&source.to_string()) {
      return true;
    }
    current = source.source();
  }
  false
}

pub(crate) fn json_error(status: u16, message: &str) -> Result<Response> {
  Ok(Response::from_json(&json!({ "error": message }))?.with_status(status))
}

pub(crate) fn method_not_allowed(allow: &str, json: bool) -> Result<Response> {
  let mut response = if json { json_error(405, "Method not allowed")? } else { Response::error("Method Not Allowed", 405)? };
  response.headers_mut().set("Allow", allow)?;
  Ok(response)
}

pub(crate) fn d1_row_read_limit_response() -> Result<Response> {
  json_error(429, "Database daily row read limit exceeded. Please try again after midnight UTC.")
}

pub(crate) fn canonical_cache_url(url: &Url, provider: &str, route: &str) -> Url {
  let mut cache_url = url.clone();
  cache_url.set_path(&format!("/{}/{route}", provider.to_ascii_lowercase()));
  cache_url.set_query(None);
  cache_url
}

pub(crate) fn cache_response(ctx: &Context, cache_key: String, response: &mut Response, cache_control: &str) -> Result<()> {
  response.headers_mut().set("Cache-Control", cache_control)?;
  let cached_response = response.cloned()?;
  ctx.wait_until(async move {
    let _ = Cache::default().put(cache_key, cached_response).await;
  });
  Ok(())
}

pub(crate) fn parse_required_var<T>(env: &Env, name: &str, parse: impl FnOnce(&str) -> Option<T>, requirement: &str) -> Result<T> {
  let raw = env.var(name).map_err(|_| worker::Error::RustError(format!("{name} must be set")))?.to_string();
  parse(&raw).ok_or_else(|| worker::Error::RustError(format!("invalid {name}={raw:?}: {requirement}")))
}

fn parse_non_empty(value: &str) -> Option<&str> {
  let trimmed = value.trim();
  (!trimmed.is_empty()).then_some(trimmed)
}

pub(crate) fn parse_int_in_range<T: FromStr + Ord>(value: &str, min: T, max: T) -> Option<T> {
  value.trim().parse::<T>().ok().filter(|number| (min..=max).contains(number))
}

pub(crate) fn parse_timezone(value: &str) -> Option<Tz> {
  let trimmed = value.trim();
  (!trimmed.is_empty()).then(|| trimmed.parse::<Tz>().ok()).flatten()
}

pub(crate) fn current_time(env: &Env) -> Result<DateTime<Utc>> {
  env
    .var("WORKER_TEST_NOW")
    .ok()
    .and_then(|v| DateTime::parse_from_rfc3339(&v.to_string()).ok())
    .map(|dt| dt.with_timezone(&Utc))
    .or_else(|| DateTime::from_timestamp_millis(Date::now().as_millis() as i64))
    .ok_or_else(|| worker::Error::RustError("Invalid system time".into()))
}

pub(crate) fn default_timezone(env: &Env) -> Result<Tz> {
  parse_required_var(env, "WORKER_DEFAULT_TIMEZONE", parse_timezone, "must be a valid IANA timezone")
}

fn cors_allow_origin(env: &Env) -> Result<String> {
  parse_required_var(env, "WORKER_CORS_ALLOW_ORIGIN", |raw| parse_non_empty(raw).map(str::to_owned), "must not be empty")
}

pub(crate) fn max_rows(env: &Env) -> Result<u32> {
  parse_required_var(env, "WORKER_SQL_MAX_ROWS", |raw| parse_int_in_range(raw, 1, u32::MAX), "must be a positive integer")
}

pub(crate) fn api_cache_control(env: &Env) -> Result<String> {
  let seconds = parse_required_var(env, "WORKER_API_CACHE_TTL_SECONDS", |raw| parse_int_in_range(raw, 0, MAX_TTL_SECONDS), "must be a non-negative integer")?;
  Ok(format!("public, max-age={seconds}"))
}

pub(crate) fn feed_cache_ttl_seconds(env: &Env) -> Result<i64> {
  parse_required_var(env, "WORKER_FEED_CACHE_TTL_SECONDS", |raw| parse_int_in_range(raw, 1, MAX_TTL_SECONDS), "must be a positive integer")
}

pub(crate) fn feed_cache_control(env: &Env) -> Result<String> {
  Ok(format!("public, max-age={}", feed_cache_ttl_seconds(env)?))
}

#[event(fetch)]
pub async fn fetch_route(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
  console_error_panic_hook::set_once();
  let cors_origin = cors_allow_origin(&env)?;
  let url = req.url()?;

  let path = url.path();
  let mut segments = path.trim_matches('/').split('/');

  let response = if req.method() == Method::Options {
    let mut response = Response::empty()?.with_status(204);
    response.headers_mut().set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")?;
    response.headers_mut().set("Access-Control-Allow-Headers", "Content-Type, Authorization, X-Admin-Key")?;
    response
  } else {
    let result = match (segments.next(), segments.next(), segments.next(), segments.next()) {
      (Some(provider), Some("status"), None, None) => status::handle(&req, &env, ctx, &url, provider).await,
      (Some(provider), Some("departures"), None, None) => departures::handle(&req, &env, &url, provider).await,
      (Some(provider), Some("map"), None, None) => map::handle(&req, &env, ctx, &url, provider).await,
      (Some(provider), Some("sql"), None, None) => sql::handle(&mut req, &env, provider).await,
      (Some(provider), Some("data"), Some(table_name), None) => data::handle(&req, &env, &url, provider, table_name).await,
      (Some(""), None, None, None) | (None, None, None, None) => Response::ok("Worker is running. Use /<provider>/status to check GTFS import progress."),
      _ => Response::error("Not Found", 404),
    };
    match result {
      Ok(response) => response,
      Err(err) => {
        if is_d1_row_read_limit_error(&err) {
          worker::console_warn!("D1 row read limit exceeded: {err}");
          d1_row_read_limit_response()?
        } else {
          worker::console_error!("Unhandled route error: {err}");
          Response::error("Internal Server Error", 500)?
        }
      }
    }
  };
  // Clone headers because responses retrieved from Cache::default() have immutable JS headers.
  let headers = response.headers().clone();
  headers.set("Access-Control-Allow-Origin", &cors_origin)?;
  Ok(response.with_headers(headers))
}

#[cfg(test)]
#[path = "../../test/unit/worker/lib.rs"]
mod tests;
