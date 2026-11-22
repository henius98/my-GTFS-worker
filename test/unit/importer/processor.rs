use super::*;
use crate::d1::{D1DatabaseInfo, D1Meta, D1Query, D1Result, tests::InMemoryTransport};
use crate::feed::{
  TempFeedStorage,
  tests::{InMemoryFeedFetcher, RecordedFeedRequest, zip_bytes},
};
use std::time::Duration;

fn test_runtime_config() -> RuntimeConfig {
  RuntimeConfig {
    csv_concurrency_limit: 1,
    d1_global_concurrency_limit: 2,
    d1_database_concurrency_limit: 1,
    d1_max_databases: 10,
    query_statement_batch_size: 10,
    d1_statements_per_request: 2,
    d1_daily_write_limit: 100_000,
    max_rows_per_workflow: 10,
    max_feed_download_bytes: 1024 * 1024,
    max_uncompressed_feed_bytes: 1024 * 1024,
    max_feed_archive_entries: 256,
    max_csv_record_bytes: 1024,
    max_statement_payload_bytes: 4096,
    max_temp_feed_storage_bytes: 1024 * 1024,
    db_size_threshold_bytes: 1024 * 1024,
    http_connect_timeout: Duration::from_secs(1),
    http_request_timeout: Duration::from_secs(1),
    d1_query_timeout: Duration::from_secs(1),
    d1_max_query_retries: 3,
    d1_max_retry_delay: Duration::from_secs(30),
    workflow_max_duration: None,
  }
}

fn test_provider(name: &str, database_id: &str) -> ProviderConfig {
  ProviderConfig { name: name.to_owned(), is_active: true, static_url: "https://example.invalid/".to_owned(), static_provider: name.to_owned(), database_id: database_id.to_owned() }
}

fn ok_write_result(rows_written: u64) -> D1Result {
  D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read: 0, rows_written: Some(rows_written) }), success: Some(true) }
}

fn ok_rows_result(results: Vec<serde_json::Value>) -> D1Result {
  D1Result { meta: Some(D1Meta { rows_read: u64::try_from(results.len()).unwrap_or(u64::MAX), rows_written: Some(0) }), results, success: Some(true) }
}

#[tokio::test]
async fn process_csv_file_inserts_into_staging_and_commits_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("session-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\n")?;
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_write_result(2)]);
  transport.push_query_response(vec![ok_write_result(1)]);

  let client = D1Client::with_in_memory(transport.clone(), 100, 10);
  let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 2, false, ShutdownSignal::new());
  let outcome = processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

  assert_eq!(outcome.completion, CsvFileCompletion::InProgress);
  assert!(!outcome.write_budget_exhausted);
  assert_eq!(outcome.checkpoint.line, 2);
  let recorded = transport.recorded_queries();
  assert_eq!(recorded.len(), 2);
  assert!(recorded[0].sql.starts_with("INSERT INTO \"stops__staging\""));
  assert!(recorded[1].sql.starts_with("UPDATE \"0_import_progress\""));
  assert_eq!(recorded[1].params[2], serde_json::json!(1));
  Ok(())
}

#[tokio::test]
async fn process_csv_file_marks_completed_non_empty_file_as_staged() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("staged-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n")?;
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_write_result(2)]);
  transport.push_query_response(vec![ok_write_result(1)]);

  let client = D1Client::with_in_memory(transport.clone(), 100, 10);
  let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new());
  let outcome = processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

  assert_eq!(outcome.completion, CsvFileCompletion::Staged);
  assert_eq!(outcome.checkpoint.line, 2);
  let recorded = transport.recorded_queries();
  assert_eq!(recorded.len(), 2);
  assert!(recorded[0].sql.starts_with("INSERT INTO \"stops__staging\""));
  assert!(recorded[1].sql.starts_with("UPDATE \"0_import_progress\""));
  assert_eq!(recorded[1].params[2], serde_json::json!(2));
  Ok(())
}

