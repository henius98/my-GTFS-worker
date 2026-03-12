//! Schema introspection: queries D1 for table column info and pre-fetches schemas.

use worker::{D1Database, Result};

use crate::config::*;
use crate::logger::Logger;

/// Queries D1 for the column names of a table using `PRAGMA table_info()`.
/// This makes `schema.sql` the single source of truth for columns —
/// no hardcoded column lists needed in Rust code.
async fn get_table_columns(d1: &D1Database, table_name: &str) -> Result<Vec<String>> {
    let query = format!("PRAGMA table_info({table_name})");
    let results = d1.prepare(&query).all().await?;
    let columns: Vec<ColumnInfo> = results.results()?;
    Ok(columns.into_iter().map(|c| c.name).collect())
}

/// Pre-fetches DB column schemas for all GTFS table types using a reference operator prefix.
/// Since all operators share identical table structures, we query one set of tables
/// and reuse the column info across all operators at runtime.
///
/// For example, with `prefix = "ktmb"`, queries `PRAGMA table_info(ktmb_routes)`, etc.
pub async fn prefetch_table_schemas(
    d1: &D1Database,
    logger: &Logger,
    prefix: &str,
) -> Result<Vec<TableSchema>> {
    let mut schemas = Vec::with_capacity(GTFS_TABLES.len());

    for &(csv_file, base_name) in GTFS_TABLES {
        let full_table = format!("{prefix}_{base_name}");
        let db_columns = get_table_columns(d1, &full_table).await?;

        if db_columns.is_empty() {
            logger.warn(&format!("Table `{full_table}` has no columns or does not exist, skipping."));
            continue;
        }

        let col_count = db_columns.len();
        let rows_per_insert = (D1_MAX_BOUND_PARAMS / col_count).max(1);
        let col_list = db_columns.join(", ");

        logger.info(&format!(
            "Schema loaded for `{base_name}` ({col_count} columns): \
             {rows_per_insert} rows/query, up to {} rows/batch",
            rows_per_insert * D1_MAX_BATCH_STATEMENTS
        ));

        schemas.push(TableSchema {
            csv_file,
            base_name,
            db_columns,
            rows_per_insert,
            col_list,
        });
    }

    Ok(schemas)
}
