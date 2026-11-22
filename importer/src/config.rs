use crate::budget::{LEDGER_WRITE_RESERVE, METADATA_WRITE_RESERVE};
use serde::Deserialize;
use std::{env, fs, path::Path, time::Duration};
use thiserror::Error;

const D1_MAX_VALUE_BYTES: u64 = 2_000_000;
const DEFAULT_D1_DATABASE_CONCURRENCY_LIMIT: usize = 4;

#[derive(Error, Debug)]
pub enum ConfigError {
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("TOML parsing error: {0}")]
  Toml(#[from] toml_edit::de::Error),
  #[error("TOML edit error: {0}")]
  TomlEdit(#[from] toml_edit::TomlError),
  #[error("missing required environment variable {name}")]
  MissingEnvironment { name: &'static str },
  #[error("invalid {name}={value:?}: {requirement}")]
  InvalidEnvironment { name: &'static str, value: String, requirement: &'static str },
  #[error("invalid providers configuration: {0}")]
  InvalidProviders(String),
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProviderConfig {
  pub name: String,
  #[serde(default)]
  pub is_active: bool,
  pub static_url: String,
  pub static_provider: String,
  pub database_id: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProvidersToml {
  pub budget_provider: String,
  pub providers: Vec<ProviderConfig>,
}

impl ProvidersToml {
  pub fn budget_database(&self) -> Result<&ProviderConfig, ConfigError> {
    self
      .providers
      .iter()
      .find(|provider| provider.name == self.budget_provider)
      .filter(|provider| !provider.database_id.is_empty())
      .ok_or_else(|| ConfigError::InvalidProviders("budget_provider must name a configured provider with a database_id".to_owned()))
  }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
  pub csv_concurrency_limit: usize,
  pub d1_global_concurrency_limit: usize,
  pub d1_database_concurrency_limit: usize,
  pub d1_max_databases: usize,
  pub query_statement_batch_size: usize,
  pub d1_statements_per_request: usize,
  pub d1_daily_write_limit: u64,
  pub max_rows_per_workflow: u64,
  pub max_feed_download_bytes: u64,
  pub max_uncompressed_feed_bytes: u64,
  pub max_feed_archive_entries: usize,
  pub max_csv_record_bytes: u64,
  pub max_statement_payload_bytes: u64,
  pub max_temp_feed_storage_bytes: u64,
  pub db_size_threshold_bytes: u64,
  pub http_connect_timeout: Duration,
  pub http_request_timeout: Duration,
  pub d1_query_timeout: Duration,
  pub d1_max_query_retries: u32,
  pub d1_max_retry_delay: Duration,
  pub workflow_max_duration: Option<Duration>,
}

impl RuntimeConfig {
  pub fn from_env() -> Result<Self, ConfigError> {
    Self::from_lookup(|name| env::var(name).ok())
  }

  fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Self, ConfigError> {
    let d1_global_concurrency_limit = bounded_usize(&mut lookup, "D1_CONCURRENCY_LIMIT", 64)?;
    let csv_concurrency_limit = d1_global_concurrency_limit.saturating_mul(2);
    let d1_database_concurrency_limit = match optional_bounded_usize(&mut lookup, "D1_DATABASE_CONCURRENCY_LIMIT", 64)? {
      Some(limit) if limit > d1_global_concurrency_limit => {
        return Err(ConfigError::InvalidEnvironment { name: "D1_DATABASE_CONCURRENCY_LIMIT", value: limit.to_string(), requirement: "must not exceed D1_CONCURRENCY_LIMIT" });
      }
      Some(limit) => limit,
      None => d1_global_concurrency_limit.min(DEFAULT_D1_DATABASE_CONCURRENCY_LIMIT),
    };
    let d1_max_databases = bounded_usize(&mut lookup, "D1_MAX_DATABASES", 50_000)?;
    let query_statement_batch_size = bounded_usize(&mut lookup, "QUERY_STATEMENT_BATCH_SIZE", 10_000)?;
    let d1_statements_per_request = bounded_usize(&mut lookup, "D1_STATEMENTS_PER_REQUEST", 16)?;
    let d1_daily_write_limit = positive_u64(&mut lookup, "D1_DAILY_WRITE_LIMIT")?;
    // Enforced below against minimum metadata + ledger reserve capacity.
    if METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE >= d1_daily_write_limit {
      return Err(ConfigError::InvalidEnvironment { name: "D1_DAILY_WRITE_LIMIT", value: d1_daily_write_limit.to_string(), requirement: "must leave capacity above metadata and ledger reserves" });
    }
    let max_rows_per_workflow = positive_u64(&mut lookup, "MAX_ROWS_PER_WORKFLOW")?;
    let max_feed_download_bytes = scaled_bytes(&mut lookup, "MAX_FEED_DOWNLOAD_MB", 1024 * 1024)?;
    let max_uncompressed_feed_bytes = scaled_bytes(&mut lookup, "MAX_UNCOMPRESSED_FEED_MB", 1024 * 1024)?;
    let max_feed_archive_entries = bounded_usize(&mut lookup, "MAX_FEED_ARCHIVE_ENTRIES", 10_000)?;
    let (max_csv_record_kb, max_csv_record_bytes) = scaled_bytes_with_raw(&mut lookup, "MAX_CSV_RECORD_KB", 1024)?;
    if max_csv_record_bytes > max_uncompressed_feed_bytes {
      return Err(ConfigError::InvalidEnvironment { name: "MAX_CSV_RECORD_KB", value: max_csv_record_kb.to_string(), requirement: "must not exceed MAX_UNCOMPRESSED_FEED_MB" });
    }
    let (max_statement_payload_kb, max_statement_payload_bytes) = scaled_bytes_with_raw(&mut lookup, "MAX_STATEMENT_PAYLOAD_KB", 1024)?;
    if max_statement_payload_bytes < max_csv_record_bytes {
      return Err(ConfigError::InvalidEnvironment { name: "MAX_STATEMENT_PAYLOAD_KB", value: max_statement_payload_kb.to_string(), requirement: "must be at least MAX_CSV_RECORD_KB" });
    }
    if max_statement_payload_bytes > D1_MAX_VALUE_BYTES {
      return Err(ConfigError::InvalidEnvironment {
        name: "MAX_STATEMENT_PAYLOAD_KB",
        value: max_statement_payload_kb.to_string(),
        requirement: "must not exceed D1's 2,000,000-byte string-value limit",
      });
    }
    let (max_temp_feed_storage_mb, max_temp_feed_storage_bytes) = scaled_bytes_with_raw(&mut lookup, "MAX_TEMP_FEED_STORAGE_MB", 1024 * 1024)?;
    if max_temp_feed_storage_bytes < max_feed_download_bytes {
      return Err(ConfigError::InvalidEnvironment { name: "MAX_TEMP_FEED_STORAGE_MB", value: max_temp_feed_storage_mb.to_string(), requirement: "must be at least MAX_FEED_DOWNLOAD_MB" });
    }
    let db_size_threshold_bytes = scaled_bytes(&mut lookup, "DB_SIZE_THRESHOLD_MB", 1024 * 1024)?;
    let http_connect_timeout = Duration::from_secs(positive_u64(&mut lookup, "HTTP_CONNECT_TIMEOUT_SECS")?);
    let http_request_timeout = Duration::from_secs(positive_u64(&mut lookup, "HTTP_REQUEST_TIMEOUT_SECS")?);
    let d1_query_timeout = Duration::from_secs(positive_u64(&mut lookup, "D1_QUERY_TIMEOUT_SECS")?);
    let d1_max_query_retries = bounded_u32_allowing_zero(&mut lookup, "D1_MAX_QUERY_RETRIES", 16)?;
    let d1_max_retry_delay = Duration::from_secs(positive_u64(&mut lookup, "D1_MAX_RETRY_DELAY_SECS")?);
    let workflow_max_duration = optional_positive_u64(&mut lookup, "WORKFLOW_MAX_DURATION_SECS")?.map(Duration::from_secs);

    Ok(Self {
      csv_concurrency_limit,
      d1_global_concurrency_limit,
      d1_database_concurrency_limit,
      d1_max_databases,
      query_statement_batch_size,
      d1_statements_per_request,
      d1_daily_write_limit,
      max_rows_per_workflow,
      max_feed_download_bytes,
      max_uncompressed_feed_bytes,
      max_feed_archive_entries,
      max_csv_record_bytes,
      max_statement_payload_bytes,
      max_temp_feed_storage_bytes,
      db_size_threshold_bytes,
      http_connect_timeout,
      http_request_timeout,
      d1_query_timeout,
      d1_max_query_retries,
      d1_max_retry_delay,
      workflow_max_duration,
    })
  }
}

fn optional_environment_value(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str) -> Option<String> {
  lookup(name).map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())
}

fn required_environment_value(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str) -> Result<String, ConfigError> {
  optional_environment_value(lookup, name).ok_or(ConfigError::MissingEnvironment { name })
}

fn parse_env<T: std::str::FromStr>(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str, requirement: &'static str) -> Result<(T, String), ConfigError> {
  let value = required_environment_value(lookup, name)?;
  let Ok(parsed) = value.parse::<T>() else {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement });
  };
  Ok((parsed, value))
}

fn optional_bounded_usize(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str, maximum: usize) -> Result<Option<usize>, ConfigError> {
  let Some(value) = optional_environment_value(lookup, name) else {
    return Ok(None);
  };
  let Ok(parsed) = value.parse::<usize>() else {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "must be a positive integer" });
  };
  if parsed == 0 || parsed > maximum {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "is outside the supported positive range" });
  }
  Ok(Some(parsed))
}