#[tokio::test]
async fn process_csv_file_skips_empty_csv_drops_staging_and_marks_completed() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("empty-csv-test", "stops.txt", b"stop_id,stop_name\n")?;
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_write_result(1), ok_write_result(1)]);

  let client = D1Client::with_in_memory(transport.clone(), 100, 10);
  let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new());
  let outcome = processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

  assert_eq!(outcome.completion, CsvFileCompletion::SkippedEmpty);
  assert_eq!(outcome.checkpoint.line, 0);
  let recorded = transport.recorded_queries();
  assert_eq!(recorded.len(), 2);
  assert_eq!(recorded[0].sql, "DROP TABLE IF EXISTS \"stops__staging\"");
  assert!(recorded[1].sql.starts_with("UPDATE \"0_import_progress\""));
  assert_eq!(recorded[1].params[2], serde_json::json!(0));
  Ok(())
}

#[tokio::test]
async fn process_csv_file_propagates_first_batch_error_and_persists_partial_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("batch-err-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\nS4,Delta\n")?;
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_write_result(1)]);
  transport.push_outcome(Err(D1Error::ApiError("injected batch failure".to_owned()).into()));
  transport.push_query_response(vec![ok_write_result(1)]);

  let runtime = Arc::new(RuntimeConfig { query_statement_batch_size: 1, d1_statements_per_request: 1, d1_database_concurrency_limit: 1, ..test_runtime_config() });
  let client = D1Client::with_in_memory(transport.clone(), 100, 10);
  let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), runtime, 10, false, ShutdownSignal::new());
  let error = match processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await {
    Ok(_) => panic!("failing D1 batch must return an error"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("injected batch failure"), "unexpected error: {error}");

  let progress_updates = transport.recorded_queries().into_iter().filter(|query| query.sql.starts_with("UPDATE \"0_import_progress\"")).collect::<Vec<_>>();
  assert_eq!(progress_updates.len(), 1);
  assert_eq!(progress_updates[0].params[0], serde_json::json!(1));
  assert_eq!(progress_updates[0].params[2], serde_json::json!(1));
  Ok(())
}

#[tokio::test]
async fn transport_failure_consumes_full_write_reservation() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("ambiguous-retry-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\n")?;
  let transport = InMemoryTransport::new();
  transport.push_retryable_error("injected 503", Some(Duration::ZERO));

  // ktmb.stops is WITHOUT ROWID (1 write/row); an ambiguous transport failure consumes that 1-write reservation
  // and skips the no-op checkpoint save because progress did not advance.
  let client = D1Client::with_in_memory(transport.clone(), 1, 10);
  let processor = ProviderProcessor::new(client.clone(), test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 1, false, ShutdownSignal::new());
  assert!(processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await.is_err());
  assert!(!transport.recorded_queries().iter().any(|query| query.sql.starts_with("UPDATE \"0_import_progress\"")));
  let probe = D1Query { sql: "INSERT INTO stops__staging VALUES (1)", params: Vec::new() };
  assert!(matches!(client.execute_import_batch("db-ktmb", &[probe], 1).await, Err(D1Error::WriteBudgetExhausted { requested: 1, remaining: 0 })));
  Ok(())
}

#[tokio::test]
async fn staging_group_shrinks_to_fit_remaining_write_budget_without_overshoot() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("shrink-budget-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\nS4,Delta\nS5,Epsilon\n")?;
  let transport = InMemoryTransport::new();
  // Group has 4 statements of 1 row each (ktmb.stops WITHOUT ROWID -> estimated 4 writes). Remaining budget is 3 writes:
  // shrinks to 3 statements (estimated 3 writes, settles to 3 -> 0 remaining), and stops before the 4th row (estimated 1 > 0).
  transport.push_query_response(vec![ok_write_result(1), ok_write_result(1), ok_write_result(1)]);
  transport.push_query_response(vec![ok_write_result(1)]); // save_file_progress

  let runtime = Arc::new(RuntimeConfig { query_statement_batch_size: 1, d1_statements_per_request: 4, d1_database_concurrency_limit: 1, ..test_runtime_config() });
  let client = D1Client::with_in_memory(transport.clone(), 3, 10);
  let processor = ProviderProcessor::new(client.clone(), test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), runtime, 10, false, ShutdownSignal::new());
  let outcome = processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

  assert_eq!(outcome.completion, CsvFileCompletion::InProgress);
  assert!(outcome.write_budget_exhausted);
  assert_eq!(outcome.checkpoint.line, 3);
  assert_eq!(client.usage().rows_written, 4);
  Ok(())
}

