use super::*;
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

#[derive(Clone, Debug)]
pub(crate) struct RecordedQuery {
  pub(crate) sql: String,
  pub(crate) params: Vec<serde_json::Value>,
}

pub(crate) type InMemoryQueryOutcome = Result<Vec<D1Result>, D1AttemptError>;

#[derive(Default)]
pub(crate) struct InMemoryTransport {
  responses: Mutex<VecDeque<InMemoryQueryOutcome>>,
  recorded: Mutex<Vec<RecordedQuery>>,
  database_info: Mutex<Option<D1DatabaseInfo>>,
  database_count: AtomicU64,
  created_databases: Mutex<Vec<(String, String)>>,
  deleted_databases: Mutex<Vec<String>>,
}

impl InMemoryTransport {
  pub(crate) fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  pub(crate) fn push_query_response(&self, results: Vec<D1Result>) {
    self.push_outcome(Ok(results));
  }

  pub(crate) fn push_retryable_error(&self, message: &str, retry_after: Option<Duration>) {
    self.push_outcome(Err(D1AttemptError::retryable(D1Error::ApiError(message.to_owned()), retry_after)));
  }

  pub(crate) fn push_outcome(&self, outcome: InMemoryQueryOutcome) {
    self.responses.lock().unwrap_or_else(PoisonError::into_inner).push_back(outcome);
  }

  pub(crate) fn set_database_info(&self, info: D1DatabaseInfo, total_databases: u64) {
    *self.database_info.lock().unwrap_or_else(PoisonError::into_inner) = Some(info);
    self.database_count.store(total_databases, Ordering::Relaxed);
  }

  pub(crate) fn recorded_queries(&self) -> Vec<RecordedQuery> {
    self.recorded.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  pub(crate) fn deleted_databases(&self) -> Vec<String> {
    self.deleted_databases.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  fn record(&self, queries: &[D1Query<'_>]) -> InMemoryQueryOutcome {
    {
      let mut recorded = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);
      for query in queries {
        recorded.push(RecordedQuery { sql: query.sql.to_owned(), params: query.params.clone() });
      }
    }
    let mut responses = self.responses.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(next) = responses.pop_front() {
      return next;
    }
    let default_results = queries.iter().map(|_| D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read: 0, rows_written: Some(1) }), success: Some(true) }).collect();
    Ok(default_results)
  }
}

impl D1Transport for InMemoryTransport {
  fn query<'a>(&'a self, _db_id: &'a str, query: &'a D1Query<'a>) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
    Box::pin(async move { self.record(std::slice::from_ref(query)) })
  }

  fn query_batch<'a>(&'a self, _db_id: &'a str, queries: &'a [D1Query<'a>]) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
    Box::pin(async move { self.record(queries) })
  }

  fn get_database_info<'a>(&'a self, db_id: &'a str) -> BoxFuture<'a, Result<D1DatabaseInfo, D1Error>> {
    Box::pin(async move {
      let guard = self.database_info.lock().unwrap_or_else(PoisonError::into_inner);
      guard.clone().ok_or_else(|| D1Error::ApiError(format!("In-memory database {db_id} was not configured")))
    })
  }

  fn get_database_count<'a>(&'a self) -> BoxFuture<'a, Result<u64, D1Error>> {
    Box::pin(async move { Ok(self.database_count.load(Ordering::Relaxed)) })
  }

  fn create_database<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, D1Error>> {
    Box::pin(async move {
      let mut created = self.created_databases.lock().unwrap_or_else(PoisonError::into_inner);
      let uuid = format!("in-memory-uuid-{}", created.len() + 1);
      created.push((name.to_owned(), uuid.clone()));
      self.database_count.fetch_add(1, Ordering::Relaxed);
      Ok(uuid)
    })
  }

  fn delete_database<'a>(&'a self, db_id: &'a str) -> BoxFuture<'a, Result<(), D1Error>> {
    Box::pin(async move {
      self.deleted_databases.lock().unwrap_or_else(PoisonError::into_inner).push(db_id.to_owned());
      Ok(())
    })
  }
}

impl D1Client {
  pub(crate) fn with_in_memory(transport: Arc<InMemoryTransport>, max_data_writes: u64, max_metadata_writes: u64) -> Self {
    Self::with_transport(transport, 4, max_data_writes, max_metadata_writes, 3, Duration::from_secs(30), Arc::new(SystemClock))
  }
}

use crate::budget::FixedClock;
use chrono::{TimeZone, Utc};

