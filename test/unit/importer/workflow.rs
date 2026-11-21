use super::*;
use crate::config::ProviderConfig;
use crate::d1::{D1DatabaseInfo, D1Meta, D1Result, tests::InMemoryTransport};
use crate::error::ProcessorError;
use crate::feed::tests::{InMemoryFeedFetcher, zip_bytes};
use std::time::Duration;

fn test_runtime(max_rows_per_workflow: u64) -> Arc<RuntimeConfig> {
  Arc::new(RuntimeConfig {
    csv_concurrency_limit: 2,
    d1_global_concurrency_limit: 4,
    d1_database_concurrency_limit: 1,
    d1_max_databases: 10,
    query_statement_batch_size: 10,
    d1_statements_per_request: 2,
    d1_daily_write_limit: 100_000,
    max_rows_per_workflow,
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
  })
}

fn ok_rows(results: Vec<serde_json::Value>) -> D1Result {
  D1Result { meta: Some(D1Meta { rows_read: u64::try_from(results.len()).unwrap_or(u64::MAX), rows_written: Some(0) }), results, success: Some(true) }
}

#[tokio::test]
async fn run_import_defers_cleanly_when_daily_budget_is_unavailable() -> Result<(), Box<dyn std::error::Error>> {
  let transport = InMemoryTransport::new();
  transport.push_query_response(vec![ok_rows(Vec::new())]); // acquire_daily_budget returns 0 rows
  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);
  let providers = ProvidersToml {
    budget_provider: "ktmb".to_owned(),
    providers: vec![ProviderConfig {
      name: "ktmb".to_owned(),
      is_active: true,
      static_url: "https://example.invalid/".to_owned(),
      static_provider: "ktmb".to_owned(),
      database_id: "db-ktmb".to_owned(),
    }],
  };
  let fetcher = InMemoryFeedFetcher::new();
  let summary = run_import(providers, test_runtime(100), client, fetcher.clone(), "unused.toml").await?;
  assert!(summary.deferred);
  assert!(!summary.has_error);
  assert!(fetcher.recorded_requests().is_empty());
  assert_eq!(transport.recorded_queries().len(), 1);
  Ok(())
}

#[tokio::test]
async fn run_import_executes_active_provider_skips_inactive_and_releases_daily_budget() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n")])?;
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport.push_query_response(vec![ok_rows(vec![serde_json::json!({ "Granted": 5_000 })])]); // acquire_daily_budget
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_dataset_version
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_files_progress
  // Remaining queries (prepare_staging, insert into staging, save_file_progress, commit_provider_swap, release_daily_budget) use default 1-write responses.

  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);
  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-1", Some(u64::try_from(archive.len())?), vec![archive]);
  let providers = ProvidersToml {
    budget_provider: "ktmb".to_owned(),
    providers: vec![
      ProviderConfig { name: "ktmb".to_owned(), is_active: true, static_url: "https://example.invalid/".to_owned(), static_provider: "ktmb".to_owned(), database_id: "db-ktmb".to_owned() },
      ProviderConfig {
        name: "mybas-johor".to_owned(),
        is_active: false,
        static_url: "https://example.invalid/".to_owned(),
        static_provider: "mybas-johor".to_owned(),
        database_id: "db-johor".to_owned(),
      },
    ],
  };

  let summary = run_import(providers, test_runtime(100), client, fetcher, "unused.toml").await?;
  assert!(!summary.deferred);
  assert!(!summary.has_error);
  assert_eq!(summary.total_rows_processed, 2);
  assert_eq!(summary.remaining_providers, 0);
  assert_eq!(summary.waves_executed, 1);
  let recorded = transport.recorded_queries();
  assert!(recorded.iter().any(|query| query.sql.starts_with("CREATE TABLE \"stops__staging\"")));
  assert!(recorded.iter().any(|query| query.sql.starts_with("INSERT INTO \"stops__staging\"")));
  assert!(recorded.iter().any(|query| query.sql == "DROP TABLE IF EXISTS \"stops\""));
  assert!(recorded.iter().any(|query| query.sql == "ALTER TABLE \"stops__staging\" RENAME TO \"stops\""));
  assert!(recorded.iter().any(|query| query.sql.contains("UPDATE \"0_daily_import_budget\"\nSET Reserved = Reserved - ?")));
  Ok(())
}

#[tokio::test]
async fn run_import_withholds_daily_lease_release_when_provider_preparation_fails_while_healthy_providers_finish_waves() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\n")])?;
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport.push_query_response(vec![ok_rows(vec![serde_json::json!({ "Granted": 5_000 })])]);
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_dataset_version (ktmb)
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_dataset_version (rapid-rail-kl)
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_files_progress for healthy provider
  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);

  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_response(Err(ProcessorError::Invariant("injected fetch failure".to_owned())));
  fetcher.push_stream("etag-1", Some(u64::try_from(archive.len())?), vec![archive]);
  let providers = ProvidersToml {
    budget_provider: "ktmb".to_owned(),
    providers: vec![
      ProviderConfig { name: "ktmb".to_owned(), is_active: true, static_url: "https://example.invalid/".to_owned(), static_provider: "ktmb".to_owned(), database_id: "db-ktmb".to_owned() },
      ProviderConfig {
        name: "rapid-rail-kl".to_owned(),
        is_active: true,
        static_url: "https://example.invalid/".to_owned(),
        static_provider: "rapid-rail-kl".to_owned(),
        database_id: "db-rail".to_owned(),
      },
    ],
  };

  let summary = run_import(providers, test_runtime(100), client, fetcher, "unused.toml").await?;
  assert!(!summary.deferred);
  assert!(summary.has_error);
  assert_eq!(summary.total_rows_processed, 3);
  assert_eq!(summary.remaining_providers, 0);
  assert_eq!(summary.waves_executed, 1);
  assert!(!transport.recorded_queries().iter().any(|query| query.sql.contains("SET Reserved = Reserved - ?")));
  Ok(())
}