#[tokio::test]
async fn concurrent_upload_reserves_budget_in_file_order_so_tail_cannot_stall_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("ordered-budget-test", "stops.txt", b"stop_id,stop_name\nS1,A\nS2,B\nS3,C\nS4,D\nS5,E\n")?;
  let transport = InMemoryTransport::new();
  // Group 0 has 2 statements of 2 rows (ktmb.stops WITHOUT ROWID -> estimated 4 writes). Remaining budget is 3 writes:
  // Group 1 (tail, 1 row = 1 est write) must NOT leapfrog Group 0; Group 0 shrinks to 1 statement (2 rows, est 2 -> settles 2 -> 1 left),
  // then stops before its second statement (2 rows, est 2 > 1).
  transport.push_query_response(vec![ok_write_result(2)]);
  transport.push_query_response(vec![ok_write_result(1)]); // save_file_progress

  let runtime = Arc::new(RuntimeConfig { query_statement_batch_size: 2, d1_statements_per_request: 2, d1_database_concurrency_limit: 4, ..test_runtime_config() });
  let client = D1Client::with_in_memory(transport.clone(), 3, 10);
  let prepared = PreparedProvider {
    provider: test_provider("ktmb", "db-ktmb"),
    download: DownloadedFeed { size: feed.reserved_bytes(), file: feed, remote_etag: "etag-1".to_owned() },
    files: vec![PreparedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default(), extracted_csv: None }],
    staged_files: Vec::new(),
  };

  let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(2)), runtime, 10, false, ShutdownSignal::new()).await?;
  assert!(outcome.write_budget_exhausted);
  assert_eq!(outcome.rows_processed, 2);
  let remaining = outcome.remaining.ok_or("expected partial provider")?;
  assert_eq!(remaining.files[0].checkpoint.line, 2);
  Ok(())
}

#[tokio::test]
async fn zero_row_budget_exhaustion_skips_noop_checkpoint_update_and_refunds_metadata() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("noop-checkpoint-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n")?;
  let transport = InMemoryTransport::new();
  // 0 data writes remain -> exhausts immediately with 0 committed rows and skips checkpoint write.
  let client = D1Client::with_in_memory(transport.clone(), 0, 6);
  let processor = ProviderProcessor::new(client.clone(), test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new());
  let outcome = processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

  assert_eq!(outcome.completion, CsvFileCompletion::InProgress);
  assert!(outcome.write_budget_exhausted);
  assert_eq!(outcome.checkpoint, Checkpoint::default());
  assert!(transport.recorded_queries().is_empty(), "unchanged InProgress checkpoint must not send UPDATE 0_import_progress");
  // Metadata reservation (6 writes) was settled to 0, so another reserve_checkpoint must still succeed.
  assert!(client.reserve_checkpoint().await.is_ok());
  Ok(())
}

