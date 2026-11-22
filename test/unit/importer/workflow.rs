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
    workflow_max_duration: None,
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
async fn run_import_releases_unused_daily_lease_when_provider_preparation_fails_while_healthy_providers_finish_waves() -> Result<(), Box<dyn std::error::Error>> {
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
  assert!(!summary.shutdown_requested);
  assert_eq!(summary.total_rows_processed, 3);
  assert_eq!(summary.remaining_providers, 0);
  assert_eq!(summary.waves_executed, 1);
  assert!(transport.recorded_queries().iter().any(|query| query.sql.contains("SET Reserved = Reserved - ?")));
  Ok(())
}

#[tokio::test]
async fn run_import_releases_only_unspent_budget_after_ambiguous_write_batch_error() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n")])?;
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport.push_query_response(vec![ok_rows(vec![serde_json::json!({ "Granted": 5_000 })])]); // acquire_daily_budget
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_dataset_version
  transport.push_query_response(vec![ok_rows(Vec::new())]); // get_files_progress
  transport.push_query_response(vec![
    D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read: 0, rows_written: Some(1) }), success: Some(true) },
    D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read: 0, rows_written: Some(1) }), success: Some(true) },
    D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read: 0, rows_written: Some(1) }), success: Some(true) },
  ]); // prepare_staging (3 statements, 3 writes)
  transport.push_retryable_error("HTTP 503", Some(Duration::ZERO)); // ambiguous batch write (2 rows * 1 write for WITHOUT ROWID stops -> 2 writes deducted on drop)
  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);

  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-1", Some(u64::try_from(archive.len())?), vec![archive]);
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

  let summary = run_import(providers, test_runtime(100), client, fetcher, "unused.toml").await?;
  assert!(summary.has_error);
  let release = transport.recorded_queries().into_iter().find(|query| query.sql.contains("SET Reserved = Reserved - ?")).ok_or("expected release_daily_budget query")?;
  // Granted 5_000 = 2 ledger + 2_000 metadata + 2_998 data.
  // Metadata spent: 3 (prepare_staging); unchanged InProgress checkpoint skips save_file_progress -> 1_997 remaining.
  // Data spent conservatively on ambiguous 503: 2 (2 rows * 1 write for WITHOUT ROWID) -> 2_996 remaining.
  // Released unused writes = 1_997 + 2_996 = 4_993.
  assert_eq!(release.params[0], serde_json::json!(4_993));
  Ok(())
}

#[tokio::test]
async fn run_import_with_shutdown_exits_cleanly_before_leasing_or_after_trigger() -> Result<(), Box<dyn std::error::Error>> {
  let transport = InMemoryTransport::new();
  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);
  let fetcher = InMemoryFeedFetcher::new();
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
  let shutdown = ShutdownSignal::new();
  shutdown.trigger();

  let summary = run_import_with_shutdown(providers, test_runtime(100), client, fetcher, "unused.toml", shutdown).await?;
  assert!(summary.shutdown_requested);
  assert!(!summary.has_error);
  assert!(!summary.deferred);
  assert!(transport.recorded_queries().is_empty());
  Ok(())
}

#[tokio::test]
async fn run_import_redistributes_multi_provider_waves_with_cached_extracted_csv() -> Result<(), Box<dyn std::error::Error>> {
  let ktmb_archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\n")])?;
  let rail_archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nR1,One\nR2,Two\nR3,Three\nR4,Four\nR5,Five\n")])?;
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-test".to_owned(), file_size: 10 }, 2);
  transport.push_query_response(vec![ok_rows(vec![serde_json::json!({ "Granted": 5_000 })])]); // acquire_daily_budget
  // Remaining queries use InMemoryTransport's default per-statement empty/1-write responses, which are order-independent across concurrent providers.
  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);

  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-ktmb", Some(u64::try_from(ktmb_archive.len())?), vec![ktmb_archive]);
  fetcher.push_stream("etag-rail", Some(u64::try_from(rail_archive.len())?), vec![rail_archive]);
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

  // Budget of 6 rows across 2 providers -> Wave 1 gives [3, 3]; small finishes at 1 row, big pauses at 3 rows
  // with extracted CSV cached -> Wave 2 redistributes remaining 2 rows to finish all 6 rows cleanly.
  let summary = run_import(providers, test_runtime(6), client, fetcher, "unused.toml").await?;
  assert!(!summary.has_error);
  assert!(!summary.shutdown_requested);
  assert_eq!(summary.waves_executed, 2);
  assert_eq!(summary.total_rows_processed, 6);
  assert_eq!(summary.remaining_providers, 0);
  Ok(())
}

