//! GTFS Worker — Cloudflare Worker API for GTFS static data.
//!
//! Note: Heavy GTFS import processing (ZIP download, CSV parsing) has been
//! moved to an external GitHub Actions workflow to respect Cloudflare Worker CPU limits.

mod database;
mod routes;

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
pub async fn fetch_route(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
  console_error_panic_hook::set_once();
  let url = req.url()?;

  let path = url.path();
  let mut segments = path.trim_matches('/').split('/');

  match (segments.next(), segments.next(), segments.next()) {
    (Some(provider), Some("status"), None) => routes::status::handle(&req, &env, ctx, &url, provider).await,
    (Some(provider), Some("sql"), None) => routes::sql::handle(&mut req, &env, provider).await,
    (Some(provider), Some("data"), Some(table_name)) => routes::data::handle(&env, &url, provider, table_name).await,
    (Some(""), None, None) | (None, None, None) => Response::ok("Worker is running. Use /<provider>/status to check GTFS import progress."),
    _ => Response::error("Not Found", 404),
  }
}
