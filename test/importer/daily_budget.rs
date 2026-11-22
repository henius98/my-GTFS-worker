//! Exercise the production ledger SQL and D1Client daily lease locally; no Cloudflare requests.

use chrono::{DateTime, TimeZone, Utc};
use gtfs_checks::{CheckResult, root};
use importer::budget::{Clock, FixedClock, LEDGER_WRITE_RESERVE, METADATA_WRITE_RESERVE};
use importer::config::{ProviderConfig, ProvidersToml, RuntimeConfig};
use importer::d1::{
  BoxFuture, Checkpoint, D1AttemptError, D1Client, D1DatabaseInfo, D1Error, D1Meta, D1Query, D1Result, D1Transport, FileImportStatus, FileProgressUpdate, StagedTableSwap, StagingTableSetup,
};
use importer::error::ProcessorError;
use importer::feed::{FeedBody, FeedFetcher, FeedResponse, FeedStream};
use importer::schema::SchemaCatalog;
use importer::workflow::run_import;
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params, params_from_iter, types::Value as SqlValue, types::ValueRef};
use std::{
  fs,
  path::Path,
  sync::{Arc, Mutex},
  thread,
};

const TEST_DAILY_WRITE_LIMIT: u64 = 100_000;

struct Budget {
  _directory: tempfile::TempDir,
  db: Connection,
  path: std::path::PathBuf,
  day: Arc<Mutex<String>>,
  acquire_sql: String,
  release_sql: String,
  migration: String,
  limit: i64,
  overhead: i64,
}

impl Budget {
  fn new() -> CheckResult<Self> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("budget.db");
    let day = Arc::new(Mutex::new("2026-09-12".to_owned()));
    let db = Self::connect(&path, &day)?;
    let migration = fs::read_to_string(root().join("migrations/ktmb/0_gtfs_schema.sql"))?;
    db.execute_batch(&migration)?;
    let rename_migration = fs::read_to_string(root().join("migrations/ktmb/20261028_prefix_infrastructure_tables.sql"))?;
    db.execute_batch(&rename_migration)?;
    Ok(Self {
      _directory: directory,
      db,
      path,
      day,
      acquire_sql: fs::read_to_string(root().join("importer/src/sql/acquire_daily_budget.sql"))?,
      release_sql: fs::read_to_string(root().join("importer/src/sql/release_daily_budget.sql"))?,
      migration,
      limit: i64::try_from(TEST_DAILY_WRITE_LIMIT)?,
      overhead: i64::try_from(METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE)?,
    })
  }

  fn connect(path: &Path, day: &Arc<Mutex<String>>) -> CheckResult<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let day = Arc::clone(day);
    db.create_scalar_function("date", 1, FunctionFlags::SQLITE_UTF8, move |_| day.lock().map(|value| value.clone()).map_err(|_| rusqlite::Error::InvalidQuery))?;
    Ok(db)
  }

  fn acquire_on(&self, db: &Connection, amount: i64, day: &str) -> CheckResult<Option<i64>> {
    Ok(db.query_row(&self.acquire_sql, params![amount, self.limit, day, self.overhead], |row| row.get(0)).optional()?)
  }

  fn acquire(&self, amount: i64) -> CheckResult<Option<i64>> {
    self.acquire_on(&self.db, amount, &self.today()?)
  }

  fn release(&self, amount: i64, day: &str) -> CheckResult {
    self.db.execute(&self.release_sql, params![amount, day, amount])?;
    Ok(())
  }

  fn today(&self) -> CheckResult<String> {
    Ok(self.day.lock().map_err(|_| "poisoned day lock")?.clone())
  }

  fn set_day(&self, day: &str) -> CheckResult {
    *self.day.lock().map_err(|_| "poisoned day lock")? = day.to_owned();
    Ok(())
  }

  fn reserved(&self) -> CheckResult<i64> {
    Ok(self.db.query_row("SELECT Reserved FROM \"0_daily_import_budget\" WHERE Id = 1", [], |row| row.get(0))?)
  }
}

struct SqliteD1Transport {
  db: Mutex<Connection>,
}

impl SqliteD1Transport {
  fn new(clock: Arc<FixedClock>) -> CheckResult<Arc<Self>> {
    let db = Connection::open_in_memory()?;
    db.create_scalar_function("date", 1, FunctionFlags::SQLITE_UTF8, move |_| Ok(clock.now_utc().format("%Y-%m-%d").to_string()))?;
    for statement in SchemaCatalog::base_schema_statements("ktmb").ok_or("ktmb base schema missing")? {
      db.execute(statement, [])?;
    }
    Ok(Arc::new(Self { db: Mutex::new(db) }))
  }

