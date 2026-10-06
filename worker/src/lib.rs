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

use worker::{Context, Env, Method, Request, Response, Result, event};

const DEFAULT_MAX_ROWS: u32 = 1000;
const CORS_ALLOW_ORIGIN: &str = "*";

fn max_rows(env: &Env) -> u32 {
  env.var("WORKER_SQL_MAX_ROWS").ok().and_then(|value| value.to_string().trim().parse::<u32>().ok()).filter(|rows| *rows > 0).unwrap_or(DEFAULT_MAX_ROWS)
}

#[event(fetch)]
pub async fn fetch_route(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
  console_error_panic_hook::set_once();
  let url = req.url()?;

  let path = url.path();
  let mut segments = path.trim_matches('/').split('/');

  let mut response = if req.method() == Method::Options {
    let mut response = Response::empty()?.with_status(204);
    response.headers_mut().set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")?;
    response.headers_mut().set("Access-Control-Allow-Headers", "Content-Type")?;
    response
  } else {
    match (segments.next(), segments.next(), segments.next()) {
      (Some(provider), Some("status"), None) => status::handle(&req, &env, ctx, &url, provider).await,
      (Some(provider), Some("departures"), None) => departures::handle(&req, &env, &url, provider).await,
      (Some(provider), Some("map"), None) => map::handle(&req, &env, ctx, &url, provider).await,
      (Some(provider), Some("sql"), None) => sql::handle(&mut req, &env, provider).await,
      (Some(provider), Some("data"), Some(table_name)) => data::handle(&env, &url, provider, table_name).await,
      (Some(""), None, None) | (None, None, None) => Response::ok("Worker is running. Use /<provider>/status to check GTFS import progress."),
      _ => Response::error("Not Found", 404),
    }?
  };
  response.headers_mut().set("Access-Control-Allow-Origin", CORS_ALLOW_ORIGIN)?;
  Ok(response)
}