#[tokio::test]
async fn batch_size_adapts_below_configured_batch_size_when_remaining_write_budget_is_smaller() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("adaptive-batch-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\nS4,Delta\nS5,Epsilon\n")?;
  let transport = InMemoryTransport::new();
  // Configured batch size is 10 rows, but only 3 data writes remain -> producer caps batch to 3 rows,
  // commits all 3 rows immediately, saves checkpoint at line 3, and drains data budget to 0.
  transport.push_query_response(vec![ok_write_result(3)]);
  transport.push_query_response(vec![ok_write_result(1)]); // save_file_progress
  let client = D1Client::with_in_memory(transport.clone(), 3, 10);
  let processor = ProviderProcessor::new(client.clone(), test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new());
  let outcome = processor.process_csv_file(feed, None, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

  assert_eq!(outcome.completion, CsvFileCompletion::InProgress);
  assert!(outcome.write_budget_exhausted);
  assert_eq!(outcome.checkpoint.line, 3);
  assert_eq!(client.remaining_data_writes(), 0);
  let recorded = transport.recorded_queries();
  assert_eq!(recorded.len(), 2);
  assert!(recorded[0].sql.starts_with("INSERT INTO \"stops__staging\""));
  assert_eq!(recorded[0].params[0], serde_json::json!(r#"[["S1","Alpha"],["S2","Beta"],["S3","Gamma"]]"#));
  Ok(())
}

#[test]
fn classify_file_progress_handles_new_changed_completed_staged_and_in_progress() {
  let checkpoint = Checkpoint { line: 42, byte: 512 };
  assert_eq!(classify_file_progress(None, "stops.txt", "crc-1", "ktmb"), FileResumeAction::StartFresh);
  let changed = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-0".to_owned(), checkpoint, status: FileImportStatus::Staged };
  assert_eq!(classify_file_progress(Some(&changed), "stops.txt", "crc-1", "ktmb"), FileResumeAction::StartFresh);
  let completed = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-1".to_owned(), checkpoint, status: FileImportStatus::Completed };
  assert_eq!(classify_file_progress(Some(&completed), "stops.txt", "crc-1", "ktmb"), FileResumeAction::SkipCompleted);
  let staged = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-1".to_owned(), checkpoint, status: FileImportStatus::Staged };
  assert_eq!(classify_file_progress(Some(&staged), "stops.txt", "crc-1", "ktmb"), FileResumeAction::AlreadyStaged);
  let partial = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-1".to_owned(), checkpoint, status: FileImportStatus::InProgress };
  assert_eq!(classify_file_progress(Some(&partial), "stops.txt", "crc-1", "ktmb"), FileResumeAction::ResumeInProgress(checkpoint));
}

#[tokio::test]
async fn prepare_provider_skips_on_304_when_complete_and_refetches_when_incomplete() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\n")])?;
  let mut reader = zip::ZipArchive::new(std::io::Cursor::new(&archive))?;
  let stops_crc = format!("{:08x}", reader.by_name("stops.txt")?.crc32());

  // 304 Not Modified with all files complete -> skips without downloading.
  let transport1 = InMemoryTransport::new();
  transport1.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport1.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "ETag": "\"v1\"" })])]);
  transport1.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "FileName": "stops.txt", "CRC": stops_crc, "LastProcessedLine": 1, "LastProcessedByte": 27, "Status": 0 })])]);
  let client1 = D1Client::with_in_memory(transport1.clone(), 100, 100);
  let rotator1 = DatabaseRotator::new(client1.clone(), "unused.toml", 1_000, 10);
  let fetcher1 = InMemoryFeedFetcher::new();
  fetcher1.push_not_modified();
  let store1 = FeedStore::new(fetcher1.clone(), Arc::new(TempFeedStorage::new(1024)), 1024, 1024, 256);
  assert!(prepare_provider(&client1, &rotator1, &store1, test_provider("ktmb", "db-ktmb")).await?.is_none());
  assert_eq!(fetcher1.recorded_requests(), vec![RecordedFeedRequest { url: "https://example.invalid/ktmb".to_owned(), if_none_match: Some("\"v1\"".to_owned()) }]);
  assert_eq!(transport1.recorded_queries().len(), 2);

  // 304 Not Modified with incomplete files -> refetches unconditionally and reuses the single 0_import_progress query.
  let transport2 = InMemoryTransport::new();
  transport2.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport2.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "ETag": "\"v1\"" })])]);
  transport2.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "FileName": "stops.txt", "CRC": stops_crc, "LastProcessedLine": 0, "LastProcessedByte": 0, "Status": 1 })])]);
  let client2 = D1Client::with_in_memory(transport2.clone(), 100, 100);
  let rotator2 = DatabaseRotator::new(client2.clone(), "unused.toml", 1_000, 10);
  let fetcher2 = InMemoryFeedFetcher::new();
  fetcher2.push_not_modified();
  fetcher2.push_stream("\"v1\"", Some(u64::try_from(archive.len())?), vec![archive]);
  let store2 = FeedStore::new(fetcher2.clone(), Arc::new(TempFeedStorage::new(1024 * 1024)), 1024 * 1024, 1024 * 1024, 256);
  let prepared = prepare_provider(&client2, &rotator2, &store2, test_provider("ktmb", "db-ktmb")).await?.ok_or("expected resumed provider")?;
  assert_eq!(prepared.files.len(), 1);
  assert_eq!(transport2.recorded_queries().len(), 2);
  assert_eq!(
    fetcher2.recorded_requests(),
    vec![
      RecordedFeedRequest { url: "https://example.invalid/ktmb".to_owned(), if_none_match: Some("\"v1\"".to_owned()) },
      RecordedFeedRequest { url: "https://example.invalid/ktmb".to_owned(), if_none_match: None },
    ]
  );
  Ok(())
}

