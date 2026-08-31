use crate::config::RuntimeConfig;
use reqwest::{
  Client, StatusCode,
  header::{AUTHORIZATION, HeaderValue, RETRY_AFTER},
};
use serde::{Deserialize, Serialize};
use std::{
  sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
  },
  time::Duration,
};
use thiserror::Error;
use tokio::sync::{Notify, Semaphore};

const MAX_QUERY_RETRIES: u32 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Error, Debug)]
pub enum D1Error {
  #[error("Reqwest error: {0}")]
  Reqwest(#[from] reqwest::Error),
  #[error("D1 API error: {0}")]
  ApiError(String),
  #[error("D1 query failed: {0:?}")]
  QueryFailed(Option<Vec<serde_json::Value>>),
  #[error("D1 workflow write budget exhausted: requested {requested} reserved writes with {remaining} remaining")]
  WriteBudgetExhausted { requested: u64, remaining: u64 },
}

#[derive(Serialize)]
pub struct D1Query<'a> {
  pub sql: &'a str,
  pub params: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct D1BatchRequest<'slice, 'query> {
  batch: &'slice [D1Query<'query>],
}

#[derive(Deserialize, Debug)]
struct D1Response {
  success: bool,
  result: Option<Vec<D1Result>>,
  errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct D1Result {
  #[serde(default)]
  pub results: Vec<serde_json::Value>,
  #[serde(default)]
  pub meta: Option<D1Meta>,
  pub success: Option<bool>,
}

impl D1Result {
  pub fn rows_written(&self) -> Option<u64> {
    self.meta.as_ref().map(|meta| meta.rows_written)
  }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct D1Meta {
  #[serde(default)]
  rows_read: u64,
  #[serde(default)]
  rows_written: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct D1Usage {
  pub rows_read: u64,
  pub rows_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct D1DatabaseInfo {
  pub name: String,
  pub file_size: u64,
}

#[derive(Debug)]
struct D1WriteBudgetState {
  remaining: u64,
  in_flight: u64,
}

#[derive(Debug)]
struct D1WriteBudget {
  state: Mutex<D1WriteBudgetState>,
  notify: Notify,
}

impl D1WriteBudget {
  fn new(maximum_writes: u64) -> Self {
    Self {
      state: Mutex::new(D1WriteBudgetState {
        remaining: maximum_writes,
        in_flight: 0,
      }),
      notify: Notify::new(),
    }
  }

  fn state(&self) -> MutexGuard<'_, D1WriteBudgetState> {
    match self.state.lock() {
      Ok(state) => state,
      Err(poisoned) => poisoned.into_inner(),
    }
  }

  async fn reserve(self: &Arc<Self>, requested: u64) -> Result<D1WriteReservation, D1Error> {
    loop {
      let notified = self.notify.notified();
      tokio::pin!(notified);
      let _ = notified.as_mut().enable();
      {
        let mut state = self.state();
        if requested <= state.remaining {
          state.remaining -= requested;
          state.in_flight = state.in_flight.saturating_add(1);
          return Ok(D1WriteReservation {
            budget: self.clone(),
            reserved: requested,
            settled: false,
          });
        }
        if state.in_flight == 0 {
          return Err(D1Error::WriteBudgetExhausted {
            requested,
            remaining: state.remaining,
          });
        }
      }
      notified.await;
    }
  }

  fn settle(&self, reserved: u64, actual: u64) -> bool {
    let mut state = self.state();
    state.in_flight = state.in_flight.saturating_sub(1);
    let exceeded_reservation = actual > reserved;
    if exceeded_reservation {
      state.remaining = state.remaining.saturating_sub(actual - reserved);
    } else {
      state.remaining = state.remaining.saturating_add(reserved - actual);
    }
    drop(state);
    self.notify.notify_waiters();
    exceeded_reservation
  }
}

pub struct D1WriteReservation {
  budget: Arc<D1WriteBudget>,
  reserved: u64,
  settled: bool,
}

impl D1WriteReservation {
  pub fn finish(mut self, actual: u64) -> Result<(), D1Error> {
    let exceeded_reservation = self.budget.settle(self.reserved, actual);
    self.settled = true;
    if exceeded_reservation {
      return Err(D1Error::ApiError(format!(
        "D1 reported {actual} row writes for a {}-write reservation; the schema write-amplification invariant was exceeded",
        self.reserved
      )));
    }
    Ok(())
  }

  pub fn consume_all(self) {
    let reserved = self.reserved;
    let _ = self.finish(reserved);
  }
}

impl Drop for D1WriteReservation {
  fn drop(&mut self) {
    if !self.settled {
      // An unexpected drop can follow task cancellation or a panic after
      // the request was dispatched. Treat that outcome as ambiguous and
      // consume the reservation instead of making it spendable again.
      self.budget.settle(self.reserved, self.reserved);
      self.settled = true;
    }
  }
}

#[derive(Clone)]
pub struct D1Client {
  pub client: Client,
  account_id: Arc<str>,
  authorization: HeaderValue,
  concurrency_limit: Arc<Semaphore>,
  write_budget: Arc<D1WriteBudget>,
  query_timeout: Duration,
  rows_read: Arc<AtomicU64>,
  rows_written: Arc<AtomicU64>,
}

impl D1Client {
  pub fn new(account_id: String, api_token: String, runtime: &RuntimeConfig) -> Result<Self, D1Error> {
    let client = Client::builder()
      .connect_timeout(runtime.http_connect_timeout)
      .timeout(runtime.http_request_timeout)
      .user_agent(concat!("my-GTFS-worker/", env!("CARGO_PKG_VERSION")))
      .build()?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {api_token}")).map_err(|error| D1Error::ApiError(format!("Invalid API token header: {error}")))?;
    authorization.set_sensitive(true);

    Ok(Self {
      client,
      account_id: Arc::from(account_id),
      authorization,
      concurrency_limit: Arc::new(Semaphore::new(runtime.d1_global_concurrency_limit)),
      write_budget: Arc::new(D1WriteBudget::new(runtime.max_d1_rows_written_per_workflow)),
      query_timeout: runtime.d1_query_timeout,
      rows_read: Arc::new(AtomicU64::new(0)),
      rows_written: Arc::new(AtomicU64::new(0)),
    })
  }

  pub fn usage(&self) -> D1Usage {
    D1Usage {
      rows_read: self.rows_read.load(Ordering::Relaxed),
      rows_written: self.rows_written.load(Ordering::Relaxed),
    }
  }

  pub async fn reserve_import_writes(&self, maximum_writes: u64) -> Result<D1WriteReservation, D1Error> {
    self.write_budget.reserve(maximum_writes).await
  }

  pub async fn query(&self, db_id: &str, query: D1Query<'_>) -> Result<Vec<D1Result>, D1Error> {
    let (results, _) = self.execute_query_body(db_id, &query, MAX_QUERY_RETRIES).await?;
    validate_result_count(results, 1)
  }

  pub async fn batch_with_retry_state(&self, db_id: &str, queries: &[D1Query<'_>]) -> Result<(Vec<D1Result>, bool), D1Error> {
    if queries.is_empty() {
      return Ok((Vec::new(), false));
    }
    let (results, had_ambiguous_retry) = self.execute_query_body(db_id, &D1BatchRequest { batch: queries }, MAX_QUERY_RETRIES).await?;
    Ok((validate_result_count(results, queries.len())?, had_ambiguous_retry))
  }

  async fn execute_query_body<T>(&self, db_id: &str, body: &T, max_retries: u32) -> Result<(Vec<D1Result>, bool), D1Error>
  where
    T: Serialize + Sync + ?Sized,
  {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database/{db_id}/query", self.account_id);
    let mut retries = 0;
    let mut backoff = Duration::from_secs(1);
    let mut had_ambiguous_retry = false;

    loop {
      let permit = self
        .concurrency_limit
        .acquire()
        .await
        .map_err(|error| D1Error::ApiError(format!("Failed to acquire D1 request permit: {error}")))?;
      let response = self
        .client
        .post(&url)
        .header(AUTHORIZATION, self.authorization.clone())
        .timeout(self.query_timeout)
        .json(body)
        .send()
        .await;

      match response {
        Ok(response) if response.status().is_success() => {
          let decoded = response.json::<D1Response>().await;
          drop(permit);
          match decoded {
            Ok(decoded) => {
              let results = validate_query_response(decoded)?;
              self.record_usage(&results);
              return Ok((results, had_ambiguous_retry));
            }
            Err(error) if retries < max_retries => {
              had_ambiguous_retry = true;
              println!("D1 response decode error: {error}; retrying {}/{} in {:?}", retries + 1, max_retries, backoff);
            }
            Err(error) => return Err(D1Error::Reqwest(error)),
          }
        }
        Ok(response) => {
          let status = response.status();
          let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
          let error_text = response.text().await.unwrap_or_default();
          drop(permit);

          if !is_retryable_status(status) || retries >= max_retries {
            return Err(D1Error::ApiError(format!("HTTP {status}: {error_text}")));
          }

          let delay = retry_after.unwrap_or(backoff).min(MAX_RETRY_DELAY);
          had_ambiguous_retry = true;
          println!("D1 API error ({status}): {error_text}; retrying {}/{} in {:?}", retries + 1, max_retries, delay);
          retries += 1;
          tokio::time::sleep(delay).await;
          backoff = (backoff * 2).min(Duration::from_secs(8));
          continue;
        }
        Err(error) => {
          drop(permit);
          if error.is_builder() || retries >= max_retries {
            return Err(D1Error::Reqwest(error));
          }
          had_ambiguous_retry = true;
          println!("D1 request error: {error}; retrying {}/{} in {:?}", retries + 1, max_retries, backoff);
        }
      }

      retries += 1;
      tokio::time::sleep(backoff).await;
      backoff = (backoff * 2).min(Duration::from_secs(8));
    }
  }

  fn record_usage(&self, results: &[D1Result]) {
    let rows_read = results.iter().filter_map(|result| result.meta.as_ref()).map(|meta| meta.rows_read).sum();
    let rows_written = results.iter().filter_map(|result| result.meta.as_ref()).map(|meta| meta.rows_written).sum();
    self.rows_read.fetch_add(rows_read, Ordering::Relaxed);
    self.rows_written.fetch_add(rows_written, Ordering::Relaxed);
  }

  pub async fn get_dataset_version(&self, db_id: &str, provider_name: &str) -> Result<Option<String>, D1Error> {
    let res = self
      .query(
        db_id,
        D1Query {
          sql: "SELECT ETag FROM dataset_versions WHERE Provider = ?",
          params: vec![serde_json::Value::String(provider_name.to_owned())],
        },
      )
      .await?;
    Ok(
      res
        .first()
        .and_then(|result| result.results.first())
        .and_then(|row| row.get("ETag"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned),
    )
  }

  pub async fn set_dataset_version(&self, db_id: &str, provider: &str, etag: &str) -> Result<(), D1Error> {
    self.query(
            db_id,
            D1Query {
                sql: "INSERT INTO dataset_versions (Provider, ETag, UpdatedAt) VALUES (?, ?, CURRENT_TIMESTAMP) ON CONFLICT(Provider) DO UPDATE SET ETag = excluded.ETag, UpdatedAt = CURRENT_TIMESTAMP WHERE dataset_versions.ETag IS NOT excluded.ETag",
                params: vec![serde_json::Value::String(provider.to_owned()), serde_json::Value::String(etag.to_owned())],
            },
        )
        .await?;
    Ok(())
  }

  pub async fn get_file_progress_summary(&self, db_id: &str, provider_name: &str) -> Result<(u64, u64), D1Error> {
    let res = self
      .query(
        db_id,
        D1Query {
          sql: "SELECT COUNT(*) AS total, COALESCE(SUM(CASE WHEN Status != 0 THEN 1 ELSE 0 END), 0) AS incomplete FROM import_progress WHERE Provider = ?",
          params: vec![serde_json::Value::String(provider_name.to_owned())],
        },
      )
      .await?;
    let row = res.first().and_then(|result| result.results.first());
    let total = row.and_then(|value| value.get("total")).and_then(serde_json::Value::as_u64).unwrap_or(0);
    let incomplete = row.and_then(|value| value.get("incomplete")).and_then(serde_json::Value::as_u64).unwrap_or(0);
    Ok((total, incomplete))
  }

  pub async fn get_all_files_progress(&self, db_id: &str, provider_name: &str) -> Result<Vec<serde_json::Value>, D1Error> {
    let res = self
      .query(
        db_id,
        D1Query {
          sql: "SELECT FileName, CRC, LastProcessedLine, LastProcessedByte, Status FROM import_progress WHERE Provider = ?",
          params: vec![serde_json::Value::String(provider_name.to_owned())],
        },
      )
      .await?;
    Ok(res.first().map(|result| result.results.clone()).unwrap_or_default())
  }

  #[allow(clippy::too_many_arguments)]
  pub async fn init_file_progress(&self, db_id: &str, provider: &str, file: &str, crc: &str, line: u64, byte: u64, status: i64) -> Result<(), D1Error> {
    self.query(
            db_id,
            D1Query {
                sql: "INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt) VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ON CONFLICT(Provider, FileName) DO UPDATE SET CRC = excluded.CRC, LastProcessedLine = excluded.LastProcessedLine, LastProcessedByte = excluded.LastProcessedByte, Status = excluded.Status, UpdatedAt = CURRENT_TIMESTAMP WHERE import_progress.CRC IS NOT excluded.CRC OR import_progress.LastProcessedLine IS NOT excluded.LastProcessedLine OR import_progress.LastProcessedByte IS NOT excluded.LastProcessedByte OR import_progress.Status IS NOT excluded.Status",
                params: vec![
                    serde_json::Value::String(provider.to_owned()),
                    serde_json::Value::String(file.to_owned()),
                    serde_json::Value::String(crc.to_owned()),
                    serde_json::Value::Number(line.into()),
                    serde_json::Value::Number(byte.into()),
                    serde_json::Value::Number(status.into()),
                ],
            },
        )
        .await?;
    Ok(())
  }

  pub async fn update_file_progress(&self, db_id: &str, provider: &str, file: &str, line: u64, byte: u64, status: i64) -> Result<(), D1Error> {
    self.query(
            db_id,
            D1Query {
                sql: "UPDATE import_progress SET LastProcessedLine = ?, LastProcessedByte = ?, Status = ?, UpdatedAt = CURRENT_TIMESTAMP WHERE Provider = ? AND FileName = ? AND (LastProcessedLine IS NOT ? OR LastProcessedByte IS NOT ? OR Status IS NOT ?)",
                params: vec![
                    serde_json::Value::Number(line.into()),
                    serde_json::Value::Number(byte.into()),
                    serde_json::Value::Number(status.into()),
                    serde_json::Value::String(provider.to_owned()),
                    serde_json::Value::String(file.to_owned()),
                    serde_json::Value::Number(line.into()),
                    serde_json::Value::Number(byte.into()),
                    serde_json::Value::Number(status.into()),
                ],
            },
        )
        .await?;
    Ok(())
  }

  pub async fn get_database_info(&self, db_id: &str) -> Result<D1DatabaseInfo, D1Error> {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database/{db_id}", self.account_id);
    let response = self.client.get(&url).header(AUTHORIZATION, self.authorization.clone()).send().await?;
    if !response.status().is_success() {
      return Err(D1Error::ApiError(format!("Failed to get database {db_id}: {}", response.text().await.unwrap_or_default())));
    }

    let json: serde_json::Value = response.json().await?;
    if !json.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false) {
      return Err(D1Error::ApiError(format!("API success=false: {json:?}")));
    }

    let result = json.get("result").ok_or_else(|| D1Error::ApiError(format!("Database {db_id} response did not contain a result")))?;
    let name = result
      .get("name")
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned)
      .ok_or_else(|| D1Error::ApiError(format!("Database {db_id} response did not contain a name")))?;
    let file_size = result
      .get("file_size")
      .and_then(serde_json::Value::as_u64)
      .ok_or_else(|| D1Error::ApiError(format!("Database {db_id} response did not contain a numeric file_size")))?;
    Ok(D1DatabaseInfo { name, file_size })
  }

  pub async fn get_database_count(&self) -> Result<u64, D1Error> {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database", self.account_id);
    let response = self.client.get(&url).header(AUTHORIZATION, self.authorization.clone()).query(&[("per_page", "10")]).send().await?;
    if !response.status().is_success() {
      return Err(D1Error::ApiError(format!("Failed to list D1 databases: {}", response.text().await.unwrap_or_default())));
    }

    let json: serde_json::Value = response.json().await?;
    if !json.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false) {
      return Err(D1Error::ApiError(format!("API success=false while listing D1 databases: {json:?}")));
    }

    json
      .get("result_info")
      .and_then(|result_info| result_info.get("total_count"))
      .and_then(serde_json::Value::as_u64)
      .ok_or_else(|| D1Error::ApiError("D1 database list response did not contain result_info.total_count".into()))
  }

  pub async fn create_database(&self, name: &str) -> Result<String, D1Error> {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database", self.account_id);
    let response = self
      .client
      .post(&url)
      .header(AUTHORIZATION, self.authorization.clone())
      .json(&serde_json::json!({ "name": name }))
      .send()
      .await?;
    if !response.status().is_success() {
      return Err(D1Error::ApiError(format!("Failed to create database {name}: {}", response.text().await.unwrap_or_default())));
    }

    let json: serde_json::Value = response.json().await?;
    if !json.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false) {
      return Err(D1Error::ApiError(format!("API success=false: {json:?}")));
    }

    json
      .get("result")
      .and_then(|result| result.get("uuid"))
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned)
      .ok_or_else(|| D1Error::ApiError("No uuid in create database response".into()))
  }