fn optional_positive_u64(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str) -> Result<Option<u64>, ConfigError> {
  let Some(value) = optional_environment_value(lookup, name) else {
    return Ok(None);
  };
  let Ok(parsed) = value.parse::<u64>() else {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "must be a positive integer" });
  };
  if parsed == 0 {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "must be greater than zero" });
  }
  Ok(Some(parsed))
}

fn bounded_usize(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str, maximum: usize) -> Result<usize, ConfigError> {
  let (parsed, value) = parse_env(lookup, name, "must be a positive integer")?;
  if parsed == 0 || parsed > maximum {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "is outside the supported positive range" });
  }
  Ok(parsed)
}

fn bounded_u32_allowing_zero(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str, maximum: u32) -> Result<u32, ConfigError> {
  let (parsed, value) = parse_env(lookup, name, "must be a non-negative integer")?;
  if parsed > maximum {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "is outside the supported non-negative range" });
  }
  Ok(parsed)
}

fn positive_u64(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str) -> Result<u64, ConfigError> {
  let (parsed, value) = parse_env(lookup, name, "must be a positive integer")?;
  if parsed == 0 {
    return Err(ConfigError::InvalidEnvironment { name, value, requirement: "must be greater than zero" });
  }
  Ok(parsed)
}

fn scaled_bytes(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str, unit_bytes: u64) -> Result<u64, ConfigError> {
  Ok(scaled_bytes_with_raw(lookup, name, unit_bytes)?.1)
}

fn scaled_bytes_with_raw(lookup: &mut impl FnMut(&str) -> Option<String>, name: &'static str, unit_bytes: u64) -> Result<(u64, u64), ConfigError> {
  let raw = positive_u64(lookup, name)?;
  let bytes = raw.checked_mul(unit_bytes).ok_or_else(|| ConfigError::InvalidEnvironment { name, value: raw.to_string(), requirement: "must fit in bytes" })?;
  Ok((raw, bytes))
}

pub fn load_config(path: impl AsRef<Path>) -> Result<ProvidersToml, ConfigError> {
  let providers_toml_str = fs::read_to_string(path)?;
  let config: ProvidersToml = toml_edit::de::from_str(&providers_toml_str)?;
  Ok(config)
}

#[cfg(test)]
#[path = "../../test/unit/importer/config.rs"]
mod tests;
