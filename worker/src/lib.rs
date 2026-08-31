//! GTFS Worker — Cloudflare Worker API for GTFS static data.
//!
//! Note: Heavy GTFS import processing (ZIP download, CSV parsing) has been
//! moved to an external GitHub Actions workflow to respect Cloudflare Worker CPU limits.

use worker::{Cache, Context, Env, Method, Request, Response, Result, event};

#[event(fetch)]
pub async fn fetch_route(req: Request, env: Env, ctx: Context) -> Result<Response> {
  console_error_panic_hook::set_once();
  let url = req.url()?;

  let path = url.path();
  let mut segments = path.trim_matches('/').split('/');

  match (segments.next(), segments.next(), segments.next()) {
    (Some(provider), Some("status"), None) => {
      if req.method() != Method::Get {
        return Response::error("Method Not Allowed", 405);
      }

      // Query strings, path casing, and a trailing slash do not alter status
      // output; canonicalize them so callers cannot create arbitrary cache keys.
      let mut cache_url = url.clone();
      cache_url.set_path(&format!("/{}/status", provider.to_ascii_lowercase()));
      cache_url.set_query(None);
      let cache_key = cache_url.to_string();
      let cache = Cache::default();
      if let Ok(Some(response)) = cache.get(&cache_key, false).await {
        return Ok(response);
      }

      let binding_name = format!("DB_{}", provider.to_uppercase().replace("-", "_"));
      let d1 = match env.d1(&binding_name) {
        Ok(db) => db,
        Err(_) => return Response::error(format!("Provider '{}' not found or DB not bound", provider), 404),
      };

      // Fetch detailed import progress
      let progress_results = match d1.prepare("SELECT * FROM import_progress").all().await {
        Ok(res) => res,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      let mut details = Vec::new();
      // In a real scenario we'd define a proper struct, using dynamic JSON for simplicity here
      if let Ok(results) = progress_results.results::<serde_json::Value>() {
        details = results;
      }

      let mut response = Response::from_json(&details)?;
      response.headers_mut().set("Cache-Control", "public, max-age=60")?;
      let cached_response = response.cloned()?;
      ctx.wait_until(async move {
        let _ = Cache::default().put(cache_key, cached_response).await;
      });
      Ok(response)
    }

    (Some(""), None, None) | (None, None, None) => Response::ok("Worker is running. Use /<provider>/status to check GTFS import progress."),

    _ => Response::error("Not Found", 404),
  }
}
