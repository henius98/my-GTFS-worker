//! Validate live GTFS headers against every active provider's schema and migration chain.

use gtfs_checks::{CheckResult, Provider, providers, root};
use importer::schema::SchemaCatalog;
use reqwest::blocking::Client;
use rusqlite::{Connection, params};
use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  io::Cursor,
  path::{Path, PathBuf},
  time::Duration,
};
use zip::ZipArchive;

type Schema = BTreeMap<String, Vec<String>>;

fn table_names(db: &Connection) -> CheckResult<BTreeSet<String>> {
  let mut statement = db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?;
  Ok(statement.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?)
}

fn columns(db: &Connection, table: &str) -> CheckResult<Vec<String>> {
  let mut statement = db.prepare(&format!("PRAGMA table_info({})", SchemaCatalog::quote_identifier(table)))?;
  Ok(statement.query_map([], |row| row.get(1))?.collect::<Result<_, _>>()?)
}

fn schema_from_base_statements(provider_name: &str) -> CheckResult<Schema> {
  let statements = SchemaCatalog::base_schema_statements(provider_name).ok_or_else(|| format!("no compiled base schema statements for {provider_name}"))?;
  let db = Connection::open_in_memory()?;
  for statement in statements {
    db.execute(statement, [])?;
  }
  let mut tables = Schema::new();
  for table in table_names(&db)? {
    if !SchemaCatalog::is_infrastructure_table(&table) {
      tables.insert(table.clone(), columns(&db, &table)?);
    }
  }
  Ok(tables)
}

fn validate_replay_cleanup_migrations() -> CheckResult {
  let db_johor = Connection::open_in_memory()?;
  for statement in SchemaCatalog::base_schema_statements("mybas-johor").ok_or("missing mybas-johor base schema")? {
    db_johor.execute(statement, [])?;
  }
  let fare = ["leg", "from", "to", "product"];
  db_johor.execute("INSERT OR REPLACE INTO fare_leg_rules (leg_group_id, from_area_id, to_area_id, fare_product_id) VALUES (?1, ?2, ?3, ?4)", fare)?;
  db_johor.execute("INSERT OR REPLACE INTO fare_leg_rules (leg_group_id, from_area_id, to_area_id, fare_product_id) VALUES (?1, ?2, ?3, ?4)", fare)?;
  let fare_count: i64 = db_johor.query_row("SELECT COUNT(*) FROM fare_leg_rules", [], |row| row.get(0))?;

  let db_agency = Connection::open_in_memory()?;
  for statement in SchemaCatalog::base_schema_statements("rapid-bus-mrtfeeder").ok_or("missing rapid-bus-mrtfeeder base schema")? {
    db_agency.execute(statement, [])?;
  }
  db_agency.execute("INSERT OR REPLACE INTO agency (agency_name, agency_url, agency_timezone, agency_phone, agency_lang) VALUES ('old', 'url', 'zone', 'phone', 'en')", [])?;
  db_agency.execute("INSERT OR REPLACE INTO agency (agency_name, agency_url, agency_timezone, agency_phone, agency_lang) VALUES ('latest', 'url', 'zone', 'phone', 'en')", [])?;
  let agency_names = {
    let mut statement = db_agency.prepare("SELECT agency_name FROM agency")?;
    statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?
  };
  if fare_count != 1 || agency_names != vec!["latest".to_owned()] {
    return Err("replay cleanup migrations did not enforce idempotency".into());
  }
  Ok(())
}