fn ok_result(rows_read: u64, rows_written: Option<u64>) -> D1Result {
  D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read, rows_written }), success: Some(true) }
}

#[test]
fn omitted_write_metadata_is_not_a_zero_write_response() -> Result<(), serde_json::Error> {
  let result: D1Result = serde_json::from_str(r#"{"meta":{"rows_read":0},"success":true}"#)?;
  assert_eq!(result.rows_written(), None);
  Ok(())
}

#[tokio::test]
async fn exhausted_data_budget_preserves_checkpoint_capacity() -> Result<(), D1Error> {
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_result(0, Some(8))]);
  transport.push_query_response(vec![ok_result(0, Some(1))]);
  let client = D1Client::with_in_memory(transport, 8, CHECKPOINT_WRITE_RESERVE);
  let checkpoint = client.reserve_checkpoint().await?;
  let query = D1Query { sql: "INSERT INTO stops VALUES (1)", params: Vec::new() };
  client.execute_import_batch("db-1", std::slice::from_ref(&query), 8).await?;
  assert!(matches!(client.execute_import_batch("db-1", std::slice::from_ref(&query), 1).await, Err(D1Error::WriteBudgetExhausted { .. })));
  let update = FileProgressUpdate { file: "stops.txt", checkpoint: Checkpoint { line: 10, byte: 120 }, status: FileImportStatus::InProgress };
  client.save_file_progress(checkpoint, "db-1", update).await?;
  assert_eq!(client.budget.metadata_remaining(), CHECKPOINT_WRITE_RESERVE - 1);
  Ok(())
}

#[tokio::test]
async fn typed_progress_round_trip_decodes_status_and_checkpoints() -> Result<(), D1Error> {
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![D1Result {
    results: vec![
      serde_json::json!({
        "FileName": "stops.txt",
        "CRC": "a1b2c3d4",
        "LastProcessedLine": 42,
        "LastProcessedByte": 1024,
        "Status": 1
      }),
      serde_json::json!({
        "FileName": "routes.txt",
        "CRC": "ff00ff00",
        "LastProcessedLine": 10,
        "LastProcessedByte": 256,
        "Status": 0
      }),
      serde_json::json!({
        "FileName": "trips.txt",
        "CRC": "11223344",
        "LastProcessedLine": 500,
        "LastProcessedByte": 8192,
        "Status": 2
      }),
    ],
    ..ok_result(3, Some(0))
  }]);
  let client = D1Client::with_in_memory(transport, 100, 10);
  let progress = client.get_files_progress("db-1").await?;
  assert_eq!(
    progress,
    vec![
      StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "a1b2c3d4".to_owned(), checkpoint: Checkpoint { line: 42, byte: 1024 }, status: FileImportStatus::InProgress },
      StoredFileProgress { file_name: "routes.txt".to_owned(), crc: "ff00ff00".to_owned(), checkpoint: Checkpoint { line: 10, byte: 256 }, status: FileImportStatus::Completed },
      StoredFileProgress { file_name: "trips.txt".to_owned(), crc: "11223344".to_owned(), checkpoint: Checkpoint { line: 500, byte: 8192 }, status: FileImportStatus::Staged },
    ]
  );
  Ok(())
}

#[test]
fn retry_after_accepts_delta_seconds() {
  assert_eq!(parse_retry_after(Some(&HeaderValue::from_static("17"))), Some(Duration::from_secs(17)));
  assert_eq!(parse_retry_after(Some(&HeaderValue::from_static("not-a-number"))), None);
}

#[test]
fn failed_statement_rejects_successful_envelope() {
  let response = D1Response { success: true, result: Some(vec![D1Result { results: Vec::new(), meta: None, success: Some(false) }]), errors: None };
  assert!(validate_query_response(response).is_err());
}

#[test]
fn result_count_must_match_submitted_statements() {
  assert!(validate_result_count(Vec::new(), 1).is_err());
  assert!(validate_result_count(vec![ok_result(0, None)], 1).is_ok());
}

#[test]
fn usage_metadata_is_aggregated() {
  let transport = InMemoryTransport::new();
  let client = D1Client::with_in_memory(transport, 2, 2);
  client.record_usage(&[ok_result(7, Some(3)), ok_result(11, Some(5))]);
  assert_eq!(client.usage(), D1Usage { rows_read: 18, rows_written: 8 });
}

