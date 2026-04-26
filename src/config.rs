//! Shared configuration: constants, types, and utility helpers used across all modules.

use serde::Deserialize;

// ─── GTFS Table Definitions ────────────────────────────────────────────────────
// Maps CSV filenames to base DB table names (without operator prefix).
// Actual table names are constructed at runtime as: {prefix}_{base_name}
// e.g., ("routes.txt", "routes") → "ktmb_routes", "rapid_bus_penang_routes", etc.
// Columns are discovered automatically from schema.sql via PRAGMA table_info.
pub const GTFS_TABLES: &[(&str, &str)] = &[
    ("shapes.txt", "shapes"),
    ("routes.txt", "routes"),
    ("stops.txt", "stops"),
    ("calendar.txt", "calendar"),
    ("trips.txt", "trips"),
    ("stop_times.txt", "stop_times"),
    ("agency.txt", "agency"),
    ("areas.txt", "areas"),
    ("fare_leg_rules.txt", "fare_leg_rules"),
    ("fare_media.txt", "fare_media"),
    ("fare_products.txt", "fare_products"),
    ("rider_categories.txt", "rider_categories"),
    ("stop_areas.txt", "stop_areas"),
    ("frequencies.txt", "frequencies"),
    ("calendar_dates.txt", "calendar_dates"),
];

// ─── D1 Limits ─────────────────────────────────────────────────────────────────
/// Maximum bound parameters per single SQL query (Cloudflare D1 hard limit).
pub const D1_MAX_BOUND_PARAMS: usize = 100;
/// Maximum statements allowed in a single `d1.batch()` call.
pub const D1_MAX_BATCH_STATEMENTS: usize = 100;

// ─── Types ─────────────────────────────────────────────────────────────────────

/// Represents a single column from SQLite's `PRAGMA table_info()` result.
#[derive(Deserialize)]
pub struct ColumnInfo {
    pub name: String,
}

/// Pre-fetched schema info for a GTFS table type (shared across all operators).
/// The `base_name` field stores the unprefixed table name (e.g., "routes").
/// Full table names are constructed at runtime as `{prefix}_{base_name}`.
pub struct TableSchema {
    pub csv_file: &'static str,
    pub base_name: &'static str,
    pub db_columns: Vec<String>,
    /// Maximum rows per multi-row INSERT (based on DB column count).
    pub rows_per_insert: usize,
    /// Pre-built comma-separated column list for SQL (e.g. "col_a, col_b, col_c").
    pub col_list: String,
}

// ─── Helpers ───────────────────────────────────────────────────────────────────

/// Maps any `Display`-able error into `worker::Error::RustError`.
#[inline]
pub fn to_worker_err(e: impl std::fmt::Display) -> worker::Error {
    worker::Error::RustError(e.to_string())
}

/// Derives a SQL-safe table prefix from a GTFS enum value.
///
/// Examples:
/// - `"ktmb"` → `"ktmb"`
/// - `"prasarana?category=rapid-bus-penang"` → `"rapid_bus_penang"`
/// - `"mybas-johor"` → `"mybas_johor"`
pub fn enum_to_prefix(item: &str) -> String {
    let base = item.split("category=").nth(1).unwrap_or(item);
    // Sanitize: only allow alphanumeric, hyphens, and underscores.
    // This prevents SQL injection and ensures the prefix is safe for table names.
    let sanitized: String = base
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    sanitized.replace('-', "_")
}
