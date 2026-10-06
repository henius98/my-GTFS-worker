//! Validate live GTFS headers against every active provider's schema and migration chain.

use gtfs_checks::{CheckResult, Provider, providers, root};
use reqwest::blocking::Client;
use rusqlite::{Connection, params_from_iter};
use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  io::Cursor,
  path::{Path, PathBuf},
  time::Duration,
};
use zip::ZipArchive;

const INFRASTRUCTURE_TABLES: [&str; 5] = ["daily_import_budget", "departure_cache", "logs", "dataset_versions", "import_progress"];
type Schema = BTreeMap<String, Vec<String>>;

fn quote(identifier: &str) -> String {
  format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn table_names(db: &Connection) -> CheckResult<BTreeSet<String>> {
  let mut statement = db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?;
  Ok(statement.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?)
}

fn columns(db: &Connection, table: &str) -> CheckResult<Vec<String>> {
  let mut statement = db.prepare(&format!("PRAGMA table_info({})", quote(table)))?;
  Ok(statement.query_map([], |row| row.get(1))?.collect::<Result<_, _>>()?)
}

fn schema(path: &Path) -> CheckResult<Schema> {
  let db = Connection::open_in_memory()?;
  db.execute_batch(&fs::read_to_string(path)?)?;
  let mut tables = Schema::new();
  for table in table_names(&db)? {
    if !INFRASTRUCTURE_TABLES.contains(&table.as_str()) {
      tables.insert(table.clone(), columns(&db, &table)?);
    }
  }
  Ok(tables)
}

fn migration_files(directory: &Path) -> CheckResult<Vec<PathBuf>> {
  let mut files = fs::read_dir(directory)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()?;
  files.retain(|path| path.extension().is_some_and(|extension| extension == "sql"));
  files.sort_by_key(|path| {
    let name = path.file_name().map(|value| value.to_string_lossy().into_owned()).unwrap_or_default();
    let number = name.split('_').next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(u64::MAX);
    (number, name)
  });
  Ok(files)
}

fn validate_replay_cleanup_migrations() -> CheckResult {
  let db = Connection::open_in_memory()?;
  db.execute_batch("CREATE TABLE fare_leg_rules (leg_group_id TEXT, from_area_id TEXT, to_area_id TEXT, fare_product_id TEXT); CREATE TABLE agency (agency_name TEXT, agency_url TEXT, agency_timezone TEXT, agency_phone TEXT, agency_lang TEXT);")?;
  let fare = ["leg", "from", "to", "product"];
  db.execute("INSERT INTO fare_leg_rules VALUES (?1, ?2, ?3, ?4)", fare)?;
  db.execute("INSERT INTO agency VALUES ('old', 'url', 'zone', 'phone', 'en'), ('new', 'url', 'zone', 'phone', 'en')", [])?;
  db.execute_batch(&fs::read_to_string(root().join("migrations/mybas-johor/20260826_deduplicate_fare_leg_rules.sql"))?)?;
  db.execute_batch(&fs::read_to_string(root().join("migrations/rapid-bus-mrtfeeder/20260826_enforce_single_agency.sql"))?)?;
  db.execute("INSERT OR REPLACE INTO fare_leg_rules VALUES (?1, ?2, ?3, ?4)", fare)?;
  db.execute("INSERT OR REPLACE INTO agency VALUES ('latest', 'url', 'zone', 'phone', 'en')", [])?;
  let fare_count: i64 = db.query_row("SELECT COUNT(*) FROM fare_leg_rules", [], |row| row.get(0))?;
  let agency_names = {
    let mut statement = db.prepare("SELECT agency_name FROM agency")?;
    statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?
  };
  if fare_count != 1 || agency_names != vec!["latest".to_owned()] {
    return Err("replay cleanup migrations did not enforce idempotency".into());
  }
  Ok(())
}

fn validate_legacy_progress_upgrade(directory: &Path) -> CheckResult {
  let files = migration_files(directory)?.into_iter().filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().ends_with("_add_import_progress_byte.sql"))).collect::<Vec<_>>();
  if files.len() != 1 {
    return Err(format!("expected exactly one progress-byte migration, found {}", files.len()).into());
  }
  let db = Connection::open_in_memory()?;
  db.execute_batch("CREATE TABLE import_progress (Provider TEXT, FileName TEXT, CRC TEXT, LastProcessedLine INTEGER, Status TINYINT CHECK (Status IN (0, 1)), UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (Provider, FileName)); CREATE TABLE import_progress_offsets (Provider TEXT, FileName TEXT, CRC TEXT, LastProcessedLine INTEGER, LastProcessedByte INTEGER, PRIMARY KEY (Provider, FileName)); INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status, UpdatedAt) VALUES ('legacy-provider', 'stops.txt', 'legacy-crc', 17, 1, '2026-08-30 00:00:00');")?;
  db.execute_batch(&fs::read_to_string(&files[0])?)?;
  let row: (String, String, String, i64, i64, i64, String) =
    db.query_row("SELECT Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt FROM import_progress", [], |row| {
      Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
    })?;
  if row != ("legacy-provider".into(), "stops.txt".into(), "legacy-crc".into(), 17, 0, 1, "2026-08-30 00:00:00".into()) {
    return Err(format!("legacy progress row changed during migration: {row:?}").into());
  }
  let leftovers: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('import_progress_legacy', 'import_progress_offsets')", [], |row| row.get(0))?;
  if leftovers != 0 {
    return Err("legacy or sidecar progress table remained after migration".into());
  }
  Ok(())
}

