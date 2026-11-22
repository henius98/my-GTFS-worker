use crate::format::ColumnFormatter;
use std::sync::Arc;

pub(crate) const STAGING_TABLE_SUFFIX: &str = include!(concat!(env!("OUT_DIR"), "/staging_suffix.rs"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlannedColumn {
  pub(crate) csv_index: usize,
  pub(crate) db_column: &'static str,
  pub(crate) formatter: ColumnFormatter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableImportPlan {
  pub(crate) columns: Vec<PlannedColumn>,
  pub(crate) insert_sql: Arc<str>,
}

pub struct SchemaCatalog;

impl SchemaCatalog {
  pub(crate) fn provider_schemas(provider_name: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
    include!(concat!(env!("OUT_DIR"), "/schemas.rs"))
  }

  pub fn base_schema_statements(provider_name: &str) -> Option<&'static [&'static str]> {
    include!(concat!(env!("OUT_DIR"), "/base_schema_sql.rs"))
  }

  pub fn provider_migrations(provider_name: &str) -> Option<&'static [&'static [&'static str]]> {
    include!(concat!(env!("OUT_DIR"), "/migrations_sql.rs"))
  }

  pub fn primary_keys(provider_name: &str, table_name: &str) -> &'static [&'static str] {
    include!(concat!(env!("OUT_DIR"), "/primary_keys.rs"))
  }

  pub fn staging_table_name(table_name: &str) -> String {
    format!("{table_name}{STAGING_TABLE_SUFFIX}")
  }

  pub fn staging_create_sql(provider_name: &str, table_name: &str) -> Option<&'static str> {
    if Self::is_infrastructure_table(table_name) {
      return None;
    }
    include!(concat!(env!("OUT_DIR"), "/staging_schema_sql.rs"))
  }

  pub fn max_writes_per_staging_row(provider_name: &str, table_name: &str) -> u64 {
    include!(concat!(env!("OUT_DIR"), "/staging_writes_per_row.rs"))
  }

  pub fn is_infrastructure_table(table_name: &str) -> bool {
    matches!(table_name, "departure_cache" | "0_daily_import_budget" | "0_import_progress" | "0_dataset_versions")
  }

  pub fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
  }

  pub fn drop_table_if_exists_sql(table_name: &str) -> String {
    format!("DROP TABLE IF EXISTS {}", Self::quote_identifier(table_name))
  }

  pub fn rename_table_sql(from_table: &str, to_table: &str) -> String {
    format!("ALTER TABLE {} RENAME TO {}", Self::quote_identifier(from_table), Self::quote_identifier(to_table))
  }

  pub fn positional_insert_sql(table_name: &str, columns: &[&str], keys: &[&str]) -> String {
    let column_list = columns.iter().map(|column| Self::quote_identifier(column)).collect::<Vec<_>>().join(", ");
    let selects = (0..columns.len()).map(|index| format!("json_extract(value, '$[{index}]')")).collect::<Vec<_>>().join(", ");
    let quoted_table = Self::quote_identifier(table_name);
    let non_key_columns = columns.iter().copied().filter(|column| !keys.contains(column)).map(Self::quote_identifier).collect::<Vec<_>>();
    if non_key_columns.is_empty() {
      return format!("INSERT INTO {quoted_table} ({column_list}) SELECT {selects} FROM json_each(?) WHERE TRUE ON CONFLICT DO NOTHING");
    }
    let assignments = non_key_columns.iter().map(|quoted| format!("{quoted} = excluded.{quoted}")).collect::<Vec<_>>().join(", ");
    let changed = non_key_columns.iter().map(|quoted| format!("{quoted_table}.{quoted} IS NOT excluded.{quoted}")).collect::<Vec<_>>().join(" OR ");
    format!("INSERT INTO {quoted_table} ({column_list}) SELECT {selects} FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET {assignments} WHERE {changed}")
  }

  pub(crate) fn plan_table_import(provider_name: &str, table_name: &str, db_columns: &'static [&'static str], headers: &csv::StringRecord) -> Option<TableImportPlan> {
    let mut columns = Vec::with_capacity(db_columns.len());
    let mut matched_columns = Vec::with_capacity(db_columns.len());
    for &db_column in db_columns {
      if let Some(csv_index) = headers.iter().position(|header| header.trim().trim_start_matches('\u{feff}').trim() == db_column) {
        columns.push(PlannedColumn { csv_index, db_column, formatter: ColumnFormatter::for_column(table_name, db_column) });
        matched_columns.push(db_column);
      }
    }
    if columns.is_empty() {
      return None;
    }
    let staging_table = Self::staging_table_name(table_name);
    let insert_sql = Self::positional_insert_sql(&staging_table, &matched_columns, Self::primary_keys(provider_name, table_name));
    Some(TableImportPlan { columns, insert_sql: Arc::from(insert_sql) })
  }
}

#[cfg(test)]
#[path = "../../test/unit/importer/schema.rs"]
mod tests;