fn validate_legacy_progress_upgrade(directory: &Path) -> CheckResult {
  let mut prefix_files = fs::read_dir(directory)?
    .map(|entry| entry.map(|entry| entry.path()))
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().ends_with("_prefix_infrastructure_tables.sql")))
    .collect::<Vec<_>>();
  prefix_files.sort();
  let Some(prefix_file) = prefix_files.first() else {
    return Ok(());
  };
  let db = Connection::open_in_memory()?;
  db.execute_batch("CREATE TABLE agency (agency_name TEXT, agency_url TEXT, agency_timezone TEXT, agency_phone TEXT, agency_lang TEXT); CREATE TABLE logs (Id INTEGER PRIMARY KEY, Level TINYINT NOT NULL, Message TEXT NOT NULL); CREATE TABLE import_progress (Provider TEXT, FileName TEXT, CRC TEXT, LastProcessedLine INTEGER, LastProcessedByte INTEGER NOT NULL DEFAULT 0, Status TINYINT CHECK (Status IN (0, 1)), UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (Provider, FileName)); INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt) VALUES ('legacy-provider', 'stops.txt', 'legacy-crc', 17, 128, 1, '2026-08-30 00:00:00'); CREATE TABLE dataset_versions (Provider TEXT PRIMARY KEY, ETag TEXT, UpdatedAt DATETIME); INSERT INTO dataset_versions (Provider, ETag, UpdatedAt) VALUES ('legacy-provider', 'etag-1', '2026-08-30 00:00:00');")?;
  db.execute_batch(&fs::read_to_string(prefix_file)?)?;
  let migrated: (String, String, i64, i64, i64, String) = db.query_row("SELECT FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt FROM \"0_import_progress\"", [], |row| {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
  })?;
  if migrated != ("stops.txt".into(), "legacy-crc".into(), 17, 128, 1, "2026-08-30 00:00:00".into()) {
    return Err(format!("0_import_progress row changed during prefix migration: {migrated:?}").into());
  }
  let etag: String = db.query_row("SELECT ETag FROM \"0_dataset_versions\"", [], |row| row.get(0))?;
  if etag != "etag-1" {
    return Err(format!("0_dataset_versions ETag changed during prefix migration: {etag}").into());
  }
  let legacy_leftovers: i64 =
    db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('logs', 'import_progress', 'dataset_versions', 'daily_import_budget')", [], |row| row.get(0))?;
  if legacy_leftovers != 0 {
    return Err("unprefixed infrastructure table remained after prefix migration".into());
  }
  Ok(())
}

fn validate_staged_status_migration(directory: &Path) -> CheckResult {
  let migration_file = directory.join("20261121_add_staged_import_status.sql");
  if !migration_file.is_file() {
    return Err(format!("missing staged status migration: {}", migration_file.display()).into());
  }
  let db = Connection::open_in_memory()?;
  db.execute_batch(
    "CREATE TABLE \"0_import_progress\" (FileName TEXT PRIMARY KEY, CRC TEXT, LastProcessedLine INTEGER, LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0), Status TINYINT CHECK (Status IN (0, 1)), UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP) WITHOUT ROWID; INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt) VALUES ('stops.txt', 'done-crc', 10, 200, 0, '2026-11-20 00:00:00'), ('stop_times.txt', 'partial-crc', 50, 1024, 1, '2026-11-20 00:00:00');",
  )?;
  db.execute_batch(&fs::read_to_string(&migration_file)?)?;
  let remaining = {
    let mut statement = db.prepare("SELECT FileName, Status FROM \"0_import_progress\" ORDER BY FileName")?;
    statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?
  };
  if remaining != vec![("stops.txt".to_owned(), 0)] {
    return Err(format!("20261121_add_staged_import_status.sql must preserve Status=0 and drop legacy Status=1 rows; got {remaining:?}").into());
  }
  db.execute("INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, LastProcessedByte, Status) VALUES ('routes.txt', 'staged-crc', 5, 120, 2)", [])?;
  Ok(())
}