  fn reserved(&self) -> CheckResult<i64> {
    let db = self.db.lock().map_err(|_| "poisoned sqlite lock")?;
    Ok(db.query_row("SELECT Reserved FROM \"0_daily_import_budget\" WHERE Id = 1", [], |row| row.get(0))?)
  }

  fn execute_queries(&self, queries: &[D1Query<'_>]) -> Result<Vec<D1Result>, D1AttemptError> {
    let db = self.db.lock().map_err(|_| D1Error::ApiError("poisoned sqlite lock".to_owned()))?;
    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
      let sql_params = query.params.iter().map(json_to_sql).collect::<Vec<_>>();
      let before = db.total_changes();
      let mut stmt = db.prepare(query.sql).map_err(|error| D1Error::ApiError(error.to_string()))?;
      let col_names = stmt.column_names().into_iter().map(str::to_owned).collect::<Vec<_>>();
      let mut rows_out = Vec::new();
      let mut rows = stmt.query(params_from_iter(sql_params.iter())).map_err(|error| D1Error::ApiError(error.to_string()))?;
      while let Some(row) = rows.next().map_err(|error| D1Error::ApiError(error.to_string()))? {
        let mut obj = serde_json::Map::with_capacity(col_names.len());
        for (index, name) in col_names.iter().enumerate() {
          let val = row.get_ref(index).map_err(|error| D1Error::ApiError(error.to_string()))?;
          obj.insert(name.clone(), sql_to_json(val));
        }
        rows_out.push(serde_json::Value::Object(obj));
      }
      let rows_written = db.total_changes().saturating_sub(before);
      results.push(D1Result { meta: Some(D1Meta { rows_read: u64::try_from(rows_out.len()).unwrap_or(u64::MAX), rows_written: Some(rows_written) }), results: rows_out, success: Some(true) });
    }
    Ok(results)
  }
}

impl D1Transport for SqliteD1Transport {
  fn query<'a>(&'a self, _db_id: &'a str, query: &'a D1Query<'a>) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
    Box::pin(async move { self.execute_queries(std::slice::from_ref(query)) })
  }

  fn query_batch<'a>(&'a self, _db_id: &'a str, queries: &'a [D1Query<'a>]) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
    Box::pin(async move { self.execute_queries(queries) })
  }

  fn get_database_info<'a>(&'a self, _db_id: &'a str) -> BoxFuture<'a, Result<D1DatabaseInfo, D1Error>> {
    Box::pin(async { Ok(D1DatabaseInfo { name: "sqlite-test".to_owned(), file_size: 0 }) })
  }

  fn get_database_count<'a>(&'a self) -> BoxFuture<'a, Result<u64, D1Error>> {
    Box::pin(async { Ok(1) })
  }

  fn create_database<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<String, D1Error>> {
    Box::pin(async { Ok("sqlite-created".to_owned()) })
  }

  fn delete_database<'a>(&'a self, _db_id: &'a str) -> BoxFuture<'a, Result<(), D1Error>> {
    Box::pin(async { Ok(()) })
  }
}

fn json_to_sql(value: &serde_json::Value) -> SqlValue {
  match value {
    serde_json::Value::Null => SqlValue::Null,
    serde_json::Value::Bool(boolean) => SqlValue::Integer(if *boolean { 1 } else { 0 }),
    serde_json::Value::Number(number) => {
      if let Some(int) = number.as_i64() {
        SqlValue::Integer(int)
      } else if let Some(uint) = number.as_u64() {
        SqlValue::Integer(i64::try_from(uint).unwrap_or(i64::MAX))
      } else {
        SqlValue::Real(number.as_f64().unwrap_or(0.0))
      }
    }
    serde_json::Value::String(text) => SqlValue::Text(text.clone()),
    other => SqlValue::Text(other.to_string()),
  }
}

fn sql_to_json(value: ValueRef<'_>) -> serde_json::Value {
  match value {
    ValueRef::Null => serde_json::Value::Null,
    ValueRef::Integer(int) => serde_json::json!(int),
    ValueRef::Real(real) => serde_json::json!(real),
    ValueRef::Text(bytes) => serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned()),
    ValueRef::Blob(bytes) => serde_json::json!(bytes),
  }
}