#[tokio::test]
async fn run_import_soft_duration_limit_drains_and_releases_daily_budget() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n")])?;
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 10 }, 1);
  transport.push_query_response(vec![ok_rows(vec![serde_json::json!({ "Granted": 5_000 })])]); // acquire_daily_budget
  let client = D1Client::with_in_memory(transport.clone(), 5_000, 1_000);

  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-1", Some(u64::try_from(archive.len())?), vec![archive]);
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

  let mut runtime = (*test_runtime(100)).clone();
  runtime.workflow_max_duration = Some(Duration::from_millis(1));
  let shutdown = ShutdownSignal::new();
  // Wait for the deadline timer inside run_import_with_shutdown by letting a tiny delay occur?
  // Instead, test a 1ms soft duration with a slow fetcher so the deadline fires after acquire_daily_budget.
  struct DelayedFeedFetcher {
    inner: Arc<InMemoryFeedFetcher>,
    delay: Duration,
  }
  impl FeedFetcher for DelayedFeedFetcher {
    fn fetch<'a>(&'a self, url: &'a str, if_none_match: Option<&'a str>) -> crate::d1::BoxFuture<'a, Result<crate::feed::FeedResponse, ProcessorError>> {
      Box::pin(async move {
        tokio::time::sleep(self.delay).await;
        self.inner.fetch(url, if_none_match).await
      })
    }
  }

  let delayed_fetcher = Arc::new(DelayedFeedFetcher { inner: fetcher, delay: Duration::from_millis(30) });
  let summary = run_import_with_shutdown(providers, Arc::new(runtime), client, delayed_fetcher, "unused.toml", shutdown).await?;
  assert!(summary.shutdown_requested);
  assert!(!summary.has_error);
  assert_eq!(summary.waves_executed, 0);
  assert!(transport.recorded_queries().iter().any(|query| query.sql.contains("SET Reserved = Reserved - ?")));
  Ok(())
}

#[tokio::test]
async fn run_import_splits_tight_daily_write_budget_fairly_across_multiple_providers() -> Result<(), Box<dyn std::error::Error>> {
  struct RowCountingTransport(Arc<InMemoryTransport>);

  impl crate::d1::D1Transport for RowCountingTransport {
    fn query<'a>(&'a self, db_id: &'a str, query: &'a crate::d1::D1Query<'a>) -> crate::d1::BoxFuture<'a, Result<Vec<D1Result>, crate::d1::D1AttemptError>> {
      self.0.query(db_id, query)
    }

    fn query_batch<'a>(&'a self, db_id: &'a str, queries: &'a [crate::d1::D1Query<'a>]) -> crate::d1::BoxFuture<'a, Result<Vec<D1Result>, crate::d1::D1AttemptError>> {
      let row_counts = queries
        .iter()
        .map(|query| {
          if query.sql.starts_with("INSERT INTO ") {
            query
              .params
              .first()
              .and_then(serde_json::Value::as_str)
              .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(json).ok())
              .and_then(|rows| u64::try_from(rows.len()).ok())
              .unwrap_or(1)
          } else {
            1
          }
        })
        .collect::<Vec<_>>();
      Box::pin(async move {
        let mut results = self.0.query_batch(db_id, queries).await?;
        for (result, count) in results.iter_mut().zip(row_counts) {
          if let Some(meta) = result.meta.as_mut() {
            meta.rows_written = Some(count);
          }
        }
        Ok(results)
      })
    }

    fn get_database_info<'a>(&'a self, db_id: &'a str) -> crate::d1::BoxFuture<'a, Result<D1DatabaseInfo, crate::d1::D1Error>> {
      self.0.get_database_info(db_id)
    }

    fn get_database_count<'a>(&'a self) -> crate::d1::BoxFuture<'a, Result<u64, crate::d1::D1Error>> {
      self.0.get_database_count()
    }

    fn create_database<'a>(&'a self, name: &'a str) -> crate::d1::BoxFuture<'a, Result<String, crate::d1::D1Error>> {
      self.0.create_database(name)
    }

    fn delete_database<'a>(&'a self, db_id: &'a str) -> crate::d1::BoxFuture<'a, Result<(), crate::d1::D1Error>> {
      self.0.delete_database(db_id)
    }
  }

  let ktmb_archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nK1,One\nK2,Two\nK3,Three\nK4,Four\nK5,Five\nK6,Six\n")])?;
  let rail_archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nR1,One\nR2,Two\nR3,Three\nR4,Four\nR5,Five\nR6,Six\n")])?;
  let transport = InMemoryTransport::new();
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-test".to_owned(), file_size: 10 }, 2);
  // Granted 2_008 writes = 2 ledger + 2_000 metadata + 6 data writes across 2 providers -> 3 rows each (not 6 + 0).
  transport.push_query_response(vec![ok_rows(vec![serde_json::json!({ "Granted": 2_008 })])]);
  let client = D1Client::with_transport(Arc::new(RowCountingTransport(transport.clone())), 4, 5_000, 1_000, 3, Duration::from_secs(30), Arc::new(crate::budget::SystemClock));

  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-ktmb", Some(u64::try_from(ktmb_archive.len())?), vec![ktmb_archive]);
  fetcher.push_stream("etag-rail", Some(u64::try_from(rail_archive.len())?), vec![rail_archive]);
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

  let mut runtime = (*test_runtime(100_000)).clone();
  runtime.query_statement_batch_size = 10;
  let summary = run_import(providers, Arc::new(runtime), client, fetcher, "unused.toml").await?;
  assert!(!summary.has_error);
  assert!(summary.write_budget_exhausted);
  assert_eq!(summary.remaining_providers, 2);
  assert_eq!(summary.total_rows_processed, 6);
  let insert_payloads = transport
    .recorded_queries()
    .into_iter()
    .filter(|query| query.sql.starts_with("INSERT INTO \"stops__staging\""))
    .filter_map(|query| query.params.first().and_then(serde_json::Value::as_str).map(str::to_owned))
    .collect::<Vec<_>>();
  assert_eq!(insert_payloads.len(), 2, "both providers must receive an equal write slice");
  assert!(insert_payloads.iter().any(|payload| payload.contains("\"K1\"") && payload.contains("\"K3\"") && !payload.contains("\"K4\"")));
  assert!(insert_payloads.iter().any(|payload| payload.contains("\"R1\"") && payload.contains("\"R3\"") && !payload.contains("\"R4\"")));
  Ok(())
}
