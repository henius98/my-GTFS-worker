//! CSV → D1 processing: reads CSV files from ZIP archives, maps columns to the DB schema,
//! and performs bulk multi-row INSERTs into Cloudflare D1.

use std::io::{Cursor, Read};
use serde::Deserialize;
use worker::{D1Database, Result};
use zip::ZipArchive;

use crate::config::*;
use crate::logger::Logger;

// ─── Core CSV Processor ────────────────────────────────────────────────────────

/// Extracts a CSV file from the archive, maps its columns to the pre-fetched DB schema,
/// and bulk-inserts the rows into the specified operator-prefixed table.
pub async fn process_csv_file(
    d1: &D1Database,
    logger: &Logger,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    schema: &TableSchema,
    table_name: &str,
) -> Result<()> {
    let filename = schema.csv_file;

    // ── Extract file from archive ──────────────────────────────────────────
    let mut file = match archive.by_name(filename) {
        Ok(f) => f,
        Err(_) => {
            logger.warn(&format!("{filename} not found in archive, skipping."));
            return Ok(());
        }
    };
    logger.info(&format!("Processing {filename} → `{table_name}`..."));

    let mut content = String::new();
    file.read_to_string(&mut content).map_err(to_worker_err)?;

    // ── Parse CSV & resolve column indices against pre-fetched DB schema ───
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(content.as_bytes());

    let headers = rdr.headers().map_err(to_worker_err)?.clone();

    // Intersect: only import columns that exist in BOTH the DB schema AND the CSV headers
    let (matched_columns, csv_indices) = resolve_matching_columns(&headers, &schema.db_columns);

    if matched_columns.is_empty() {
        logger.warn(&format!("No matching columns between {filename} and `{table_name}`, skipping."));
        return Ok(());
    }

    // Log diagnostics only when there are mismatches (CSV-only or DB-only columns)
    log_column_diagnostics(logger, filename, &headers, &schema.db_columns, &matched_columns);

    // ── Compute sizing and build SQL ───────────────────────────────────────
    let col_count = matched_columns.len();
    let rows_per_insert = if col_count == schema.db_columns.len() {
        schema.rows_per_insert
    } else {
        (D1_MAX_BOUND_PARAMS / col_count).max(1)
    };

    let effective_col_list = if col_count == schema.db_columns.len() {
        schema.col_list.clone()
    } else {
        matched_columns.join(", ")
    };

    let full_insert_sql = build_multi_row_sql(table_name, &effective_col_list, col_count, rows_per_insert);
    let full_stmt = d1.prepare(&full_insert_sql);

    // ── Snapshot row count BEFORE inserts (for new vs. updated tracking) ───
    let count_before = count_rows(d1, table_name).await?;

    // ── Stream CSV rows into multi-row bulk inserts ────────────────────────
    let mut batch_stmts = Vec::with_capacity(D1_MAX_BATCH_STATEMENTS);
    let mut row_param_buf: Vec<worker::wasm_bindgen::JsValue> =
        Vec::with_capacity(rows_per_insert * col_count);
    let mut rows_in_buf: usize = 0;
    let mut count_processed: u32 = 0;

    for result in rdr.records() {
        let record = result.map_err(to_worker_err)?;

        // Append this row's column values to the param buffer
        for &idx in &csv_indices {
            let val = record
                .get(idx)
                .map(worker::wasm_bindgen::JsValue::from_str)
                .unwrap_or_else(worker::wasm_bindgen::JsValue::null);
            row_param_buf.push(val);
        }
        rows_in_buf += 1;
        count_processed += 1;

        // When buffer is full → bind as one multi-row INSERT statement
        if rows_in_buf >= rows_per_insert {
            let bound = full_stmt.clone().bind(&row_param_buf).map_err(to_worker_err)?;
            batch_stmts.push(bound);
            row_param_buf = Vec::with_capacity(rows_per_insert * col_count);
            rows_in_buf = 0;

            // When we have enough statements → flush the batch
            if batch_stmts.len() >= D1_MAX_BATCH_STATEMENTS {
                d1.batch(batch_stmts).await.map_err(to_worker_err)?;
                batch_stmts = Vec::with_capacity(D1_MAX_BATCH_STATEMENTS);
            }
        }
    }

    // ── Handle remaining rows (partial multi-row INSERT) ───────────────────
    if rows_in_buf > 0 {
        let partial_sql = build_multi_row_sql(table_name, &effective_col_list, col_count, rows_in_buf);
        let partial_stmt = d1.prepare(&partial_sql);
        let bound = partial_stmt.bind(&row_param_buf).map_err(to_worker_err)?;
        batch_stmts.push(bound);
    }

    // ── Flush remaining statements ─────────────────────────────────────────
    if !batch_stmts.is_empty() {
        d1.batch(batch_stmts).await.map_err(to_worker_err)?;
    }

    // ── Compute new vs. updated from row count difference ──────────────────
    let count_after = count_rows(d1, table_name).await?;
    let count_new = count_after.saturating_sub(count_before);
    let count_updated = count_processed.saturating_sub(count_new);

    logger.info(&format!(
        "✅ `{table_name}` sync complete: {count_processed} items processed ({count_new} new, {count_updated} updated)."
    ));
    Ok(())
}

