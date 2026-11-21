use super::*;

#[test]
fn runtime_config_requires_every_server_env_var_without_defaults() -> Result<(), Box<dyn std::error::Error>> {
  let entries = [
    ("D1_CONCURRENCY_LIMIT", "10"),
    ("D1_MAX_DATABASES", "10"),
    ("QUERY_STATEMENT_BATCH_SIZE", "2000"),
    ("D1_STATEMENTS_PER_REQUEST", "8"),
    ("D1_DAILY_WRITE_LIMIT", "100000"),
    ("MAX_ROWS_PER_WORKFLOW", "5000000"),
    ("MAX_FEED_DOWNLOAD_MB", "256"),
    ("MAX_UNCOMPRESSED_FEED_MB", "512"),
    ("MAX_FEED_ARCHIVE_ENTRIES", "256"),
    ("MAX_CSV_RECORD_KB", "512"),
    ("MAX_STATEMENT_PAYLOAD_KB", "1536"),
    ("MAX_TEMP_FEED_STORAGE_MB", "2048"),
    ("DB_SIZE_THRESHOLD_MB", "490"),
    ("HTTP_CONNECT_TIMEOUT_SECS", "10"),
    ("HTTP_REQUEST_TIMEOUT_SECS", "120"),
    ("D1_QUERY_TIMEOUT_SECS", "35"),
    ("D1_MAX_QUERY_RETRIES", "3"),
    ("D1_MAX_RETRY_DELAY_SECS", "30"),
  ];
  let map: std::collections::HashMap<&str, &str> = entries.into_iter().collect();
  let config = RuntimeConfig::from_lookup(|name| map.get(name).map(|value| (*value).to_owned()))?;
  assert_eq!(config.csv_concurrency_limit, 20);
  assert_eq!(config.d1_global_concurrency_limit, 10);
  assert_eq!(config.d1_database_concurrency_limit, 10);
  assert_eq!(config.d1_max_databases, 10);
  assert_eq!(config.query_statement_batch_size, 2000);
  assert_eq!(config.d1_statements_per_request, 8);
  assert_eq!(config.d1_daily_write_limit, 100_000);
  assert_eq!(config.max_rows_per_workflow, 5_000_000);
  assert_eq!(config.max_feed_download_bytes, 256 * 1024 * 1024);
  assert_eq!(config.max_uncompressed_feed_bytes, 512 * 1024 * 1024);
  assert_eq!(config.max_feed_archive_entries, 256);
  assert_eq!(config.max_csv_record_bytes, 512 * 1024);
  assert_eq!(config.max_statement_payload_bytes, 1_536 * 1024);
  assert_eq!(config.max_temp_feed_storage_bytes, 2_048 * 1024 * 1024);
  assert_eq!(config.db_size_threshold_bytes, 490 * 1024 * 1024);
  assert_eq!(config.http_connect_timeout, Duration::from_secs(10));
  assert_eq!(config.http_request_timeout, Duration::from_secs(120));
  assert_eq!(config.d1_query_timeout, Duration::from_secs(35));
  assert_eq!(config.d1_max_query_retries, 3);
  assert_eq!(config.d1_max_retry_delay, Duration::from_secs(30));

  for (missing_key, _) in entries {
    let result = RuntimeConfig::from_lookup(|name| (name != missing_key).then(|| map[name].to_owned()));
    assert!(matches!(result, Err(ConfigError::MissingEnvironment { name }) if name == missing_key));
  }

  let zero_retries = RuntimeConfig::from_lookup(|name| Some(if name == "D1_MAX_QUERY_RETRIES" { "0" } else { map[name] }.to_owned()))?;
  assert_eq!(zero_retries.d1_max_query_retries, 0);

  let minimum_reserves = (METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE).to_string();
  let daily_below_reserves = RuntimeConfig::from_lookup(|name| Some(if name == "D1_DAILY_WRITE_LIMIT" { minimum_reserves.as_str() } else { map[name] }.to_owned()));
  assert!(matches!(daily_below_reserves, Err(ConfigError::InvalidEnvironment { name: "D1_DAILY_WRITE_LIMIT", .. })));
  Ok(())
}