fn validate_local_migrations(directory: &Path, expected: &Schema) -> CheckResult {
  validate_legacy_progress_upgrade(directory)?;
  let db = Connection::open_in_memory()?;
  for migration in migration_files(directory)? {
    db.execute_batch(&fs::read_to_string(migration)?)?;
  }
  let runtime_tables = table_names(&db)?.into_iter().filter(|name| !INFRASTRUCTURE_TABLES.contains(&name.as_str())).collect::<BTreeSet<_>>();
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
  let progress = columns(&db, "import_progress")?;
  if progress != ["Provider", "FileName", "CRC", "LastProcessedLine", "LastProcessedByte", "Status", "UpdatedAt"].map(str::to_owned).to_vec() {
    return Err(format!("runtime progress schema differs from expected single-table schema: {progress:?}").into());
  }
  db.execute("INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status) VALUES ('checkpoint-probe', 'stops.txt', 'current-crc', 7, 1)", [])?;
  let checkpoint: (i64, i64) = db.query_row("SELECT LastProcessedLine, LastProcessedByte FROM import_progress WHERE Provider = 'checkpoint-probe'", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
  if checkpoint != (7, 0) {
    return Err(format!("byte checkpoint default returned {checkpoint:?} instead of (7, 0)").into());
  }
  db.execute("UPDATE import_progress SET LastProcessedLine = 8, LastProcessedByte = 321 WHERE Provider = 'checkpoint-probe'", [])?;
  let checkpoint: (i64, i64) = db.query_row("SELECT LastProcessedLine, LastProcessedByte FROM import_progress WHERE Provider = 'checkpoint-probe'", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
  if checkpoint != (8, 321) {
    return Err(format!("byte checkpoint update returned {checkpoint:?} instead of (8, 321)").into());
  }
  db.execute("DELETE FROM import_progress WHERE Provider = 'checkpoint-probe'", [])?;

  for (table, expected_columns) in expected {
    let actual_columns = columns(&db, table)?;
    if &actual_columns != expected_columns {
      return Err(format!("runtime columns for `{table}` differ from compile-time schema: runtime={actual_columns:?}, compile_time={expected_columns:?}").into());
    }
    let has_primary_key = {
      let mut statement = db.prepare(&format!("PRAGMA table_info({})", quote(table)))?;
      statement.query_map([], |row| row.get::<_, i64>(5))?.collect::<Result<Vec<_>, _>>()?.into_iter().any(|key| key != 0)
    };
    let indexes = {
      let mut statement = db.prepare(&format!("PRAGMA index_list({})", quote(table)))?;
      statement.query_map([], |row| row.get::<_, i64>(2))?.collect::<Result<Vec<_>, _>>()?
    };
    if !has_primary_key && indexes.iter().all(|unique| *unique == 0) {
      return Err(format!("`{table}` has no uniqueness constraint; checkpoint replay would append duplicate rows").into());
    }
    if indexes.len() > 1 {
      return Err(format!("`{table}` has {} indexes; write-budget reservation assumes at most one index write per row", indexes.len()).into());
    }
    let quoted_columns = actual_columns.iter().map(|column| quote(column)).collect::<Vec<_>>().join(", ");
    let placeholders = vec!["?"; actual_columns.len()].join(", ");
    let assignments = actual_columns.iter().map(|column| format!("{} = excluded.{}", quote(column), quote(column))).collect::<Vec<_>>().join(", ");
    let changed = actual_columns.iter().map(|column| format!("{}.{} IS NOT excluded.{}", quote(table), quote(column), quote(column))).collect::<Vec<_>>().join(" OR ");
    let sql = format!("INSERT INTO {} ({quoted_columns}) VALUES ({placeholders}) ON CONFLICT DO UPDATE SET {assignments} WHERE {changed}", quote(table));
    let probe = (0..actual_columns.len()).map(|index| format!("replay-probe-{index}")).collect::<Vec<_>>();
    db.execute(&sql, params_from_iter(probe.iter()))?;
    let before = db.total_changes();
    db.execute(&sql, params_from_iter(probe.iter()))?;
    let replay_writes = db.total_changes() - before;
    let replay_count: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {}", quote(table)), [], |row| row.get(0))?;
    db.execute(&format!("DELETE FROM {}", quote(table)), [])?;
    if replay_count != 1 || replay_writes != 0 {
      return Err(format!("`{table}` retained {replay_count} rows after identical UPSERT and reported {replay_writes} unnecessary writes").into());
    }
  }
  Ok(())
}

fn validate_provider(provider: &Provider, client: &Client) -> (Vec<String>, usize) {
  let name = &provider.name;
  let mut lines = vec![format!("\n## Provider: `{name}`")];
  let migration = root().join("migrations").join(name).join("0_gtfs_schema.sql");
  if !migration.is_file() {
    lines.push(format!("- ❌ Migration file not found: `{}`", migration.display()));
    return (lines, 1);
  }
  let expected = match schema(&migration) {
    Ok(expected) => expected,
    Err(error) => {
      lines.push(format!("- ❌ Cannot read base schema: {error}"));
      return (lines, 1);
    }
  };
  let mut errors = 0;
  match validate_local_migrations(migration.parent().unwrap_or(Path::new(".")), &expected) {
    Ok(()) => lines.push("- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.".into()),
    Err(error) => { lines.push(format!("- ❌ Local migration chain failed: {error}")); errors += 1; }
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
  let mut filenames =
    archive.file_names().map(str::to_owned).filter(|name| name.ends_with(".txt") && !name.contains("__MACOSX") && !name.rsplit('/').next().unwrap_or_default().starts_with("._")).collect::<Vec<_>>();
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
    if &csv_columns == db_columns {
      lines.push(format!("- ✅ `{table}` matches exactly."));
      continue;
    }
    let csv_set = csv_columns.iter().cloned().collect::<BTreeSet<_>>();
    let db_set = db_columns.iter().cloned().collect::<BTreeSet<_>>();
    let missing = csv_set.difference(&db_set).collect::<Vec<_>>();
    let extra = db_set.difference(&csv_set).collect::<Vec<_>>();
    if missing.is_empty() && extra.is_empty() {
      lines.push(format!("- ❌ `{table}` has a column-order mismatch: migration={db_columns:?}, feed={csv_columns:?}"));
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
