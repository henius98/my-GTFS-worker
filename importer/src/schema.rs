use std::sync::Arc;

pub use crate::sql_queries::{provider_maintenance_queries, quote_identifier};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableImportPlan {
  pub csv_indices: Vec<usize>,
  pub matched_columns: Vec<&'static str>,
  pub insert_sql: Arc<str>,
}

pub struct SchemaCatalog;

impl SchemaCatalog {
  pub fn provider_schemas(provider_name: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
    parse_provider_schemas(provider_name)
  }

  pub fn provider_migrations(provider_name: &str) -> Option<&'static [&'static str]> {
    get_provider_schema_sql(provider_name)
  }

  pub fn primary_keys(provider_name: &str, table_name: &str) -> &'static [&'static str] {
    primary_keys(provider_name, table_name)
  }

  pub fn maintenance_queries(provider_name: &str) -> Vec<&'static str> {
    provider_maintenance_queries(provider_name)
  }

  pub fn is_infrastructure_table(table_name: &str) -> bool {
    matches!(table_name, "departure_cache" | "0_daily_import_budget" | "0_import_progress" | "0_dataset_versions")
  }

  pub fn plan_table_import(provider_name: &str, table_name: &str, db_columns: &'static [&'static str], headers: &csv::StringRecord) -> Option<TableImportPlan> {
    let mut csv_indices = Vec::with_capacity(db_columns.len());
    let mut matched_columns = Vec::with_capacity(db_columns.len());
    for &db_column in db_columns {
      if let Some(position) = headers.iter().position(|header| header == db_column) {
        csv_indices.push(position);
        matched_columns.push(db_column);
      }
    }
    if matched_columns.is_empty() {
      return None;
    }
    let insert_sql = positional_insert_sql(table_name, &matched_columns, primary_keys(provider_name, table_name));
    Some(TableImportPlan { csv_indices, matched_columns, insert_sql: Arc::from(insert_sql) })
  }
}

pub fn parse_provider_schemas(provider_name: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
  include!(concat!(env!("OUT_DIR"), "/schemas.rs"))
}

pub fn get_provider_schema_sql(provider_name: &str) -> Option<&'static [&'static str]> {
  include!(concat!(env!("OUT_DIR"), "/schema_sql.rs"))
}

pub fn primary_keys(provider_name: &str, table_name: &str) -> &'static [&'static str] {
  include!(concat!(env!("OUT_DIR"), "/primary_keys.rs"))
}

pub fn positional_insert_sql(table_name: &str, columns: &[&str], keys: &[&str]) -> String {
  let column_list = columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");
  let selects = (0..columns.len()).map(|index| format!("json_extract(value, '$[{index}]')")).collect::<Vec<_>>().join(", ");
  let assignments = columns
    .iter()
    .filter(|column| !keys.contains(column))
    .map(|column| {
      let quoted = quote_identifier(column);
      format!("{quoted} = excluded.{quoted}")
    })
    .collect::<Vec<_>>()
    .join(", ");
  let quoted_table = quote_identifier(table_name);
  if assignments.is_empty() {
    return format!("INSERT INTO {quoted_table} ({column_list}) SELECT {selects} FROM json_each(?) WHERE TRUE ON CONFLICT DO NOTHING");
  }
  let changed = columns
    .iter()
    .filter(|column| !keys.contains(column))
    .map(|column| {
      let quoted = quote_identifier(column);
      format!("{quoted_table}.{quoted} IS NOT excluded.{quoted}")
    })
    .collect::<Vec<_>>()
    .join(" OR ");
  format!("INSERT INTO {quoted_table} ({column_list}) SELECT {selects} FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET {assignments} WHERE {changed}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn positional_sql_quotes_identifiers() {
    assert_eq!(
      positional_insert_sql("stop\"times", &["trip\"id", "stop_sequence"], &["trip\"id"]),
      "INSERT INTO \"stop\"\"times\" (\"trip\"\"id\", \"stop_sequence\") SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]') FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET \"stop_sequence\" = excluded.\"stop_sequence\" WHERE \"stop\"\"times\".\"stop_sequence\" IS NOT excluded.\"stop_sequence\""
    );
  }

  #[test]
  fn upserts_preserve_primary_keys_and_skip_key_only_conflicts() {
    let keys = SchemaCatalog::primary_keys("ktmb", "stop_times");
    assert_eq!(keys, &["stop_id", "trip_id", "stop_sequence"]);
    let sql = positional_insert_sql("stop_times", &["stop_id", "trip_id", "stop_sequence", "arrival_time"], keys);
    let update = sql.split_once("DO UPDATE SET").map(|(_, update)| update).unwrap_or_default();
    assert!(update.contains("\"arrival_time\" = excluded.\"arrival_time\""));
    assert!(!update.contains("\"stop_id\""));
    assert!(!update.contains("\"trip_id\""));
    assert!(!update.contains("\"stop_sequence\""));
    assert!(positional_insert_sql("stop_times", keys, keys).ends_with("DO NOTHING"));
    assert_eq!(SchemaCatalog::primary_keys("mybas-johor", "areas"), &["area_id"]);
    assert_eq!(SchemaCatalog::primary_keys("ktmb", "trips"), &["trip_id"]);
    assert!(SchemaCatalog::primary_keys("mybas-johor", "fare_leg_rules").is_empty());
  }

  #[test]
  fn plan_table_import_projects_matching_headers_and_builds_sql() {
    let headers = csv::StringRecord::from(vec!["extra_col", "arrival_time", "stop_id", "trip_id", "stop_sequence"]);
    let plan = SchemaCatalog::plan_table_import("ktmb", "stop_times", &["stop_id", "trip_id", "stop_sequence", "arrival_time", "missing_col"], &headers);
    assert!(plan.is_some());
    let Some(plan) = plan else {
      return;
    };
    assert_eq!(plan.csv_indices, vec![2, 3, 4, 1]);
    assert_eq!(plan.matched_columns, vec!["stop_id", "trip_id", "stop_sequence", "arrival_time"]);
    assert!(plan.insert_sql.contains("ON CONFLICT DO UPDATE SET \"arrival_time\" = excluded.\"arrival_time\""));

    let unmatched = csv::StringRecord::from(vec!["unrelated"]);
    assert!(SchemaCatalog::plan_table_import("ktmb", "stop_times", &["stop_id"], &unmatched).is_none());
  }

  #[test]
  fn provider_maintenance_queries_are_non_empty_for_known_providers() {
    let providers = ["mybas-johor", "ktmb", "rapid-bus-mrtfeeder", "rapid-rail-kl", "rapid-bus-kl", "rapid-bus-penang"];
    for provider in providers {
      let queries = SchemaCatalog::maintenance_queries(provider);
      assert!(!queries.is_empty(), "queries for {provider} should not be empty");
      for query in queries {
        assert!(query.starts_with("DELETE") || query.starts_with("INSERT"), "unexpected query in {provider}: {query}");
      }
    }
    assert!(SchemaCatalog::maintenance_queries("unknown-provider").is_empty());
  }
}
