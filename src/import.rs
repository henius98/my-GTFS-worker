//! Import orchestration: downloads GTFS ZIP archives and dispatches CSV processing.

use std::io::Cursor;
use worker::{Env, Fetch, Method, Request, RequestInit, Result};
use zip::ZipArchive;

use crate::config::*;
use crate::logger::Logger;
use crate::processor::process_csv_file;
use crate::schema::prefetch_table_schemas;

/// Core import pipeline: fetches all GTFS datasets and imports each into D1.
///
/// Each operator enum maps to a table prefix (e.g., "ktmb" → `ktmb_routes`).
/// Column schemas are pre-fetched once from the first operator's tables and
/// reused across all operators (since they share identical structures).
pub async fn import_gtfs(env: &Env) -> Result<()> {
    let logger = Logger::new(env.d1("DB").ok());

    let result = async {
        let d1 = env.d1("DB")?;
        let base_url = env.var("GTFS_STATIC_URL")?.to_string();
        let enum_str = env.var("GTFS_STATIC_ENUM")?.to_string();

        let normalized = enum_str.replace(['\n', '\r'], "");
        let items: Vec<&str> = normalized
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        if items.is_empty() {
            logger.warn("GTFS_STATIC_ENUM is empty, nothing to import.");
            return Ok(());
        }

    // Pre-fetch column schemas using the first operator's tables as reference.
    // All operators share identical table structures, so one set of PRAGMAs suffices.
        let first_prefix = enum_to_prefix(items[0]);
        let schemas = prefetch_table_schemas(&d1, &logger, &first_prefix).await?;

        for item in &items {
            let prefix = enum_to_prefix(item);
            let target_url = format!("{base_url}{item}");
            logger.info(&format!("Downloading GTFS data from: {target_url}"));

            let bytes = fetch_zip_bytes(&target_url).await?;
            logger.info(&format!("Downloaded ZIP size: {} bytes", bytes.len()));

            let cursor = Cursor::new(bytes);
            let mut archive = ZipArchive::new(cursor).map_err(to_worker_err)?;

        // Process each GTFS table with the operator-prefixed table name
            for schema in &schemas {
                let table_name = format!("{prefix}_{}", schema.base_name);
                process_csv_file(&d1, &logger, &mut archive, schema, &table_name).await?;
            }

            logger.info(&format!("Finished dataset import for: {item}"));
        }

        logger.info("Successfully imported all GTFS data into D1.");
        Ok(())
    }
    .await;

    // Catch and log any errors that bubbled up from the `?` operators
    if let Err(ref e) = result {
        logger.error(&format!("GTFS import failed: {e}"));
    }

    result
}

/// Downloads a ZIP file from the given URL and returns the raw bytes.
async fn fetch_zip_bytes(url: &str) -> Result<Vec<u8>> {
    let req = Request::new_with_init(
        url,
        &RequestInit {
            method: Method::Get,
            ..Default::default()
        },
    )?;

    Fetch::Request(req).send().await?.bytes().await
}