fn validate_local_migrations(provider_name: &str, directory: &Path, expected: &Schema) -> CheckResult {
  validate_legacy_progress_upgrade(directory)?;
  validate_staged_status_migration(directory)?;
  let migrations = SchemaCatalog::provider_migrations(provider_name).ok_or_else(|| format!("no compiled migrations for {provider_name}"))?;
  let db = Connection::open_in_memory()?;
  for migration in migrations {
    for statement in *migration {
      db.execute(statement, [])?;
    }
  }
  let runtime_tables = table_names(&db)?.into_iter().filter(|name| !SchemaCatalog::is_infrastructure_table(name)).collect::<BTreeSet<_>>();
  let expected_tables = expected.keys().cloned().collect::<BTreeSet<_>>();
  if runtime_tables != expected_tables {
    return Err(
      format!(
        "runtime migration tables differ from compile-time schema: runtime_only={:?}, compile_time_only={:?}",
        runtime_tables.difference(&expected_tables).collect::<Vec<_>>(),
        expected_tables.difference(&runtime_tables).collect::<Vec<_>>()
      )
      .into(),
    );
  }
  let progress = columns(&db, "0_import_progress")?;
  if progress != ["FileName", "CRC", "LastProcessedLine", "LastProcessedByte", "Status", "UpdatedAt"].map(str::to_owned).to_vec() {
    return Err(format!("runtime progress schema differs from expected single-table schema: {progress:?}").into());
  }
  db.execute("INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, Status) VALUES ('stops.txt', 'current-crc', 7, 1)", [])?;
  let checkpoint: (i64, i64) = db.query_row("SELECT LastProcessedLine, LastProcessedByte FROM \"0_import_progress\" WHERE FileName = 'stops.txt'", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
  if checkpoint != (7, 0) {
    return Err(format!("byte checkpoint default returned {checkpoint:?} instead of (7, 0)").into());
  }
  db.execute("UPDATE \"0_import_progress\" SET LastProcessedLine = 8, LastProcessedByte = 321, Status = 2 WHERE FileName = 'stops.txt'", [])?;
  let checkpoint: (i64, i64, i64) =
    db.query_row("SELECT LastProcessedLine, LastProcessedByte, Status FROM \"0_import_progress\" WHERE FileName = 'stops.txt'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
  if checkpoint != (8, 321, 2) {
    return Err(format!("staged checkpoint update returned {checkpoint:?} instead of (8, 321, 2)").into());
  }
  if db.execute("UPDATE \"0_import_progress\" SET Status = 3 WHERE FileName = 'stops.txt'", []).is_ok() {
    return Err("0_import_progress accepted invalid Status = 3".into());
  }
  db.execute("DELETE FROM \"0_import_progress\" WHERE FileName = 'stops.txt'", [])?;

  for (table, expected_columns) in expected {
    let actual_columns = columns(&db, table)?;
    if &actual_columns != expected_columns {
      return Err(format!("runtime columns for `{table}` differ from compile-time schema: runtime={actual_columns:?}, compile_time={expected_columns:?}").into());
    }
    let staging_table = SchemaCatalog::staging_table_name(table);
    let staging_create_sql = SchemaCatalog::staging_create_sql(provider_name, table).ok_or_else(|| format!("missing staging DDL for {provider_name}.{table}"))?;
    let column_refs = actual_columns.iter().map(String::as_str).collect::<Vec<_>>();
    let sql = SchemaCatalog::positional_insert_sql(&staging_table, &column_refs, SchemaCatalog::primary_keys(provider_name, table));
    let count_sql = format!("SELECT COUNT(*) FROM {}", SchemaCatalog::quote_identifier(table));
    db.execute_batch("SAVEPOINT replay_probe")?;
    db.execute(&SchemaCatalog::drop_table_if_exists_sql(&staging_table), [])?;
    db.execute(staging_create_sql, [])?;
    let has_primary_key = {
      let mut statement = db.prepare(&format!("PRAGMA table_info({})", SchemaCatalog::quote_identifier(&staging_table)))?;
      statement.query_map([], |row| row.get::<_, i64>(5))?.collect::<Result<Vec<_>, _>>()?.into_iter().any(|key| key != 0)
    };
    let indexes = {
      let mut statement = db.prepare(&format!("PRAGMA index_list({})", SchemaCatalog::quote_identifier(&staging_table)))?;
      statement.query_map([], |row| row.get::<_, i64>(2))?.collect::<Result<Vec<_>, _>>()?
    };
    if !has_primary_key && indexes.iter().all(|unique| *unique == 0) {
      return Err(format!("`{staging_table}` has no uniqueness constraint; checkpoint replay would append duplicate rows").into());
    }
    let max_allowed_indexes = 1;
    if indexes.len() > max_allowed_indexes {
      return Err(format!("`{staging_table}` has {} indexes; write-budget reservation assumes at most {} index writes per row", indexes.len(), max_allowed_indexes).into());
    }
    let probe = actual_columns.iter().enumerate().map(|(index, column)| if column == "id" { serde_json::json!(1) } else { serde_json::json!(format!("replay-probe-{index}")) }).collect::<Vec<_>>();
    let payload = serde_json::to_string(&vec![probe])?;
    db.execute(&sql, params![payload])?;
    let before = db.total_changes();
    db.execute(&sql, params![payload])?;
    let replay_writes = db.total_changes() - before;
    db.execute(&SchemaCatalog::drop_table_if_exists_sql(table), [])?;
    db.execute(&SchemaCatalog::rename_table_sql(&staging_table, table), [])?;
    let swapped_count: i64 = db.query_row(&count_sql, [], |row| row.get(0))?;
    db.execute_batch("ROLLBACK TO replay_probe; RELEASE replay_probe;")?;
    if swapped_count != 1 || replay_writes != 0 {
      return Err(format!("`{table}` retained {swapped_count} rows after staging UPSERT + swap and reported {replay_writes} unnecessary writes").into());
    }
  }
  Ok(())
}

fn is_allowed_synthetic_db_column(provider_name: &str, table: &str, column: &str) -> bool {
  provider_name == "rapid-bus-mrtfeeder" && table == "agency" && column == "id"
}

fn validate_provider(provider: &Provider, client: &Client) -> (Vec<String>, usize) {
  let name = &provider.name;
  let mut lines = vec![format!("\n## Provider: `{name}`")];
  let migration = root().join("migrations").join(name).join("0_gtfs_schema.sql");
  if !migration.is_file() {
    lines.push(format!("- ❌ Migration file not found: `{}`", migration.display()));
    return (lines, 1);
  }
  let expected = match schema_from_base_statements(name) {
    Ok(expected) => expected,
    Err(error) => {
      lines.push(format!("- ❌ Cannot read compiled base schema: {error}"));
      return (lines, 1);
    }
  };
  let mut errors = 0;
  match validate_local_migrations(name, migration.parent().unwrap_or(Path::new(".")), &expected) {
    Ok(()) => lines.push(
      "- ✅ Local migration chain and pre-split base schema match; production positional UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound."
        .into(),
    ),
    Err(error) => {
      lines.push(format!("- ❌ Local migration chain failed: {error}"));
      errors += 1;
    }
  }
  let url = format!("{}{}", provider.static_url, provider.static_provider);
  // Network failures must enter the report.
  let payload = client.get(&url).header("User-Agent", "my-GTFS-worker-schema-validator/1.0").send().and_then(|response| response.error_for_status()).and_then(|response| response.bytes());
  let payload = match payload {
    Ok(payload) => payload,
    Err(error) => {
      lines.push(format!("- ❌ Failed to download feed: {error}"));
      return (lines, errors + 1);
    }
  };
  let mut archive = match ZipArchive::new(Cursor::new(payload)) {
    Ok(archive) => archive,
    Err(error) => {
      lines.push(format!("- ❌ Failed to open feed ZIP: {error}"));
      return (lines, errors + 1);
    }
  };
  let mut matched = BTreeSet::new();
  let mut filenames = match archive.file_names().collect::<Result<Vec<_>, _>>() {
    Ok(names) => names
      .into_iter()
      .map(std::borrow::Cow::into_owned)
      .filter(|name| name.ends_with(".txt") && !name.contains("__MACOSX") && !name.rsplit('/').next().unwrap_or_default().starts_with("._"))
      .collect::<Vec<_>>(),
    Err(error) => {
      lines.push(format!("- ❌ Failed to read feed ZIP entry names: {error}"));
      return (lines, errors + 1);
    }
  };
  filenames.sort();
  for filename in filenames {
    let table = filename.rsplit('/').next().unwrap_or_default().trim_end_matches(".txt");
    let Some(db_columns) = expected.get(table) else {
      lines.push(format!("- ❌ Table `{table}` from the feed is absent from the migration."));
      errors += 1;
      continue;
    };
    matched.insert(table.to_owned());
    let csv_columns = (|| -> CheckResult<Vec<String>> {
      let file = archive.by_name(&filename)?;
      let mut reader = csv::ReaderBuilder::new().has_headers(true).from_reader(file);
      Ok(reader.headers()?.iter().map(|column| column.trim().trim_start_matches('\u{feff}').to_owned()).collect())
    })();
    let csv_columns = match csv_columns {
      Ok(columns) => columns,
      Err(error) => {
        lines.push(format!("- ❌ Could not read `{filename}` header: {error}"));
        errors += 1;
        continue;
      }
    };
    let expected_csv_columns = db_columns.iter().filter(|column| !is_allowed_synthetic_db_column(name, table, column)).cloned().collect::<Vec<_>>();
    if csv_columns == expected_csv_columns {
      lines.push(format!("- ✅ `{table}` matches exactly."));
      continue;
    }
    let csv_set = csv_columns.iter().cloned().collect::<BTreeSet<_>>();
    let db_set = expected_csv_columns.iter().cloned().collect::<BTreeSet<_>>();
    let missing = csv_set.difference(&db_set).collect::<Vec<_>>();
    let extra = db_set.difference(&csv_set).collect::<Vec<_>>();
    if missing.is_empty() && extra.is_empty() {
      lines.push(format!("- ❌ `{table}` has a column-order mismatch: migration={expected_csv_columns:?}, feed={csv_columns:?}"));
    } else {
      lines.push(format!("- ❌ `{table}` differs: missing_in_db={missing:?}, extra_in_db={extra:?}"));
    }
    errors += 1;
  }
  for table in expected.keys().filter(|name| !matched.contains(*name)) {
    lines.push(format!("- ⚠️ Table `{table}` exists in the migration but not in the feed."));
  }
  (lines, errors)
}

fn main() -> CheckResult {
  let mut report_path = root().join("test/importer/migrations_validation_report.md");
  let mut write_report = true;
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--no-report" => write_report = false,
      "--report" => report_path = PathBuf::from(args.next().ok_or("missing --report path")?),
      _ => return Err(format!("unknown argument: {arg}").into()),
    }
  }
  let mut lines = vec!["# Migrations Validation Report".to_owned(), "\n## Replay-safety migrations".to_owned()];
  let mut errors = 0;
  match validate_replay_cleanup_migrations() {
    Ok(()) => lines.push("- ✅ Replay cleanup migrations enforce future idempotency.".into()),
    Err(error) => {
      lines.push(format!("- ❌ Replay cleanup migration validation failed: {error}"));
      errors += 1;
    }
  }
  let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
  for provider in providers()?.into_iter().filter(|provider| provider.is_active) {
    let (provider_lines, provider_errors) = validate_provider(&provider, &client);
    lines.extend(provider_lines);
    errors += provider_errors;
  }
  let report = format!("{}\n", lines.join("\n"));
  if write_report {
    fs::write(report_path, &report)?;
  }
  print!("{report}");
  if errors != 0 {
    return Err(format!("validation failed with {errors} schema error(s)").into());
  }
  println!("All active provider schemas match their live GTFS feeds.");
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn local_migrations_and_staging_swaps_succeed() -> CheckResult {
    validate_replay_cleanup_migrations()?;
    for provider in providers()?.into_iter().filter(|provider| provider.is_active) {
      let directory = root().join("migrations").join(&provider.name);
      let expected = schema_from_base_statements(&provider.name)?;
      validate_local_migrations(&provider.name, &directory, &expected)?;
    }
    Ok(())
  }
}
