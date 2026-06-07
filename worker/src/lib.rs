//! GTFS Worker — Cloudflare Worker API for GTFS static data.
//!
//! Note: Heavy GTFS import processing (ZIP download, CSV parsing) has been
//! moved to an external GitHub Actions workflow to respect Cloudflare Worker CPU limits.

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
pub async fn fetch_route(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    let url = req.url()?;

    worker::console_log!("Received {} request to {}", req.method(), url.path());

    let path = url.path();
    let mut segments = path.trim_matches('/').split('/');

    match (segments.next(), segments.next()) {
        (Some(provider), Some("status")) => {
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

            Response::from_json(&details)
        }

        (Some(""), None) | (None, None) => Response::ok("Worker is running. Use /<provider>/status to check GTFS import progress."),

        _ => Response::error("Not Found", 404),
    }
}
