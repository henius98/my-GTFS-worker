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
    worker::console_log!("Scheduled GTFS import started.");
    
    match import_gtfs(&env).await {
        Ok(()) => worker::console_log!("Scheduled GTFS import completed successfully."),
        Err(e) => worker::console_error!("Scheduled GTFS import failed: {e}"),
    }
}

#[event(fetch)]
pub async fn fetch_route(
    req: Request,
    env: Env,
    ctx: Context,
) -> Result<Response> {
    console_error_panic_hook::set_once();
    let url = req.url()?;

    match url.path() {
        "/import" => {
            let env_clone = env.clone();
            ctx.wait_until(async move {
                if let Err(e) = import_gtfs(&env_clone).await {
                    worker::console_error!("Background GTFS import failed: {e}");
                }
            });
            Response::ok("GTFS import started in background. Check Cloudflare logs or D1 `logs` table for progress.")
        }

        "/" => {
            Response::ok("Worker is running. Use /import to trigger the database sync.")
        }

        _ => {
            Response::error("Not Found", 404)
        }
    }
}