#[tokio::test]
async fn staging_prepare_and_atomic_swap_batches_use_metadata_budget() -> Result<(), D1Error> {
  let transport = InMemoryTransport::new();
  let client = D1Client::with_in_memory(transport.clone(), 0, 100);

  client
    .prepare_staging(
      "db-1",
      &[OrphanStagingCleanup { file: "old.txt", table_name: "old" }],
      &[StagingTableSetup { file: "stops.txt", crc: "crc-1", table_name: "stops", create_sql: "CREATE TABLE \"stops__staging\" (stop_id TEXT PRIMARY KEY) WITHOUT ROWID" }],
    )
    .await?;

  client.commit_provider_swap("db-1", &[StagedTableSwap { file: "stops.txt", table_name: "stops" }], Some("etag-2")).await?;

  let recorded = transport.recorded_queries();
  let sqls = recorded.iter().map(|query| query.sql.as_str()).collect::<Vec<_>>();
  assert_eq!(sqls[0], "DROP TABLE IF EXISTS \"old__staging\"");
  assert_eq!(sqls[1], DELETE_FILE_PROGRESS_SQL);
  assert_eq!(sqls[2], "DROP TABLE IF EXISTS \"stops__staging\"");
  assert_eq!(sqls[3], "CREATE TABLE \"stops__staging\" (stop_id TEXT PRIMARY KEY) WITHOUT ROWID");
  assert_eq!(sqls[4], INIT_FILE_PROGRESS_SQL);
  assert_eq!(sqls[5], "DROP TABLE IF EXISTS \"stops\"");
  assert_eq!(sqls[6], "ALTER TABLE \"stops__staging\" RENAME TO \"stops\"");
  assert_eq!(sqls[7], COMPLETE_FILE_PROGRESS_SQL);
  assert_eq!(sqls[8], SET_DATASET_VERSION_SQL);
  Ok(())
}

#[tokio::test]
async fn commit_provider_swap_reserves_rename_ddl_writes_and_settles_overage_without_failing() -> Result<(), D1Error> {
  let transport = InMemoryTransport::new();
  // Real D1 reports ~14-20+ rows_written per ALTER TABLE RENAME (and may exceed reservation on large schemas).
  transport.push_query_response(vec![ok_result(17, Some(0)), ok_result(81, Some(48)), ok_result(1, Some(1)), ok_result(0, Some(1))]);
  let client = D1Client::with_in_memory(transport, 0, 100);

  client.commit_provider_swap("db-1", &[StagedTableSwap { file: "stops.txt", table_name: "stops" }], Some("etag-2")).await?;
  assert_eq!(client.budget.metadata_remaining(), 50);
  Ok(())
}

#[test]
fn http_d1_transport_base_url_trims_trailing_slash_and_rejects_empty() -> Result<(), D1Error> {
  let runtime = RuntimeConfig {
    csv_concurrency_limit: 1,
    d1_global_concurrency_limit: 1,
    d1_database_concurrency_limit: 1,
    d1_max_databases: 1,
    query_statement_batch_size: 1,
    d1_statements_per_request: 1,
    d1_daily_write_limit: 100_000,
    max_rows_per_workflow: 1,
    max_feed_download_bytes: 1,
    max_uncompressed_feed_bytes: 1,
    max_feed_archive_entries: 256,
    max_csv_record_bytes: 1,
    max_statement_payload_bytes: 1,
    max_temp_feed_storage_bytes: 1,
    db_size_threshold_bytes: 1,
    http_connect_timeout: Duration::from_secs(1),
    http_request_timeout: Duration::from_secs(1),
    d1_query_timeout: Duration::from_secs(1),
    d1_max_query_retries: 3,
    d1_max_retry_delay: Duration::from_secs(30),
  };
  let transport = HttpD1Transport::with_base_url("acct".to_owned(), "tok".to_owned(), "http://127.0.0.1:8799/".to_owned(), &runtime)?;
  assert_eq!(transport.d1_url("/db-1/query"), "http://127.0.0.1:8799/client/v4/accounts/acct/d1/database/db-1/query");
  assert!(HttpD1Transport::with_base_url("acct".to_owned(), "tok".to_owned(), "   ".to_owned(), &runtime).is_err());
  Ok(())
}

