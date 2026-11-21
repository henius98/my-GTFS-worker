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

use chrono_tz::Tz;
use worker::{Context, Env, Method, Request, Response, Result, event};

pub(crate) const MAX_IDENTIFIER_BYTES: usize = 256;
pub(crate) const MAX_TTL_SECONDS: i64 = i64::MAX / 1_000;

pub(crate) fn is_d1_row_read_limit(message: &str) -> bool {
  message.to_ascii_lowercase().contains("daily row read limit")
}

pub(crate) fn d1_row_read_limit_response() -> Result<Response> {
  Ok(
    Response::from_json(&serde_json::json!({
      "error": "Database daily row read limit exceeded. Please try again after midnight UTC."
    }))?
    .with_status(429),
  )
}

pub(crate) fn required_var(env: &Env, name: &str) -> Result<String> {
  Ok(env.var(name).map_err(|_| worker::Error::RustError(format!("{name} must be set")))?.to_string())
}

fn parse_non_empty(value: &str) -> Option<&str> {
  let trimmed = value.trim();
  (!trimmed.is_empty()).then_some(trimmed)
}

pub(crate) fn parse_positive_u32(value: &str) -> Option<u32> {
  value.trim().parse::<u32>().ok().filter(|number| *number > 0)
}

pub(crate) fn parse_positive_usize(value: &str) -> Option<usize> {
  value.trim().parse::<usize>().ok().filter(|number| *number > 0)
}

pub(crate) fn parse_positive_i64(value: &str, max: i64) -> Option<i64> {
  value.trim().parse::<i64>().ok().filter(|number| (1..=max).contains(number))
}

pub(crate) fn parse_non_negative_i64(value: &str, max: i64) -> Option<i64> {
  value.trim().parse::<i64>().ok().filter(|number| (0..=max).contains(number))
}

pub(crate) fn parse_timezone(value: &str) -> Option<&str> {
  let trimmed = value.trim();
  (!trimmed.is_empty() && trimmed.parse::<Tz>().is_ok()).then_some(trimmed)
}

fn cors_allow_origin(env: &Env) -> Result<String> {
  let raw = required_var(env, "WORKER_CORS_ALLOW_ORIGIN")?;
  parse_non_empty(&raw).map(str::to_owned).ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_CORS_ALLOW_ORIGIN={raw:?}: must not be empty")))
}

pub(crate) fn max_rows(env: &Env) -> Result<u32> {
  let raw = required_var(env, "WORKER_SQL_MAX_ROWS")?;
  parse_positive_u32(&raw).ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_SQL_MAX_ROWS={raw:?}: must be a positive integer")))
}

pub(crate) fn api_cache_control(env: &Env) -> Result<String> {
  let raw = required_var(env, "WORKER_API_CACHE_TTL_SECONDS")?;
  let seconds = parse_non_negative_i64(&raw, MAX_TTL_SECONDS).ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_API_CACHE_TTL_SECONDS={raw:?}: must be a non-negative integer")))?;
  Ok(format!("public, max-age={seconds}"))
}

pub(crate) fn feed_cache_ttl_seconds(env: &Env) -> Result<i64> {
  let raw = required_var(env, "WORKER_FEED_CACHE_TTL_SECONDS")?;
  parse_positive_i64(&raw, MAX_TTL_SECONDS).ok_or_else(|| worker::Error::RustError(format!("invalid WORKER_FEED_CACHE_TTL_SECONDS={raw:?}: must be a positive integer")))
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
        if is_d1_row_read_limit(&err.to_string()) {
          worker::console_warn!("D1 row read limit exceeded: {err}");
          d1_row_read_limit_response()?
        } else {
          worker::console_error!("Unhandled route error: {err}");
          Response::error("Internal Server Error", 500)?
        }
      }
    }
  };
  let headers = response.headers().clone();
  headers.set("Access-Control-Allow-Origin", &cors_origin)?;
  Ok(response.with_headers(headers))
}

#[cfg(test)]
#[path = "../../test/unit/worker/lib.rs"]
mod tests;