  pub async fn execute_schema(&self, db_id: &str, schema_sql: &str) -> Result<(), D1Error> {
    for statement in schema_sql.split(';').map(str::trim).filter(|statement| !statement.is_empty()) {
      let (results, _) = self.execute_query_body(db_id, &D1Query { sql: statement, params: Vec::new() }, 0).await?;
      validate_result_count(results, 1)?;
    }
    Ok(())
  }

  pub async fn delete_database(&self, db_id: &str) -> Result<(), D1Error> {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database/{db_id}", self.account_id);
    let response = self.client.delete(&url).header(AUTHORIZATION, self.authorization.clone()).send().await?;
    if !response.status().is_success() {
      return Err(D1Error::ApiError(format!("Failed to delete database {db_id}: {}", response.text().await.unwrap_or_default())));
    }

    let json: serde_json::Value = response.json().await?;
    if !json.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false) {
      return Err(D1Error::ApiError(format!("API success=false: {json:?}")));
    }
    Ok(())
  }
}

fn validate_query_response(response: D1Response) -> Result<Vec<D1Result>, D1Error> {
  if !response.success {
    return Err(D1Error::QueryFailed(response.errors));
  }

  let results = response.result.unwrap_or_default();
  if results.iter().any(|result| result.success == Some(false)) {
    return Err(D1Error::QueryFailed(response.errors));
  }
  Ok(results)
}

