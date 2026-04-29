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
    let d1 = env.d1("DB")?;

    // ── Check/Acquire distributed lock ──────────────────────────────────────
    let lock_query = "
        UPDATE sync_status 
        SET IsRunning = 1, LastStarted = CURRENT_TIMESTAMP 
        WHERE Id = 'gtfs_import' 
          AND (IsRunning = 0 OR LastStarted < datetime('now', '-1 hour'))
    ";
    
    let lock_result = d1.prepare(lock_query).run().await?;
    let changes = match lock_result.meta()? {
        Some(m) => m.changes.unwrap_or(0),
        None => 0,
    };
    
    if changes == 0 {
        logger.warn("Import skipped: Another sync is already in progress.").await;
        return Ok(());
    }

    let result = async {
        let base_url = env.var("GTFS_STATIC_URL")?.to_string();
        let enum_str = env.var("GTFS_STATIC_ENUM")?.to_string();

        let normalized = enum_str.replace(['\n', '\r'], "");
        let items: Vec<&str> = normalized
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        for item in &items {
            let prefix = enum_to_prefix(item);
            let target_url = format!("{base_url}{item}");
            let start_time = worker::Date::now().as_millis();

            let op_result = async {
                // 1. Check existing version in DB to enable conditional GET
                let version: DatasetVersion = d1.prepare("SELECT ETag, LastModified FROM dataset_versions WHERE Prefix = ?")
                    .bind(&[prefix.clone().into()])?
                    .first()
                    .await?
                    .unwrap_or_default();

                logger.info(&format!("Checking for updates: {target_url}")).await;

                let (bytes, new_etag, new_lm) = fetch_gtfs_with_cache(&target_url, &version).await?;

                if bytes.is_empty() {
                    logger.info(&format!("Dataset {item} is up to date (304 Not Modified). Skipping import.")).await;
                    return Ok(());
                }

                logger.info(&format!("Update found. Downloaded ZIP size: {} bytes", bytes.len())).await;

                // Pre-fetch column schemas for this operator
                let schemas = prefetch_table_schemas(&d1, &logger, &prefix).await?;
                let cursor = Cursor::new(bytes);
                let mut archive = ZipArchive::new(cursor).map_err(to_worker_err)?;

                // 2. Initialize ImportContext to track active IDs (Save D1 Writes)
                let mut ctx = ImportContext::default();

                for schema in &schemas {
                    let table_name = format!("{prefix}_{}", schema.base_name);
                    process_csv_file(&d1, &logger, &mut archive, schema, &table_name, &mut ctx).await?;
                }

                // 3. Update version info in DB after successful import
                d1.prepare("INSERT OR REPLACE INTO dataset_versions (Prefix, ETag, LastModified, LastImported) VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)")
                    .bind(&[prefix.into(), new_etag.into(), new_lm.into()])?
                    .run()
                    .await?;

                Ok::<(), worker::Error>(())
            }
            .await;

            let duration = worker::Date::now().as_millis() - start_time;
            if let Err(e) = op_result {
                logger.error(&format!("Skipping dataset {item} due to error: {e} (Took {}ms)", duration)).await;
                continue;
            }
            logger.info(&format!("Finished dataset import for: {item} (Took {}ms)", duration)).await;
        }

        logger.info("Successfully processed all GTFS datasets.").await;
        Ok(())
    }
    .await;

    if let Err(ref e) = result {
        logger.error(&format!("GTFS import failed: {e}")).await;
    }

    // ── Release distributed lock ────────────────────────────────────────────
    let _ = d1.prepare("UPDATE sync_status SET IsRunning = 0, LastFinished = CURRENT_TIMESTAMP WHERE Id = 'gtfs_import'")
        .run()
        .await;

    result
}

/// Downloads a ZIP file with conditional headers (ETag/Last-Modified).
/// Returns (bytes, new_etag, new_last_modified). If bytes is empty, content is unchanged.
async fn fetch_gtfs_with_cache(
    url: &str, 
    version: &DatasetVersion
) -> Result<(Vec<u8>, Option<String>, Option<String>)> {
    let mut headers = worker::Headers::new();
    if let Some(ref etag) = version.ETag {
        headers.set("If-None-Match", etag)?;
    }
    if let Some(ref lm) = version.LastModified {
        headers.set("If-Modified-Since", lm)?;
    }

    let req = Request::new_with_init(
        url,
        &RequestInit {
            method: Method::Get,
            headers,
            ..Default::default()
        },
    )?;

    let mut resp = Fetch::Request(req).send().await?;
    
    if resp.status_code() == 304 {
        return Ok((vec![], None, None));
    }

    let new_etag = resp.headers().get("ETag")?;
    let new_lm = resp.headers().get("Last-Modified")?;
    let bytes = resp.bytes().await?;

    Ok((bytes, new_etag, new_lm))
}
