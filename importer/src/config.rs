use serde::Deserialize;
use std::{env, fs, time::Duration};
use thiserror::Error;

const D1_MAX_VALUE_BYTES: u64 = 2_000_000;

#[derive(Error, Debug)]
pub enum ConfigError {
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("TOML parsing error: {0}")]
  Toml(#[from] toml::de::Error),
  #[error("invalid {name}={value:?}: {requirement}")]
  InvalidEnvironment { name: &'static str, value: String, requirement: &'static str },
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProviderConfig {
  pub name: String,
  #[serde(default = "default")]
  pub is_active: bool,
  pub static_url: String,
  pub static_provider: String,
  pub database_id: String,
}

fn default() -> bool {
  false
}

#[derive(Deserialize, Debug)]
pub struct ProvidersToml {
  pub providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
  pub csv_concurrency_limit: usize,
  pub d1_global_concurrency_limit: usize,
  pub d1_database_concurrency_limit: usize,
  pub d1_max_databases: usize,
  pub query_statement_batch_size: usize,
  pub d1_statements_per_request: usize,
  pub max_d1_rows_written_per_workflow: u64,
  pub max_rows_per_workflow: u64,
  pub max_feed_download_bytes: u64,
  pub max_uncompressed_feed_bytes: u64,
  pub max_csv_record_bytes: u64,
  pub max_statement_payload_bytes: u64,
  pub max_temp_feed_storage_bytes: u64,
  pub db_size_threshold_bytes: u64,
  pub http_connect_timeout: Duration,
  pub http_request_timeout: Duration,
  pub d1_query_timeout: Duration,
}

impl RuntimeConfig {
  pub fn from_env() -> Result<Self, ConfigError> {
    let detected_parallelism = std::thread::available_parallelism().map(|parallelism| parallelism.get()).unwrap_or(2).clamp(1, 8);
    let threshold_mb = positive_u64("DB_SIZE_THRESHOLD_MB", 400)?;
    let db_size_threshold_bytes = threshold_mb.checked_mul(1024 * 1024).ok_or_else(|| ConfigError::InvalidEnvironment {
      name: "DB_SIZE_THRESHOLD_MB",
      value: threshold_mb.to_string(),
      requirement: "must fit in bytes",
    })?;
    let max_feed_download_mb = positive_u64("MAX_FEED_DOWNLOAD_MB", 256)?;
    let max_feed_download_bytes = max_feed_download_mb.checked_mul(1024 * 1024).ok_or_else(|| ConfigError::InvalidEnvironment {
      name: "MAX_FEED_DOWNLOAD_MB",
      value: max_feed_download_mb.to_string(),
      requirement: "must fit in bytes",
    })?;
    let max_uncompressed_feed_mb = positive_u64("MAX_UNCOMPRESSED_FEED_MB", 512)?;
    let max_uncompressed_feed_bytes = max_uncompressed_feed_mb.checked_mul(1024 * 1024).ok_or_else(|| ConfigError::InvalidEnvironment {
      name: "MAX_UNCOMPRESSED_FEED_MB",
      value: max_uncompressed_feed_mb.to_string(),
      requirement: "must fit in bytes",
    })?;
    let max_csv_record_kb = positive_u64("MAX_CSV_RECORD_KB", 512)?;
    let max_csv_record_bytes = max_csv_record_kb.checked_mul(1024).ok_or_else(|| ConfigError::InvalidEnvironment {
      name: "MAX_CSV_RECORD_KB",
      value: max_csv_record_kb.to_string(),
      requirement: "must fit in bytes",
    })?;
    if max_csv_record_bytes > max_uncompressed_feed_bytes {
      return Err(ConfigError::InvalidEnvironment {
        name: "MAX_CSV_RECORD_KB",
        value: max_csv_record_kb.to_string(),
        requirement: "must not exceed MAX_UNCOMPRESSED_FEED_MB",
      });
    }
    let max_statement_payload_kb = positive_u64("MAX_STATEMENT_PAYLOAD_KB", 1_536)?;
    let max_statement_payload_bytes = max_statement_payload_kb.checked_mul(1024).ok_or_else(|| ConfigError::InvalidEnvironment {
      name: "MAX_STATEMENT_PAYLOAD_KB",
      value: max_statement_payload_kb.to_string(),
      requirement: "must fit in bytes",
    })?;
    if max_statement_payload_bytes < max_csv_record_bytes {
      return Err(ConfigError::InvalidEnvironment {
        name: "MAX_STATEMENT_PAYLOAD_KB",
        value: max_statement_payload_kb.to_string(),
        requirement: "must be at least MAX_CSV_RECORD_KB",
      });
    }
    if max_statement_payload_bytes > D1_MAX_VALUE_BYTES {
      return Err(ConfigError::InvalidEnvironment {
        name: "MAX_STATEMENT_PAYLOAD_KB",
        value: max_statement_payload_kb.to_string(),
        requirement: "must not exceed D1's 2,000,000-byte string-value limit",
      });
    }
    let max_temp_feed_storage_mb = positive_u64("MAX_TEMP_FEED_STORAGE_MB", 2_048)?;
    let max_temp_feed_storage_bytes = max_temp_feed_storage_mb.checked_mul(1024 * 1024).ok_or_else(|| ConfigError::InvalidEnvironment {
      name: "MAX_TEMP_FEED_STORAGE_MB",
      value: max_temp_feed_storage_mb.to_string(),
      requirement: "must fit in bytes",
    })?;
    if max_temp_feed_storage_bytes < max_feed_download_bytes {
      return Err(ConfigError::InvalidEnvironment {
        name: "MAX_TEMP_FEED_STORAGE_MB",
        value: max_temp_feed_storage_mb.to_string(),
        requirement: "must be at least MAX_FEED_DOWNLOAD_MB",
      });
    }

    let query_statement_batch_size = bounded_usize("QUERY_STATEMENT_BATCH_SIZE", 1_000, 10_000)?;
    let d1_statements_per_request = bounded_usize("D1_STATEMENTS_PER_REQUEST", 4, 16)?;
    let max_d1_rows_written_per_workflow = positive_u64("MAX_D1_ROWS_WRITTEN_PER_WORKFLOW", 40_000)?;
    // if max_d1_rows_written_per_workflow > 100_000 {
    //   return Err(ConfigError::InvalidEnvironment {
    //     name: "MAX_D1_ROWS_WRITTEN_PER_WORKFLOW",
    //     value: max_d1_rows_written_per_workflow.to_string(),
    //     requirement: "must not exceed the D1 Free daily write allowance of 100,000 rows",
    //   });
    // }
    let maximum_group_rows = u64::try_from(query_statement_batch_size)
      .unwrap_or(u64::MAX)
      .saturating_mul(u64::try_from(d1_statements_per_request).unwrap_or(u64::MAX));
    if maximum_group_rows.saturating_mul(2) > max_d1_rows_written_per_workflow {
      return Err(ConfigError::InvalidEnvironment {
        name: "MAX_D1_ROWS_WRITTEN_PER_WORKFLOW",
        value: max_d1_rows_written_per_workflow.to_string(),
        requirement: "must reserve at least two indexed D1 writes for every row in one maximum statement group",
      });
    }

    Ok(Self {
      csv_concurrency_limit: bounded_usize("CSV_CONCURRENCY_LIMIT", detected_parallelism, 64)?,
      d1_global_concurrency_limit: bounded_usize("D1_CONCURRENCY_LIMIT", 6, 64)?,
      d1_database_concurrency_limit: bounded_usize("D1_DATABASE_CONCURRENCY_LIMIT", 1, 16)?,
      d1_max_databases: bounded_usize("D1_MAX_DATABASES", 10, 50_000)?,
      query_statement_batch_size,
      d1_statements_per_request,
      max_d1_rows_written_per_workflow,
      max_rows_per_workflow: positive_u64_alias("MAX_ROWS_PER_WORKFLOW", "MAX_ROWS_PER_RUN", 1_750_000)?,
      max_feed_download_bytes,
      max_uncompressed_feed_bytes,
      max_csv_record_bytes,
      max_statement_payload_bytes,
      max_temp_feed_storage_bytes,
      db_size_threshold_bytes,
      http_connect_timeout: Duration::from_secs(10),
      http_request_timeout: Duration::from_secs(120),
      d1_query_timeout: Duration::from_secs(35),
    })
  }
}

fn environment_value(name: &'static str) -> Option<String> {
  env::var(name).ok().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())
}