fn validate_result_count(results: Vec<D1Result>, expected: usize) -> Result<Vec<D1Result>, D1Error> {
  if results.len() != expected {
    return Err(D1Error::ApiError(format!("D1 returned {} result(s) for {expected} submitted statement(s)", results.len())));
  }
  Ok(results)
}

fn is_retryable_status(status: StatusCode) -> bool {
  status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::REQUEST_TIMEOUT
}

fn parse_retry_after(value: Option<&HeaderValue>) -> Option<Duration> {
  value.and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok()).map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn retry_after_accepts_delta_seconds() {
    assert_eq!(parse_retry_after(Some(&HeaderValue::from_static("17"))), Some(Duration::from_secs(17)));
    assert_eq!(parse_retry_after(Some(&HeaderValue::from_static("not-a-number"))), None);
  }

  #[test]
  fn failed_statement_rejects_successful_envelope() {
    let response = D1Response {
      success: true,
      result: Some(vec![D1Result {
        results: Vec::new(),
        meta: None,
        success: Some(false),
      }]),
      errors: None,
    };
    assert!(validate_query_response(response).is_err());
  }

  #[test]
  fn result_count_must_match_submitted_statements() {
    assert!(validate_result_count(Vec::new(), 1).is_err());
    assert!(
      validate_result_count(
        vec![D1Result {
          results: Vec::new(),
          meta: None,
          success: Some(true),
        }],
        1
      )
      .is_ok()
    );
  }

  #[test]
  fn usage_metadata_is_aggregated() {
    let runtime = RuntimeConfig {
      csv_concurrency_limit: 1,
      d1_global_concurrency_limit: 1,
      d1_database_concurrency_limit: 1,
      d1_max_databases: 10,
      query_statement_batch_size: 1,
      d1_statements_per_request: 1,
      max_d1_rows_written_per_workflow: 2,
      max_rows_per_workflow: 1,
      max_feed_download_bytes: 1,
      max_uncompressed_feed_bytes: 1,
      max_csv_record_bytes: 1,
      max_statement_payload_bytes: 1,
      max_temp_feed_storage_bytes: 1,
      db_size_threshold_bytes: 1,
      http_connect_timeout: Duration::from_secs(1),
      http_request_timeout: Duration::from_secs(1),
      d1_query_timeout: Duration::from_secs(1),
    };
    let client = D1Client::new("account".into(), "token".into(), &runtime);
    assert!(client.is_ok());
    let Some(client) = client.ok() else {
      return;
    };
    client.record_usage(&[
      D1Result {
        results: Vec::new(),
        meta: Some(D1Meta { rows_read: 7, rows_written: 3 }),
        success: Some(true),
      },
      D1Result {
        results: Vec::new(),
        meta: Some(D1Meta { rows_read: 11, rows_written: 5 }),
        success: Some(true),
      },
    ]);
    assert_eq!(client.usage(), D1Usage { rows_read: 18, rows_written: 8 });
  }

  #[tokio::test]
  async fn write_budget_recycles_noop_capacity_and_hard_stops() -> Result<(), D1Error> {
    let budget = Arc::new(D1WriteBudget::new(10));
    let reservation = budget.reserve(8).await?;
    reservation.finish(3)?;
    assert_eq!(budget.state().remaining, 7);

    let reservation = budget.reserve(7).await?;
    reservation.consume_all();
    assert!(matches!(budget.reserve(1).await, Err(D1Error::WriteBudgetExhausted { requested: 1, remaining: 0 })));
    Ok(())
  }

  #[tokio::test]
  async fn write_budget_wakes_waiters_after_capacity_is_released() -> Result<(), D1Error> {
    let budget = Arc::new(D1WriteBudget::new(10));
    let first = budget.reserve(8).await?;
    let waiting_budget = budget.clone();
    let waiter = tokio::spawn(async move { waiting_budget.reserve(4).await });
    tokio::task::yield_now().await;
    first.finish(2)?;

    let joined = match tokio::time::timeout(Duration::from_secs(1), waiter).await {
      Ok(joined) => joined.map_err(|error| D1Error::ApiError(format!("write-budget waiter failed to join: {error}")))?,
      Err(_) => return Err(D1Error::ApiError("write-budget waiter was not notified".to_owned())),
    };
    let reservation = joined?;
    reservation.finish(0)?;
    assert_eq!(budget.state().remaining, 8);
    Ok(())
  }

  #[tokio::test]
  async fn dropped_write_reservation_fails_closed() -> Result<(), D1Error> {
    let budget = Arc::new(D1WriteBudget::new(10));
    let reservation = budget.reserve(8).await?;
    drop(reservation);

    assert_eq!(budget.state().remaining, 2);
    assert!(matches!(budget.reserve(3).await, Err(D1Error::WriteBudgetExhausted { requested: 3, remaining: 2 })));
    Ok(())
  }
}
