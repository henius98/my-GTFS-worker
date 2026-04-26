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
/// Column schemas are pre-fetched for each operator to account for structural
/// variations (e.g., missing tables or optional columns) between datasets.
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
            logger.warn("GTFS_STATIC_ENUM is empty, nothing to import.").await;
            return Ok(());
        }

        for item in &items {
            let prefix = enum_to_prefix(item);
            let target_url = format!("{base_url}{item}");

            // Wrap each operator's import in an async block to catch errors locally.
            // This ensures that a failure in one dataset (e.g. network timeout)
            // does not prevent other datasets from being imported.
            let op_result = async {
                logger.info(&format!("Downloading GTFS data from: {target_url}")).await;

                // Pre-fetch column schemas specifically for this operator's prefix.
                let schemas = prefetch_table_schemas(&d1, &logger, &prefix).await?;

                let bytes = fetch_zip_bytes(&target_url).await?;
                logger.info(&format!("Downloaded ZIP size: {} bytes", bytes.len())).await;

                let cursor = Cursor::new(bytes);
                let mut archive = ZipArchive::new(cursor).map_err(to_worker_err)?;

                // Process each GTFS table with the operator-prefixed table name
                for schema in &schemas {
                    let table_name = format!("{prefix}_{}", schema.base_name);
                    process_csv_file(&d1, &logger, &mut archive, schema, &table_name).await?;
                }
                Ok::<(), worker::Error>(())
            }
            .await;

            if let Err(e) = op_result {
                logger.error(&format!("Skipping dataset {item} due to error: {e}")).await;
                continue;
            }

            logger.info(&format!("Finished dataset import for: {item}")).await;
        }

        logger.info("Successfully imported all GTFS data into D1.").await;
        Ok(())
    }
    .await;

    // Catch and log any errors that bubbled up from the `?` operators
    if let Err(ref e) = result {
        logger.error(&format!("GTFS import failed: {e}")).await;
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
