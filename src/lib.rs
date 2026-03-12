//! GTFS Worker — Scheduled Cloudflare Worker that imports GTFS static data into D1.
//!
//! Module structure:
//!   - `config`    — Constants, types (`TableSchema`, `ColumnInfo`), shared helpers
//!   - `logger`    — Console + D1 logging
//!   - `schema`    — DB schema introspection via PRAGMA table_info
//!   - `import`    — Import orchestration: ZIP download + dataset iteration
//!   - `processor` — CSV parsing, column resolution, bulk INSERT into D1

use worker::{event, Context, Env, Request, Response, Result, ScheduleContext, ScheduledEvent};

mod config;
mod import;
mod logger;
mod processor;
mod schema;

use import::import_gtfs;

// ─── Entry Points ──────────────────────────────────────────────────────────────
#[event(scheduled)]
pub async fn main(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    console_error_panic_hook::set_once();
    let _ = import_gtfs(&env).await;
}

#[event(fetch)]
pub async fn fetch_route(
    req: Request,
    env: Env,
    _ctx: Context,
) -> Result<Response> {
    console_error_panic_hook::set_once();
    let url = req.url()?;

    match url.path() {
        "/import" => {
            match import_gtfs(&env).await {
                Ok(()) => Response::ok("GTFS imported successfully via fetch trigger."),
                Err(e) => Response::error(format!("Error: {e}"), 500),
            }
        }

        "/" => {
            Response::ok("Worker is running. Use /import to trigger the database sync.")
        }

        _ => {
            Response::error("Not Found", 404)
        }
    }
}