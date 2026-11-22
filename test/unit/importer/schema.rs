use super::*;

#[test]
fn positional_sql_quotes_identifiers() {
  assert_eq!(
    SchemaCatalog::positional_insert_sql("stop\"times", &["trip\"id", "stop_sequence"], &["trip\"id"]),
    "INSERT INTO \"stop\"\"times\" (\"trip\"\"id\", \"stop_sequence\") SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]') FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET \"stop_sequence\" = excluded.\"stop_sequence\" WHERE \"stop\"\"times\".\"stop_sequence\" IS NOT excluded.\"stop_sequence\""
  );
}

#[test]
fn upserts_preserve_primary_keys_and_skip_key_only_conflicts() {
  let keys = SchemaCatalog::primary_keys("ktmb", "stop_times");
  assert_eq!(keys, &["stop_id", "trip_id", "stop_sequence"]);
  let sql = SchemaCatalog::positional_insert_sql("stop_times", &["stop_id", "trip_id", "stop_sequence", "arrival_time"], keys);
  let update = sql.split_once("DO UPDATE SET").map(|(_, update)| update).unwrap_or_default();
  assert!(update.contains("\"arrival_time\" = excluded.\"arrival_time\""));
  assert!(!update.contains("\"stop_id\""));
  assert!(!update.contains("\"trip_id\""));
  assert!(!update.contains("\"stop_sequence\""));
  assert!(SchemaCatalog::positional_insert_sql("stop_times", keys, keys).ends_with("DO NOTHING"));
  assert_eq!(SchemaCatalog::primary_keys("mybas-johor", "areas"), &["area_id"]);
  assert_eq!(SchemaCatalog::primary_keys("ktmb", "trips"), &["trip_id"]);
  assert_eq!(SchemaCatalog::primary_keys("mybas-johor", "fare_leg_rules"), &["leg_group_id", "from_area_id", "to_area_id", "fare_product_id"]);
  assert_eq!(SchemaCatalog::primary_keys("rapid-bus-mrtfeeder", "agency"), &["id"]);
}

#[test]
fn plan_table_import_projects_matching_headers_and_targets_staging_table() {
  let headers = csv::StringRecord::from(vec!["\u{feff}stop_id", " extra_col ", "arrival_time ", "trip_id", "stop_sequence"]);
  let plan = SchemaCatalog::plan_table_import("ktmb", "stop_times", &["stop_id", "trip_id", "stop_sequence", "arrival_time", "missing_col"], &headers);
  assert!(plan.is_some());
  let Some(plan) = plan else {
    return;
  };
  assert_eq!(
    plan.columns,
    vec![
      PlannedColumn { csv_index: 0, db_column: "stop_id", formatter: ColumnFormatter::None },
      PlannedColumn { csv_index: 3, db_column: "trip_id", formatter: ColumnFormatter::None },
      PlannedColumn { csv_index: 4, db_column: "stop_sequence", formatter: ColumnFormatter::None },
      PlannedColumn { csv_index: 2, db_column: "arrival_time", formatter: ColumnFormatter::GtfsTime },
    ]
  );
  assert!(plan.insert_sql.starts_with("INSERT INTO \"stop_times__staging\""));
  assert!(plan.insert_sql.contains("ON CONFLICT DO UPDATE SET \"arrival_time\" = excluded.\"arrival_time\""));

  let freq_headers = csv::StringRecord::from(vec!["trip_id", "start_time", "end_time", "headway_secs"]);
  let freq_plan = SchemaCatalog::plan_table_import("rapid-rail-kl", "frequencies", &["trip_id", "start_time", "end_time", "headway_secs"], &freq_headers);
  let Some(freq_plan) = freq_plan else {
    panic!("frequencies plan must match");
  };
  assert_eq!(
    freq_plan.columns.iter().map(|column| (column.db_column, column.formatter)).collect::<Vec<_>>(),
    vec![("trip_id", ColumnFormatter::None), ("start_time", ColumnFormatter::GtfsTime), ("end_time", ColumnFormatter::GtfsTime), ("headway_secs", ColumnFormatter::None)]
  );

  let unmatched = csv::StringRecord::from(vec!["unrelated"]);
  assert!(SchemaCatalog::plan_table_import("ktmb", "stop_times", &["stop_id"], &unmatched).is_none());
}

#[test]
fn base_schema_statements_strip_comments_and_never_split_comment_semicolons() {
  let providers = ["mybas-johor", "ktmb", "rapid-bus-mrtfeeder", "rapid-rail-kl", "rapid-bus-kl", "rapid-bus-penang"];
  for provider in providers {
    let statements = SchemaCatalog::base_schema_statements(provider);
    assert!(statements.is_some(), "base schema statements missing for {provider}");
    for statement in statements.unwrap_or_default() {
      assert!(!statement.is_empty(), "empty statement in {provider}");
      assert!(!statement.starts_with("--"), "unstripped comment in {provider}: {statement}");
      assert!(!statement.starts_with("empty route_id"), "comment semicolon split in {provider}: {statement}");
    }
  }
  assert!(SchemaCatalog::base_schema_statements("unknown-provider").is_none());
}

#[test]
fn staging_create_sql_covers_all_gtfs_tables_and_excludes_infrastructure_tables() {
  let providers = ["mybas-johor", "ktmb", "rapid-bus-mrtfeeder", "rapid-rail-kl", "rapid-bus-kl", "rapid-bus-penang"];
  for provider in providers {
    let schemas = SchemaCatalog::provider_schemas(provider).unwrap_or_default();
    assert!(!schemas.is_empty(), "schemas missing for {provider}");
    for &(table_name, _) in schemas {
      let ddl = SchemaCatalog::staging_create_sql(provider, table_name);
      if SchemaCatalog::is_infrastructure_table(table_name) {
        assert!(ddl.is_none(), "infrastructure table {table_name} in {provider} must not produce staging DDL");
      } else {
        let expected_prefix = format!("CREATE TABLE {}", SchemaCatalog::quote_identifier(&SchemaCatalog::staging_table_name(table_name)));
        let Some(ddl) = ddl else {
          panic!("missing staging DDL for {provider}.{table_name}");
        };
        assert!(ddl.starts_with(&expected_prefix), "unexpected staging DDL for {provider}.{table_name}: {ddl}");
        if table_name == "stop_times" {
          assert!(ddl.ends_with("WITHOUT ROWID"), "stop_times staging DDL in {provider} must use WITHOUT ROWID: {ddl}");
        }
      }
    }
  }
  assert!(SchemaCatalog::staging_create_sql("unknown-provider", "stops").is_none());
}

#[test]
fn max_writes_per_staging_row_uses_1_for_without_rowid_tables_and_2_as_fallback() {
  let providers = ["mybas-johor", "ktmb", "rapid-bus-mrtfeeder", "rapid-rail-kl", "rapid-bus-kl", "rapid-bus-penang"];
  for provider in providers {
    let schemas = SchemaCatalog::provider_schemas(provider).unwrap_or_default();
    for &(table_name, _) in schemas {
      if !SchemaCatalog::is_infrastructure_table(table_name) {
        assert_eq!(SchemaCatalog::max_writes_per_staging_row(provider, table_name), 1, "{provider}.{table_name} should reserve 1 write per row");
      }
    }
  }
  assert_eq!(SchemaCatalog::max_writes_per_staging_row("ktmb", "departure_cache"), 2);
  assert_eq!(SchemaCatalog::max_writes_per_staging_row("unknown-provider", "stops"), 2);
}