fn utc(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> CheckResult<DateTime<Utc>> {
  Ok(Utc.with_ymd_and_hms(year, month, day, hour, min, sec).single().ok_or("invalid UTC timestamp")?)
}

#[test]
fn crashed_runs_keep_capacity_and_third_run_is_denied() -> CheckResult {
  let budget = Budget::new()?;
  assert!(budget.acquire(budget.limit / 2)?.is_some());
  assert!(budget.acquire(budget.limit / 2)?.is_some());
  assert_eq!(budget.acquire(budget.limit / 2)?, None);
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn returning_unused_capacity_preserves_consumed_writes() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit / 2)?;
  let unused = budget.limit / 2 - 10_000;
  budget.release(unused, &budget.today()?)?;
  assert_eq!(budget.reserved()?, 10_000);
  assert!(budget.acquire(budget.limit / 2)?.is_some());
  assert_eq!(budget.acquire(budget.limit / 2)?, Some(budget.limit / 2 - 10_000));
  assert_eq!(budget.acquire(budget.limit / 2)?, None);
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn full_allowance_and_later_run_use_remaining_capacity() -> CheckResult {
  let budget = Budget::new()?;
  assert_eq!(budget.limit, 100_000);
  assert_eq!(budget.acquire(budget.limit)?, Some(budget.limit));
  assert_eq!(budget.acquire(budget.limit)?, None);
  let unused = budget.limit - 10_000;
  budget.release(unused, &budget.today()?)?;
  assert_eq!(budget.acquire(budget.limit)?, Some(unused));
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn remaining_capacity_must_cover_metadata_and_ledger() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit)?;
  budget.release(budget.overhead, &budget.today()?)?;
  assert_eq!(budget.acquire(budget.limit)?, None);
  assert_eq!(budget.reserved()?, budget.limit - budget.overhead);
  Ok(())
}

#[test]
fn stale_day_cannot_acquire_a_new_lease() -> CheckResult {
  let budget = Budget::new()?;
  let old_day = budget.today()?;
  budget.set_day("2026-09-13")?;
  assert_eq!(budget.acquire_on(&budget.db, budget.limit, &old_day)?, None);
  assert_eq!(budget.reserved()?, 0);
  Ok(())
}

#[test]
fn next_day_resets_and_old_release_cannot_refund_new_day() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit / 2)?;
  let old_day = budget.today()?;
  budget.set_day("2026-09-13")?;
  assert_eq!(budget.acquire(budget.limit)?, Some(budget.limit));
  budget.release(30_000, &old_day)?;
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn concurrent_processes_share_one_allowance() -> CheckResult {
  let budget = Budget::new()?;
  let grants = thread::scope(|scope| {
    let workers = (0..4)
      .map(|_| {
        let path = budget.path.clone();
        let day = Arc::clone(&budget.day);
        let sql = budget.acquire_sql.clone();
        let limit = budget.limit;
        let overhead = budget.overhead;
        scope.spawn(move || -> Result<Option<i64>, String> {
          let result = (|| -> CheckResult<Option<i64>> {
            let db = Budget::connect(&path, &day)?;
            let today = day.lock().map_err(|_| "poisoned day lock")?.clone();
            Ok(db.query_row(&sql, params![40_000, limit, today, overhead], |row| row.get(0)).optional()?)
          })();
          result.map_err(|error| error.to_string())
        })
      })
      .collect::<Vec<_>>();
    let mut grants = Vec::with_capacity(workers.len());
    for worker in workers {
      let result = worker.join().map_err(|_| "worker panicked")?;
      grants.push(result?);
    }
    Ok::<_, Box<dyn std::error::Error>>(grants)
  })?;
  let mut grants = grants.into_iter().flatten().collect::<Vec<_>>();
  grants.sort_unstable();
  assert_eq!(grants, [20_000, 40_000, 40_000]);
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn reapplying_migration_preserves_ledger() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit / 2)?;
  budget.db.execute_batch(&budget.migration)?;
  assert_eq!(budget.reserved()?, budget.limit / 2);
  Ok(())
}