#[tokio::test]
async fn execute_import_batch_consumes_estimated_writes_on_transport_error_or_omitted_metadata() -> Result<(), D1Error> {
  let transport = InMemoryTransport::new();
  transport.push_retryable_error("HTTP 503", Some(Duration::ZERO));
  let client = D1Client::with_in_memory(transport.clone(), 2, 10);
  let query = D1Query { sql: "INSERT INTO stops VALUES (1)", params: Vec::new() };
  assert!(matches!(client.execute_import_batch("db-1", std::slice::from_ref(&query), 2).await, Err(D1Error::ApiError(_))));
  assert_eq!(transport.recorded_queries().len(), 1);
  assert_eq!(client.budget.data_remaining(), 0);
  assert!(matches!(client.execute_import_batch("db-1", std::slice::from_ref(&query), 1).await, Err(D1Error::WriteBudgetExhausted { .. })));

  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_result(1, None)]);
  let client = D1Client::with_in_memory(transport, 4, 10);
  let query = D1Query { sql: "INSERT INTO stops VALUES (1)", params: Vec::new() };
  assert!(matches!(client.execute_import_batch("db-1", std::slice::from_ref(&query), 3).await, Err(D1Error::ApiError(_))));
  assert_eq!(client.budget.data_remaining(), 1);
  Ok(())
}

#[tokio::test]
async fn retry_loop_rechecks_daily_window_before_every_retry_attempt() -> Result<(), Box<dyn std::error::Error>> {
  struct ClockAdvancingTransport {
    clock: Arc<FixedClock>,
    attempts: AtomicU64,
  }
  impl D1Transport for ClockAdvancingTransport {
    fn query<'a>(&'a self, _db_id: &'a str, _query: &'a D1Query<'a>) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
      Box::pin(async move {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        if attempt == 0 {
          return Ok(vec![D1Result { results: vec![serde_json::json!({ "Granted": 5_000 })], meta: Some(D1Meta { rows_read: 1, rows_written: Some(1) }), success: Some(true) }]);
        }
        // Advance past 23:59:00 UTC before the retry attempt runs.
        if let Some(past_cutoff) = Utc.with_ymd_and_hms(2026, 9, 12, 23, 59, 0).single() {
          self.clock.set(past_cutoff);
        }
        Err(D1AttemptError::retryable(D1Error::ApiError("transient 503".to_owned()), Some(Duration::ZERO)))
      })
    }
    fn query_batch<'a>(&'a self, _db_id: &'a str, _queries: &'a [D1Query<'a>]) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
      Box::pin(async { Ok(Vec::new()) })
    }
    fn get_database_info<'a>(&'a self, _db_id: &'a str) -> BoxFuture<'a, Result<D1DatabaseInfo, D1Error>> {
      Box::pin(async { Ok(D1DatabaseInfo { name: String::new(), file_size: 0 }) })
    }
    fn get_database_count<'a>(&'a self) -> BoxFuture<'a, Result<u64, D1Error>> {
      Box::pin(async { Ok(1) })
    }
    fn create_database<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<String, D1Error>> {
      Box::pin(async { Ok(String::new()) })
    }
    fn delete_database<'a>(&'a self, _db_id: &'a str) -> BoxFuture<'a, Result<(), D1Error>> {
      Box::pin(async { Ok(()) })
    }
  }

  let start = Utc.with_ymd_and_hms(2026, 9, 12, 23, 58, 55).single().ok_or("invalid timestamp")?;
  let clock = FixedClock::new(start);
  let transport = Arc::new(ClockAdvancingTransport { clock: clock.clone(), attempts: AtomicU64::new(0) });
  let mut client = D1Client::with_transport(transport.clone(), 2, 5_000, METADATA_WRITE_RESERVE, 3, Duration::from_secs(30), clock);
  assert!(client.acquire_daily_budget("db-1", 5_000, 100_000).await?);

  let result = client.query_rows("db-1", D1Query { sql: "SELECT 1", params: Vec::new() }).await;
  assert!(matches!(result, Err(D1Error::WriteBudgetExhausted { .. })));
  // Attempt 0 was acquire_daily_budget, attempt 1 was the first read; retry attempt 2 was blocked by 23:59 UTC guard.
  assert_eq!(transport.attempts.load(Ordering::Relaxed), 2);
  Ok(())
}

#[tokio::test]
async fn query_rows_honors_configured_max_query_retries() -> Result<(), D1Error> {
  let transport = InMemoryTransport::new();
  transport.push_retryable_error("HTTP 503", Some(Duration::ZERO));
  transport.push_retryable_error("HTTP 503", Some(Duration::ZERO));
  let client = D1Client::with_transport(transport.clone(), 2, 100, 100, 1, Duration::ZERO, Arc::new(SystemClock));
  let result = client.query_rows("db-1", D1Query { sql: "SELECT 1", params: Vec::new() }).await;
  assert!(matches!(result, Err(D1Error::ApiError(_))));
  assert_eq!(transport.recorded_queries().len(), 2);
  Ok(())
}