#[tokio::test]
async fn prepare_provider_creates_staging_cleans_orphans_and_reuses_staged_files() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\n"), ("routes.txt", b"route_id,route_short_name\nR1,Express\n")])?;
  let mut reader = zip::ZipArchive::new(std::io::Cursor::new(&archive))?;
  let stops_crc = format!("{:08x}", reader.by_name("stops.txt")?.crc32());
  let routes_crc = format!("{:08x}", reader.by_name("routes.txt")?.crc32());

  // Case 1: stops.txt is new, routes.txt is already STAGED (Status = 2), trips.txt is an orphan (Status = 1, missing from ZIP).
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport.push_query_response(vec![ok_rows_result(Vec::new())]); // get_dataset_version
  transport.push_query_response(vec![ok_rows_result(vec![
    serde_json::json!({ "FileName": "routes.txt", "CRC": routes_crc, "LastProcessedLine": 1, "LastProcessedByte": 35, "Status": 2 }),
    serde_json::json!({ "FileName": "trips.txt", "CRC": "old-crc", "LastProcessedLine": 5, "LastProcessedByte": 100, "Status": 1 }),
  ])]); // get_files_progress
  let client = D1Client::with_in_memory(transport.clone(), 100, 100);
  let rotator = DatabaseRotator::new(client.clone(), "unused.toml", 1_000, 10);
  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-new", Some(u64::try_from(archive.len())?), vec![archive.clone()]);
  let feed_store = FeedStore::new(fetcher, Arc::new(TempFeedStorage::new(1024 * 1024)), 1024 * 1024, 1024 * 1024, 256);

  let prepared = prepare_provider(&client, &rotator, &feed_store, test_provider("ktmb", "db-ktmb")).await?.ok_or("expected PreparedProvider")?;
  assert_eq!(prepared.name(), "ktmb");
  assert_eq!(prepared.files.len(), 1);
  assert_eq!(prepared.files[0].csv_file, "stops.txt");
  assert_eq!(prepared.staged_files, vec![StagedFile { csv_file: "routes.txt".to_owned(), table_name: "routes".to_owned() }]);
  let queries = transport.recorded_queries();
  assert!(queries.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"trips__staging\""));
  assert!(queries.iter().any(|query| query.sql.starts_with("DELETE FROM \"0_import_progress\"") && query.params == vec![serde_json::json!("trips.txt")]));
  assert!(queries.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"stops__staging\""));
  assert!(queries.iter().any(|query| query.sql.starts_with("CREATE TABLE \"stops__staging\"")));
  assert!(queries.iter().any(|query| query.sql.starts_with("INSERT INTO \"0_import_progress\"") && query.params[0] == serde_json::json!("stops.txt")));

  // Case 2: All files already completed (Status = 0) with matching CRC -> records dataset version and returns None.
  let transport2 = InMemoryTransport::new();
  transport2.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport2.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "ETag": "etag-old" })])]);
  transport2.push_query_response(vec![ok_rows_result(vec![
    serde_json::json!({ "FileName": "stops.txt", "CRC": stops_crc, "LastProcessedLine": 1, "LastProcessedByte": 27, "Status": 0 }),
    serde_json::json!({ "FileName": "routes.txt", "CRC": routes_crc, "LastProcessedLine": 1, "LastProcessedByte": 35, "Status": 0 }),
  ])]);
  transport2.push_query_response(vec![ok_write_result(1)]); // set_dataset_version
  let client2 = D1Client::with_in_memory(transport2.clone(), 100, 100);
  let rotator2 = DatabaseRotator::new(client2.clone(), "unused.toml", 1_000, 10);
  let fetcher2 = InMemoryFeedFetcher::new();
  fetcher2.push_stream("etag-2", Some(u64::try_from(archive.len())?), vec![archive]);
  let feed_store2 = FeedStore::new(fetcher2, Arc::new(TempFeedStorage::new(1024 * 1024)), 1024 * 1024, 1024 * 1024, 256);

  let skipped = prepare_provider(&client2, &rotator2, &feed_store2, test_provider("ktmb", "db-ktmb")).await?;
  assert!(skipped.is_none());
  assert!(transport2.recorded_queries().iter().any(|query| query.sql.starts_with("INSERT INTO \"0_dataset_versions\"") && query.params == vec![serde_json::json!("etag-2")]));
  Ok(())
}