#[tokio::test]
async fn d1_client_daily_budget_lifecycle_refunds_exact_unused_capacity_in_sqlite() -> CheckResult {
  let clock = FixedClock::new(utc(2026, 9, 12, 12, 0, 0)?);
  let transport = SqliteD1Transport::new(clock.clone())?;
  let mut client = D1Client::with_transport(transport.clone(), 2, 5_000, METADATA_WRITE_RESERVE, 3, std::time::Duration::from_secs(30), clock);

  assert!(client.acquire_daily_budget("db-ktmb", 5_000, TEST_DAILY_WRITE_LIMIT).await?);
  assert_eq!(transport.reserved()?, 5_000);

  let staging_table = SchemaCatalog::staging_table_name("stops");
  let create_sql = SchemaCatalog::staging_create_sql("ktmb", "stops").ok_or("missing ktmb stops staging DDL")?;
  client.prepare_staging("db-ktmb", &[], &[StagingTableSetup { file: "stops.txt", crc: "crc-1", table_name: "stops", create_sql }]).await?;
  let insert_sql = SchemaCatalog::positional_insert_sql(&staging_table, &["stop_id", "stop_name"], SchemaCatalog::primary_keys("ktmb", "stops"));
  let batch = D1Query { sql: &insert_sql, params: vec![serde_json::Value::String(r#"[["S1","Alpha"],["S2","Beta"]]"#.to_owned())] };
  let usage = client.execute_import_batch("db-ktmb", &[batch], 4).await?;
  assert_eq!(usage.rows_written, 2);

  let checkpoint = client.reserve_checkpoint().await?;
  client.save_file_progress(checkpoint, "db-ktmb", FileProgressUpdate { file: "stops.txt", checkpoint: Checkpoint { line: 2, byte: 30 }, status: FileImportStatus::Staged }).await?;
  client.commit_provider_swap("db-ktmb", &[StagedTableSwap { file: "stops.txt", table_name: "stops" }], Some("etag-1")).await?;
  client.release_daily_budget().await?;

  // Ledger keeps: LEDGER_WRITE_RESERVE (2) + prepare_staging (1 progress row) + 2 stop inserts (2) + save_file_progress (1) + commit_provider_swap (1 progress + 1 etag) = 8.
  assert_eq!(transport.reserved()?, i64::try_from(LEDGER_WRITE_RESERVE + 6)?);
  Ok(())
}

#[tokio::test]
async fn d1_client_enforces_utc_window_cutoffs_against_real_sqlite() -> CheckResult {
  let clock = FixedClock::new(utc(2026, 9, 12, 23, 57, 59)?);
  let transport = SqliteD1Transport::new(clock.clone())?;
  let mut client = D1Client::with_transport(transport, 2, 5_000, METADATA_WRITE_RESERVE, 3, std::time::Duration::from_secs(30), clock.clone());

  assert!(client.acquire_daily_budget("db-ktmb", 5_000, TEST_DAILY_WRITE_LIMIT).await?);
  let staging_table = SchemaCatalog::staging_table_name("stops");
  let create_sql = SchemaCatalog::staging_create_sql("ktmb", "stops").ok_or("missing ktmb stops staging DDL")?;
  client.prepare_staging("db-ktmb", &[], &[StagingTableSetup { file: "stops.txt", crc: "crc-1", table_name: "stops", create_sql }]).await?;

  let insert_sql = SchemaCatalog::positional_insert_sql(&staging_table, &["stop_id", "stop_name"], SchemaCatalog::primary_keys("ktmb", "stops"));
  let batch = D1Query { sql: &insert_sql, params: vec![serde_json::Value::String(r#"[["S1","Alpha"]]"#.to_owned())] };

  // At 23:58:00 UTC, data writes stop while checkpoint persistence remains allowed.
  clock.set(utc(2026, 9, 12, 23, 58, 0)?);
  assert!(matches!(client.execute_import_batch("db-ktmb", &[batch], 2).await, Err(D1Error::WriteBudgetExhausted { .. })));
  let checkpoint = client.reserve_checkpoint().await?;
  client.save_file_progress(checkpoint, "db-ktmb", FileProgressUpdate { file: "stops.txt", checkpoint: Checkpoint { line: 1, byte: 15 }, status: FileImportStatus::InProgress }).await?;

  // At 23:59:00 UTC, all D1 traffic closes for this lease.
  clock.set(utc(2026, 9, 12, 23, 59, 0)?);
  assert!(matches!(client.get_dataset_version("db-ktmb").await, Err(D1Error::WriteBudgetExhausted { .. })));
  Ok(())
}

struct VecBody(Option<Vec<u8>>);

impl FeedBody for VecBody {
  fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProcessorError>> {
    Box::pin(async move { Ok(self.0.take()) })
  }
}

struct StaticZipFetcher {
  etag: Mutex<String>,
  payload: Mutex<Vec<u8>>,
}

impl StaticZipFetcher {
  fn new(etag: &str, entries: &[(&str, &[u8])]) -> CheckResult<Arc<Self>> {
    Ok(Arc::new(Self { etag: Mutex::new(etag.to_owned()), payload: Mutex::new(build_zip(entries)?) }))
  }

  fn update(&self, etag: &str, entries: &[(&str, &[u8])]) -> CheckResult {
    *self.etag.lock().map_err(|_| "poisoned etag lock")? = etag.to_owned();
    *self.payload.lock().map_err(|_| "poisoned payload lock")? = build_zip(entries)?;
    Ok(())
  }
}

impl FeedFetcher for StaticZipFetcher {
  fn fetch<'a>(&'a self, _url: &'a str, if_none_match: Option<&'a str>) -> BoxFuture<'a, Result<FeedResponse, ProcessorError>> {
    Box::pin(async move {
      let etag = self.etag.lock().map_err(|_| ProcessorError::Invariant("poisoned etag lock".to_owned()))?.clone();
      if if_none_match == Some(etag.as_str()) {
        return Ok(FeedResponse::NotModified);
      }
      let bytes = self.payload.lock().map_err(|_| ProcessorError::Invariant("poisoned payload lock".to_owned()))?.clone();
      let len = u64::try_from(bytes.len()).ok();
      Ok(FeedResponse::Stream(FeedStream { etag, content_length: len, body: Box::new(VecBody(Some(bytes))) }))
    })
  }
}

fn build_zip(entries: &[(&str, &[u8])]) -> CheckResult<Vec<u8>> {
  use std::io::{Cursor, Write};
  let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
  let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
  for (name, content) in entries {
    writer.start_file(*name, options)?;
    writer.write_all(content)?;
  }
  Ok(writer.finish()?.into_inner())
}

fn sqlite_test_runtime(max_rows_per_workflow: u64) -> Arc<RuntimeConfig> {
  Arc::new(RuntimeConfig {
    csv_concurrency_limit: 2,
    d1_global_concurrency_limit: 4,
    d1_database_concurrency_limit: 1,
    d1_max_databases: 10,
    query_statement_batch_size: 2,
    d1_statements_per_request: 2,
    d1_daily_write_limit: TEST_DAILY_WRITE_LIMIT,
    max_rows_per_workflow,
    max_feed_download_bytes: 1024 * 1024,
    max_uncompressed_feed_bytes: 1024 * 1024,
    max_feed_archive_entries: 256,
    max_csv_record_bytes: 1024,
    max_statement_payload_bytes: 4096,
    max_temp_feed_storage_bytes: 1024 * 1024,
    db_size_threshold_bytes: 1024 * 1024,
    http_connect_timeout: std::time::Duration::from_secs(1),
    http_request_timeout: std::time::Duration::from_secs(1),
    d1_query_timeout: std::time::Duration::from_secs(1),
    d1_max_query_retries: 3,
    d1_max_retry_delay: std::time::Duration::from_secs(30),
    workflow_max_duration: None,
  })
}

fn ktmb_providers() -> ProvidersToml {
  ProvidersToml {
    budget_provider: "ktmb".to_owned(),
    providers: vec![ProviderConfig {
      name: "ktmb".to_owned(),
      is_active: true,
      static_url: "https://example.invalid/".to_owned(),
      static_provider: "ktmb".to_owned(),
      database_id: "db-ktmb".to_owned(),
    }],
  }
}

#[tokio::test]
async fn end_to_end_staging_multi_run_atomic_swap_empty_guard_and_orphan_cleanup_in_sqlite() -> CheckResult {
  let clock = FixedClock::new(utc(2026, 9, 12, 12, 0, 0)?);
  let transport = SqliteD1Transport::new(clock.clone())?;

  // Seed existing live rows that must remain untouched until the atomic swap commits.
  {
    let db = transport.db.lock().map_err(|_| "poisoned sqlite lock")?;
    db.execute("INSERT INTO stops (stop_id, stop_name) VALUES ('OLD', 'Legacy Stop')", [])?;
    db.execute("INSERT INTO routes (route_id, route_short_name) VALUES ('R_OLD', 'Legacy Route')", [])?;
    db.execute("INSERT INTO trips (trip_id, route_id, service_id, direction_id) VALUES ('T_OLD', 'R_OLD', 'SVC_OLD', 0)", [])?;
  }

  let fetcher = StaticZipFetcher::new(
    "etag-v1",
    &[
      ("stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\n"),
      ("routes.txt", b"route_id,route_short_name\nR1,Express\n"),
      ("trips.txt", b"trip_id,route_id,service_id,direction_id\nT1,R1,SVC1,0\n"),
    ],
  )?;

  // Run 1: Row cap of 2 rows stages routes/trips or partial stops without swapping any live table yet.
  let client1 = D1Client::with_transport(transport.clone(), 4, 5_000, METADATA_WRITE_RESERVE, 3, std::time::Duration::from_secs(30), clock.clone());
  let summary1 = run_import(ktmb_providers(), sqlite_test_runtime(2), client1, fetcher.clone(), "unused.toml").await?;
  assert!(!summary1.has_error);
  assert_eq!(summary1.remaining_providers, 1);

  {
    let db = transport.db.lock().map_err(|_| "poisoned sqlite lock")?;
    let live_stops: Vec<String> = {
      let mut stmt = db.prepare("SELECT stop_id FROM stops ORDER BY stop_id")?;
      stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?
    };
    assert_eq!(live_stops, vec!["OLD".to_owned()], "live stops must stay unchanged before all tables finish staging");
    let live_routes: Vec<String> = {
      let mut stmt = db.prepare("SELECT route_id FROM routes ORDER BY route_id")?;
      stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?
    };
    assert_eq!(live_routes, vec!["R_OLD".to_owned()], "live routes must stay unchanged before all tables finish staging");
  }

  // Feed updates mid-import: trips.txt disappears (orphan!), routes.txt becomes empty (0 data rows -> keep R_OLD!), stops.txt stays v1.
  fetcher.update("etag-v2", &[("stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\n"), ("routes.txt", b"route_id,route_short_name\n")])?;

  // Run 2: Finishes stops.txt, skips empty routes.txt (preserving R_OLD), drops orphan trips__staging, swaps stops atomically.
  let client2 = D1Client::with_transport(transport.clone(), 4, 5_000, METADATA_WRITE_RESERVE, 3, std::time::Duration::from_secs(30), clock.clone());
  let summary2 = run_import(ktmb_providers(), sqlite_test_runtime(20), client2, fetcher.clone(), "unused.toml").await?;
  assert!(!summary2.has_error);
  assert_eq!(summary2.remaining_providers, 0);

  {
    let db = transport.db.lock().map_err(|_| "poisoned sqlite lock")?;
    let live_stops: Vec<String> = {
      let mut stmt = db.prepare("SELECT stop_id FROM stops ORDER BY stop_id")?;
      stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?
    };
    assert_eq!(live_stops, vec!["S1".to_owned(), "S2".to_owned(), "S3".to_owned()]);

    let live_routes: Vec<String> = {
      let mut stmt = db.prepare("SELECT route_id FROM routes ORDER BY route_id")?;
      stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?
    };
    assert_eq!(live_routes, vec!["R_OLD".to_owned()], "empty routes.txt must not wipe existing live routes");

    let live_trips: Vec<String> = {
      let mut stmt = db.prepare("SELECT trip_id FROM trips ORDER BY trip_id")?;
      stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?
    };
    assert_eq!(live_trips, vec!["T_OLD".to_owned()], "removed trips.txt must leave live trips untouched");

    let leftover_staging: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name LIKE '%__staging'", [], |row| row.get(0))?;
    assert_eq!(leftover_staging, 0, "all staging tables must be swapped or dropped after completion");

    let statuses: Vec<(String, i64)> = {
      let mut stmt = db.prepare("SELECT FileName, Status FROM \"0_import_progress\" ORDER BY FileName")?;
      stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?
    };
    assert_eq!(statuses, vec![("routes.txt".to_owned(), 0), ("stops.txt".to_owned(), 0)]);

    let version: String = db.query_row("SELECT ETag FROM \"0_dataset_versions\" ORDER BY rowid DESC LIMIT 1", [], |row| row.get(0))?;
    assert_eq!(version, "etag-v2");
  }

  // Run 3: Unchanged feed (304 Not Modified) with all Status = 0 skips completely with 0 rows processed.
  let client3 = D1Client::with_transport(transport, 4, 5_000, METADATA_WRITE_RESERVE, 3, std::time::Duration::from_secs(30), clock);
  let summary3 = run_import(ktmb_providers(), sqlite_test_runtime(20), client3, fetcher, "unused.toml").await?;
  assert!(!summary3.has_error);
  assert_eq!(summary3.total_rows_processed, 0);
  assert_eq!(summary3.waves_executed, 0);
  Ok(())
}