fn bounded_usize(name: &'static str, default: usize, maximum: usize) -> Result<usize, ConfigError> {
  let Some(value) = environment_value(name) else {
    return Ok(default);
  };
  let parsed = value.parse::<usize>().map_err(|_| ConfigError::InvalidEnvironment {
    name,
    value: value.clone(),
    requirement: "must be a positive integer",
  })?;
  if parsed == 0 || parsed > maximum {
    return Err(ConfigError::InvalidEnvironment {
      name,
      value,
      requirement: "is outside the supported positive range",
    });
  }
  Ok(parsed)
}

fn positive_u64(name: &'static str, default: u64) -> Result<u64, ConfigError> {
  let Some(value) = environment_value(name) else {
    return Ok(default);
  };
  let parsed = value.parse::<u64>().map_err(|_| ConfigError::InvalidEnvironment {
    name,
    value: value.clone(),
    requirement: "must be a positive integer",
  })?;
  if parsed == 0 {
    return Err(ConfigError::InvalidEnvironment {
      name,
      value,
      requirement: "must be greater than zero",
    });
  }
  Ok(parsed)
}

fn positive_u64_alias(primary: &'static str, legacy: &'static str, default: u64) -> Result<u64, ConfigError> {
  if environment_value(primary).is_some() {
    return positive_u64(primary, default);
  }
  if environment_value(legacy).is_some() {
    println!("Configuration {legacy} is deprecated; use {primary} instead.");
    return positive_u64(legacy, default);
  }
  Ok(default)
}

pub fn load_config(path: &str) -> Result<ProvidersToml, ConfigError> {
  let providers_toml_str = fs::read_to_string(path)?;
  let config: ProvidersToml = toml::from_str(&providers_toml_str)?;
  Ok(config)
}