#[tokio::test]
async fn process_prepared_provider_commits_atomic_swap_and_preserves_empty_table() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entries("provider-swap", &[("stops.txt", b"stop_id,stop_name\nS1,Alpha\n"), ("routes.txt", b"route_id,route_short_name\n")])?;
  let download = DownloadedFeed { size: feed.reserved_bytes(), file: feed, remote_etag: "etag-2".to_owned() };

  let transport = InMemoryTransport::new();
  let client = D1Client::with_in_memory(transport.clone(), 100, 100);
  let prepared = PreparedProvider {
    provider: test_provider("ktmb", "db-ktmb"),
    download,
    files: vec![
      PreparedFile { csv_file: "routes.txt".to_owned(), table_name: "routes".to_owned(), db_columns: &["route_id", "route_short_name"], checkpoint: Checkpoint::default(), extracted_csv: None },
      PreparedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default(), extracted_csv: None },
    ],
    staged_files: vec![StagedFile { csv_file: "agency.txt".to_owned(), table_name: "agency".to_owned() }],
  };

  let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(2)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new()).await?;
  assert!(outcome.remaining.is_none());
  assert_eq!(outcome.rows_processed, 1);
  assert!(!outcome.write_budget_exhausted);

  let recorded = transport.recorded_queries();
  assert!(recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"routes__staging\""));
  assert!(!recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"routes\""));
  assert!(recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"agency\""));
  assert!(recorded.iter().any(|query| query.sql == "ALTER TABLE \"agency__staging\" RENAME TO \"agency\""));
  assert!(recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"stops\""));
  assert!(recorded.iter().any(|query| query.sql == "ALTER TABLE \"stops__staging\" RENAME TO \"stops\""));
  assert!(recorded.iter().any(|query| query.sql.starts_with("INSERT INTO \"0_dataset_versions\"") && query.params == vec![serde_json::json!("etag-2")]));
  Ok(())
}

#[tokio::test]
async fn swap_succeeds_from_metadata_reserve_even_when_data_budget_is_exhausted() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("swap-metadata-reserve-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\n")?;
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_write_result(2)]); // stops__staging insert exhausts the 2-write data pool
  transport.push_query_response(vec![ok_write_result(1)]); // save_file_progress (Status = 2)
  // commit_provider_swap runs from metadata reserve (100 writes) and uses default 1-write responses.
  let client = D1Client::with_in_memory(transport.clone(), 2, 100);
  let prepared = PreparedProvider {
    provider: test_provider("ktmb", "db-ktmb"),
    download: DownloadedFeed { size: feed.reserved_bytes(), file: feed, remote_etag: "etag-2".to_owned() },
    files: vec![PreparedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default(), extracted_csv: None }],
    staged_files: Vec::new(),
  };

  let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new()).await?;
  assert!(!outcome.write_budget_exhausted);
  assert!(outcome.remaining.is_none());
  let recorded = transport.recorded_queries();
  assert!(recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"stops\""));
  assert!(recorded.iter().any(|query| query.sql == "ALTER TABLE \"stops__staging\" RENAME TO \"stops\""));
  assert!(recorded.iter().any(|query| query.sql.starts_with("INSERT INTO \"0_dataset_versions\"") && query.params == vec![serde_json::json!("etag-2")]));
  Ok(())
}

#[tokio::test]
async fn swap_defers_cleanly_when_metadata_budget_is_exhausted() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("swap-defer-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\n")?;
  let transport = InMemoryTransport::new();
  // 6 metadata writes: enough for reserve_checkpoint (6 -> settles to 1, leaving 5), too small for swap (4 statements * 4 = 16).
  let client = D1Client::with_in_memory(transport.clone(), 100, 6);
  let prepared = PreparedProvider {
    provider: test_provider("ktmb", "db-ktmb"),
    download: DownloadedFeed { size: feed.reserved_bytes(), file: feed, remote_etag: "etag-2".to_owned() },
    files: vec![PreparedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default(), extracted_csv: None }],
    staged_files: Vec::new(),
  };

  let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, ShutdownSignal::new()).await?;
  assert!(outcome.write_budget_exhausted);
  let remaining = outcome.remaining.ok_or("expected deferred staged swap")?;
  assert!(remaining.files.is_empty());
  assert_eq!(remaining.staged_files, vec![StagedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned() }]);
  let recorded = transport.recorded_queries();
  assert!(!recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"stops\""));
  assert!(!recorded.iter().any(|query| query.sql.starts_with("INSERT INTO \"0_dataset_versions\"")));
  Ok(())
}