// ─── SQL Builder ───────────────────────────────────────────────────────────────

/// Builds a multi-row `INSERT OR REPLACE` SQL with sequentially numbered placeholders.
/// Example output for 3 columns × 2 rows:
/// ```sql
/// INSERT OR REPLACE INTO ktmb_shapes (c1, c2, c3) VALUES (?1, ?2, ?3), (?4, ?5, ?6)
/// ```
pub fn build_multi_row_sql(
    table_name: &str,
    col_list: &str,
    col_count: usize,
    num_rows: usize,
) -> String {
    let row_placeholders: Vec<String> = (0..num_rows)
        .map(|row| {
            let params: Vec<String> = (1..=col_count)
                .map(|col| format!("?{}", row * col_count + col))
                .collect();
            format!("({})", params.join(", "))
        })
        .collect();

    format!(
        "INSERT OR REPLACE INTO {table_name} ({col_list}) VALUES {}",
        row_placeholders.join(", ")
    )
}

// ─── Column Resolution Helpers ─────────────────────────────────────────────────

/// Finds columns that exist in both the CSV headers and the DB schema.
/// Returns the matched column names and their corresponding CSV header indices.
fn resolve_matching_columns(
    headers: &csv::StringRecord,
    db_columns: &[String],
) -> (Vec<String>, Vec<usize>) {
    let mut matched = Vec::new();
    let mut indices = Vec::new();

    for db_col in db_columns {
        if let Some(pos) = headers.iter().position(|h| h == db_col) {
            matched.push(db_col.clone());
            indices.push(pos);
        }
    }

    (matched, indices)
}

/// Logs diagnostic info about mismatched columns between CSV and DB schema.
fn log_column_diagnostics(
    logger: &Logger,
    filename: &str,
    headers: &csv::StringRecord,
    db_columns: &[String],
    matched_columns: &[String],
) {
    let csv_only: Vec<&str> = headers
        .iter()
        .filter(|h| !db_columns.iter().any(|db| db == h))
        .collect();

    let db_only: Vec<&String> = db_columns
        .iter()
        .filter(|db| !headers.iter().any(|h| h == db.as_str()))
        .collect();

    if !csv_only.is_empty() || !db_only.is_empty() {
        let mut msg = format!(
            "Column alignment for {filename} → `{}`  ({} matched):",
            matched_columns.first().map(|_| matched_columns.len().to_string()).unwrap_or_default(),
            matched_columns.len()
        );
        if !db_only.is_empty() {
            msg.push_str(&format!(" DB-only (will be null): {db_only:?}"));
        }
        if !csv_only.is_empty() {
            msg.push_str(&format!(" CSV-only (ignored): {csv_only:?}"));
        }
        logger.warn(&msg);
    }
}

// ─── D1 Query Helpers ──────────────────────────────────────────────────────────

/// Row count result from `SELECT COUNT(*)`.
#[derive(Deserialize)]
struct CountResult {
    count: u32,
}

/// Returns the current row count for a table. Used before/after inserts to
/// accurately calculate new vs. updated rows (since SQLite's `changes()` does
/// not count implicit deletes from REPLACE conflict resolution).
async fn count_rows(d1: &D1Database, table_name: &str) -> Result<u32> {
    let query = format!("SELECT COUNT(*) AS count FROM {table_name}");
    let result = d1.prepare(&query).all().await?;
    let rows: Vec<CountResult> = result.results()?;
    Ok(rows.first().map(|r| r.count).unwrap_or(0))
}