#[tokio::test]
async fn process_prepared_provider_preserves_remaining_work_when_shutdown_is_triggered() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("shutdown-provider-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n")?;
  let transport = InMemoryTransport::new();
  let client = D1Client::with_in_memory(transport.clone(), 100, 100);
  let prepared = PreparedProvider {
    provider: test_provider("ktmb", "db-ktmb"),
    download: DownloadedFeed { size: feed.reserved_bytes(), file: feed, remote_etag: "etag-1".to_owned() },
    files: vec![PreparedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default(), extracted_csv: None }],
    staged_files: Vec::new(),
  };
  let shutdown = ShutdownSignal::new();
  shutdown.trigger();

  let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10, false, shutdown).await?;
  assert!(outcome.shutdown_requested);
  assert_eq!(outcome.rows_processed, 0);
  assert!(outcome.remaining.is_some());
  assert!(transport.recorded_queries().is_empty());
  Ok(())
}

#[tokio::test]
async fn per_database_concurrency_limit_caps_in_flight_batches_below_global_limit() -> Result<(), Box<dyn std::error::Error>> {
  use crate::budget::SystemClock;
  use crate::d1::{BoxFuture, D1AttemptError, D1Transport};
  use std::sync::atomic::AtomicUsize;

  struct ConcurrencyMeasuringTransport {
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
  }
  impl D1Transport for ConcurrencyMeasuringTransport {
    fn query<'a>(&'a self, _db_id: &'a str, _query: &'a D1Query<'a>) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
      Box::pin(async { Ok(vec![ok_write_result(1)]) })
    }
    fn query_batch<'a>(&'a self, _db_id: &'a str, queries: &'a [D1Query<'a>]) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
      Box::pin(async move {
        let is_insert = queries.iter().any(|query| query.sql.starts_with("INSERT INTO \""));
        if is_insert {
          let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
          self.max_in_flight.fetch_max(active, Ordering::SeqCst);
          tokio::time::sleep(Duration::from_millis(15)).await;
          self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(queries.iter().map(|_| ok_write_result(1)).collect())
      })
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

  let feed = TemporaryFeed::from_zip_entries(
    "per-db-cap-test",
    &[("stops.txt", b"stop_id,stop_name\nS1,A\nS2,B\nS3,C\nS4,D\nS5,E\nS6,F\n"), ("routes.txt", b"route_id,route_short_name\nR1,A\nR2,B\nR3,C\nR4,D\nR5,E\nR6,F\n")],
  )?;
  let transport = Arc::new(ConcurrencyMeasuringTransport { in_flight: AtomicUsize::new(0), max_in_flight: AtomicUsize::new(0) });
  // Global D1 concurrency = 8, per-database concurrency = 2.
  let client = D1Client::with_transport(transport.clone(), 8, 1_000, 1_000, 1, Duration::from_secs(1), Arc::new(SystemClock));
  let runtime = Arc::new(RuntimeConfig {
    csv_concurrency_limit: 4,
    d1_global_concurrency_limit: 8,
    d1_database_concurrency_limit: 2,
    query_statement_batch_size: 1,
    d1_statements_per_request: 1,
    ..test_runtime_config()
  });
  let prepared = PreparedProvider {
    provider: test_provider("ktmb", "db-ktmb"),
    download: DownloadedFeed { size: feed.reserved_bytes(), file: feed, remote_etag: "etag-1".to_owned() },
    files: vec![
      PreparedFile { csv_file: "stops.txt".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default(), extracted_csv: None },
      PreparedFile { csv_file: "routes.txt".to_owned(), table_name: "routes".to_owned(), db_columns: &["route_id", "route_short_name"], checkpoint: Checkpoint::default(), extracted_csv: None },
    ],
    staged_files: Vec::new(),
  };

  let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(4)), runtime, 100, false, ShutdownSignal::new()).await?;
  assert!(outcome.remaining.is_none());
  assert_eq!(outcome.rows_processed, 12);
  let peak = transport.max_in_flight.load(Ordering::SeqCst);
  assert!((1..=2).contains(&peak), "expected per-database peak concurrency in 1..=2, observed {peak}");
  Ok(())
}
