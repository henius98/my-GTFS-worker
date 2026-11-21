use crate::budget::{Clock, LEDGER_WRITE_RESERVE, METADATA_WRITE_RESERVE, SystemClock, WorkflowBudget, WriteReservation};
use crate::config::RuntimeConfig;
use crate::schema::SchemaCatalog;
use reqwest::{
  Client, Method, StatusCode,
  header::{AUTHORIZATION, HeaderValue, RETRY_AFTER},
};
use serde::{Deserialize, Serialize};
use std::{
  future::Future,
  pin::Pin,
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  time::Duration,
};
use thiserror::Error;
use tokio::sync::{Semaphore, SemaphorePermit};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

const CHECKPOINT_WRITE_RESERVE: u64 = 6;
const DEFAULT_D1_API_BASE_URL: &str = "https://api.cloudflare.com";
const METADATA_STATEMENT_WRITE_RESERVE: u64 = 4;
const SWAP_RENAME_WRITE_RESERVE: u64 = 32;
const ACQUIRE_DAILY_BUDGET_SQL: &str = include_str!("sql/acquire_daily_budget.sql");
const RELEASE_DAILY_BUDGET_SQL: &str = include_str!("sql/release_daily_budget.sql");
const GET_DATASET_VERSION_SQL: &str = include_str!("sql/get_dataset_version.sql");
const SET_DATASET_VERSION_SQL: &str = include_str!("sql/set_dataset_version.sql");
const GET_FILES_PROGRESS_SQL: &str = include_str!("sql/get_files_progress.sql");
const INIT_FILE_PROGRESS_SQL: &str = include_str!("sql/init_file_progress.sql");
const SAVE_FILE_PROGRESS_SQL: &str = include_str!("sql/save_file_progress.sql");
const DELETE_FILE_PROGRESS_SQL: &str = include_str!("sql/delete_file_progress.sql");
const COMPLETE_FILE_PROGRESS_SQL: &str = include_str!("sql/complete_file_progress.sql");

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

#[derive(Debug)]
pub struct D1AttemptError {
  pub error: D1Error,
  pub retryable: bool,
  pub retry_after: Option<Duration>,
}

impl D1AttemptError {
  pub fn retryable(error: D1Error, retry_after: Option<Duration>) -> Self {
    Self { error, retryable: true, retry_after }
  }
}

impl From<D1Error> for D1AttemptError {
  fn from(error: D1Error) -> Self {
    Self { error, retryable: false, retry_after: None }
  }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Checkpoint {
  pub line: u64,
  pub byte: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum FileImportStatus {
  Completed = 0,
  InProgress = 1,
  Staged = 2,
}

impl FileImportStatus {
  pub fn from_i64(value: i64) -> Option<Self> {
    match value {
      0 => Some(Self::Completed),
      1 => Some(Self::InProgress),
      2 => Some(Self::Staged),
      _ => None,
    }
  }

  pub fn as_i64(self) -> i64 {
    self as i64
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredFileProgress {
  pub file_name: String,
  pub crc: String,
  pub checkpoint: Checkpoint,
  pub status: FileImportStatus,
}

pub struct StagingTableSetup<'a> {
  pub file: &'a str,
  pub crc: &'a str,
  pub table_name: &'a str,
  pub create_sql: &'a str,
}

pub struct OrphanStagingCleanup<'a> {
  pub file: &'a str,
  pub table_name: &'a str,
}

pub struct StagedTableSwap<'a> {
  pub file: &'a str,
  pub table_name: &'a str,
}

pub struct FileProgressUpdate<'a> {
  pub file: &'a str,
  pub checkpoint: Checkpoint,
  pub status: FileImportStatus,
}

#[derive(Serialize, Clone, Debug)]
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
  fn rows_written(&self) -> Option<u64> {
    self.meta.as_ref().and_then(|meta| meta.rows_written)
  }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct D1Meta {
  #[serde(default)]
  pub rows_read: u64,
  #[serde(default)]
  pub rows_written: Option<u64>,
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

pub trait D1Transport: Send + Sync {
  fn query<'a>(&'a self, db_id: &'a str, query: &'a D1Query<'a>) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>>;
  fn query_batch<'a>(&'a self, db_id: &'a str, queries: &'a [D1Query<'a>]) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>>;
  fn get_database_info<'a>(&'a self, db_id: &'a str) -> BoxFuture<'a, Result<D1DatabaseInfo, D1Error>>;
  fn get_database_count<'a>(&'a self) -> BoxFuture<'a, Result<u64, D1Error>>;
  fn create_database<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, D1Error>>;
  fn delete_database<'a>(&'a self, db_id: &'a str) -> BoxFuture<'a, Result<(), D1Error>>;
}

#[derive(Clone)]
pub(crate) struct HttpD1Transport {
  client: Client,
  api_base_url: Arc<str>,
  account_id: Arc<str>,
  authorization: HeaderValue,
  query_timeout: Duration,
}

impl HttpD1Transport {
  pub(crate) fn with_base_url(account_id: String, api_token: String, api_base_url: String, runtime: &RuntimeConfig) -> Result<Self, D1Error> {
    let trimmed_base_url = api_base_url.trim().trim_end_matches('/');
    if trimmed_base_url.is_empty() {
      return Err(D1Error::ApiError("D1_API_BASE_URL must not be empty".to_owned()));
    }
    let client = Client::builder().connect_timeout(runtime.http_connect_timeout).timeout(runtime.http_request_timeout).user_agent(concat!("my-GTFS-worker/", env!("CARGO_PKG_VERSION"))).build()?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {api_token}")).map_err(|error| D1Error::ApiError(format!("Invalid API token header: {error}")))?;
    authorization.set_sensitive(true);
    Ok(Self { client, api_base_url: Arc::from(trimmed_base_url), account_id: Arc::from(account_id), authorization, query_timeout: runtime.d1_query_timeout })
  }

  fn d1_url(&self, suffix: &str) -> String {
    format!("{}/client/v4/accounts/{}/d1/database{suffix}", self.api_base_url, self.account_id)
  }

  async fn execute_http_query_attempt<T>(&self, db_id: &str, body: &T) -> Result<Vec<D1Result>, D1AttemptError>
  where
    T: Serialize + Sync + ?Sized,
  {
    let url = self.d1_url(&format!("/{db_id}/query"));
    let response = self.client.post(&url).header(AUTHORIZATION, self.authorization.clone()).timeout(self.query_timeout).json(body).send().await;

    match response {
      Ok(response) if response.status().is_success() => match response.json::<D1Response>().await {
        Ok(decoded) => validate_query_response(decoded).map_err(D1AttemptError::from),
        Err(error) => Err(D1AttemptError::retryable(D1Error::Reqwest(error), None)),
      },
      Ok(response) => {
        let status = response.status();
        let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
        let error_text = response.text().await.unwrap_or_default();
        let error = D1Error::ApiError(format!("HTTP {status}: {error_text}"));
        if is_retryable_status(status) { Err(D1AttemptError::retryable(error, retry_after)) } else { Err(D1AttemptError::from(error)) }
      }
      Err(error) => {
        let retryable = !error.is_builder();
        Err(D1AttemptError { error: D1Error::Reqwest(error), retryable, retry_after: None })
      }
    }
  }

  async fn control_plane_request(&self, method: Method, suffix: &str, body: Option<&serde_json::Value>, action: &str) -> Result<serde_json::Value, D1Error> {
    let url = self.d1_url(suffix);
    let mut request = self.client.request(method, &url).header(AUTHORIZATION, self.authorization.clone()).timeout(self.query_timeout);
    if let Some(payload) = body {
      request = request.json(payload);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
      return Err(D1Error::ApiError(format!("Failed to {action}: {}", response.text().await.unwrap_or_default())));
    }
    let json: serde_json::Value = response.json().await?;
    if !json.get("success").and_then(serde_json::Value::as_bool).unwrap_or(false) {
      return Err(D1Error::ApiError(format!("API success=false while trying to {action}: {json:?}")));
    }
    Ok(json)
  }
}

impl D1Transport for HttpD1Transport {
  fn query<'a>(&'a self, db_id: &'a str, query: &'a D1Query<'a>) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
    Box::pin(async move { self.execute_http_query_attempt(db_id, query).await })
  }

  fn query_batch<'a>(&'a self, db_id: &'a str, queries: &'a [D1Query<'a>]) -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>> {
    Box::pin(async move { self.execute_http_query_attempt(db_id, &D1BatchRequest { batch: queries }).await })
  }

  fn get_database_info<'a>(&'a self, db_id: &'a str) -> BoxFuture<'a, Result<D1DatabaseInfo, D1Error>> {
    Box::pin(async move {
      let json = self.control_plane_request(Method::GET, &format!("/{db_id}"), None, &format!("get database {db_id}")).await?;
      let result = json.get("result").ok_or_else(|| D1Error::ApiError(format!("Database {db_id} response did not contain a result")))?;
      let name = result.get("name").and_then(serde_json::Value::as_str).map(str::to_owned).ok_or_else(|| D1Error::ApiError(format!("Database {db_id} response did not contain a name")))?;
      let file_size = result.get("file_size").and_then(serde_json::Value::as_u64).ok_or_else(|| D1Error::ApiError(format!("Database {db_id} response did not contain a numeric file_size")))?;
      Ok(D1DatabaseInfo { name, file_size })
    })
  }

  fn get_database_count<'a>(&'a self) -> BoxFuture<'a, Result<u64, D1Error>> {
    Box::pin(async move {
      let json = self.control_plane_request(Method::GET, "?per_page=10", None, "list D1 databases").await?;
      json
        .get("result_info")
        .and_then(|result_info| result_info.get("total_count"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| D1Error::ApiError("D1 database list response did not contain result_info.total_count".into()))
    })
  }

  fn create_database<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, D1Error>> {
    Box::pin(async move {
      let payload = serde_json::json!({ "name": name });
      let json = self.control_plane_request(Method::POST, "", Some(&payload), &format!("create database {name}")).await?;
      json.get("result").and_then(|result| result.get("uuid")).and_then(serde_json::Value::as_str).map(str::to_owned).ok_or_else(|| D1Error::ApiError("No uuid in create database response".into()))
    })
  }

  fn delete_database<'a>(&'a self, db_id: &'a str) -> BoxFuture<'a, Result<(), D1Error>> {
    Box::pin(async move {
      self.control_plane_request(Method::DELETE, &format!("/{db_id}"), None, &format!("delete database {db_id}")).await?;
      Ok(())
    })
  }
}

#[derive(Clone)]
pub struct D1Client {
  transport: Arc<dyn D1Transport>,
  concurrency_limit: Arc<Semaphore>,
  budget: WorkflowBudget,
  rows_read: Arc<AtomicU64>,
  rows_written: Arc<AtomicU64>,
  max_query_retries: u32,
  max_retry_delay: Duration,
}

impl D1Client {
  pub fn new(account_id: String, api_token: String, runtime: &RuntimeConfig) -> Result<Self, D1Error> {
    Self::with_base_url(account_id, api_token, DEFAULT_D1_API_BASE_URL.to_owned(), runtime)
  }

  pub fn with_base_url(account_id: String, api_token: String, api_base_url: String, runtime: &RuntimeConfig) -> Result<Self, D1Error> {
    let transport = Arc::new(HttpD1Transport::with_base_url(account_id, api_token, api_base_url, runtime)?);
    Ok(Self::with_transport(
      transport,
      runtime.d1_global_concurrency_limit,
      runtime.d1_daily_write_limit,
      METADATA_WRITE_RESERVE,
      runtime.d1_max_query_retries,
      runtime.d1_max_retry_delay,
      Arc::new(SystemClock),
    ))
  }

  pub fn with_transport(
    transport: Arc<dyn D1Transport>,
    global_concurrency: usize,
    max_data_writes: u64,
    max_metadata_writes: u64,
    max_query_retries: u32,
    max_retry_delay: Duration,
    clock: Arc<dyn Clock>,
  ) -> Self {
    Self {
      transport,
      concurrency_limit: Arc::new(Semaphore::new(global_concurrency.max(1))),
      budget: WorkflowBudget::new(max_data_writes, max_metadata_writes, clock),
      rows_read: Arc::new(AtomicU64::new(0)),
      rows_written: Arc::new(AtomicU64::new(0)),
      max_query_retries,
      max_retry_delay,
    }
  }

  async fn acquire_permit(&self) -> Result<SemaphorePermit<'_>, D1Error> {
    self.concurrency_limit.acquire().await.map_err(|error| D1Error::ApiError(format!("Failed to acquire D1 request permit: {error}")))
  }

  pub async fn acquire_daily_budget(&mut self, database_id: &str, workflow_limit: u64, daily_write_limit: u64) -> Result<bool, D1Error> {
    let day = self.budget.today_utc();
    let requested = workflow_limit;
    if requested <= METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE {
      return Err(D1Error::ApiError("Workflow budget is too small for metadata and ledger writes".into()));
    }
    if requested > daily_write_limit {
      return Err(D1Error::ApiError("Workflow budget exceeds daily write limit".into()));
    }
    let query = D1Query { sql: ACQUIRE_DAILY_BUDGET_SQL, params: vec![requested.into(), daily_write_limit.into(), day.clone().into(), (METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE).into()] };
    let results = self.execute_single_query(database_id, &query, 0).await?;
    let Some(row) = results.first().and_then(|result| result.results.first()) else {
      println!("Daily D1 budget cannot cover another workflow's metadata and ledger writes; deferring until the next UTC day.");
      return Ok(false);
    };
    let granted = row
      .get("Granted")
      .and_then(serde_json::Value::as_u64)
      .filter(|granted| *granted > METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE && *granted <= requested)
      .ok_or_else(|| D1Error::ApiError("Daily D1 budget returned an invalid workflow allowance".into()))?;
    println!("Daily D1 budget granted {granted} writes for this workflow (configured maximum: {requested}).");
    self.budget.install_daily_lease(database_id.to_owned(), day, granted);
    Ok(true)
  }

  pub async fn release_daily_budget(&self) -> Result<(), D1Error> {
    let Some(lease) = self.budget.daily_lease() else {
      return Ok(());
    };
    let unused = self.budget.unused_writes();
    let query = D1Query { sql: RELEASE_DAILY_BUDGET_SQL, params: vec![unused.into(), lease.day.clone().into(), unused.into()] };
    // Never retry this subtraction: a lost response leaves a conservative lease.
    self.execute_single_query(&lease.database_id, &query, 0).await?;
    Ok(())
  }

  pub fn is_budget_database(&self, database_id: &str) -> bool {
    self.budget.is_budget_database(database_id)
  }

  pub fn usage(&self) -> D1Usage {
    D1Usage { rows_read: self.rows_read.load(Ordering::Relaxed), rows_written: self.rows_written.load(Ordering::Relaxed) }
  }

  async fn query_rows(&self, db_id: &str, query: D1Query<'_>) -> Result<Vec<D1Result>, D1Error> {
    let results = self.execute_single_query(db_id, &query, self.max_query_retries).await?;
    validate_result_count(results, 1)
  }

  pub async fn reserve_checkpoint(&self) -> Result<WriteReservation, D1Error> {
    self.budget.reserve_metadata(CHECKPOINT_WRITE_RESERVE).await
  }

  pub(crate) async fn reserve_import_write(&self, estimated_writes: u64) -> Result<WriteReservation, D1Error> {
    self.budget.begin_import_write(estimated_writes).await
  }

  pub(crate) async fn execute_reserved_import_batch(&self, write_reservation: WriteReservation, db_id: &str, queries: &[D1Query<'_>]) -> Result<D1Usage, D1Error> {
    if queries.is_empty() {
      let _ = write_reservation.settle(0);
      return Ok(D1Usage::default());
    }
    // A terminal transport failure may be ambiguous. Dropping `write_reservation`
    // unsettled consumes the estimated write upper bound so later work cannot spend it again.
    let results = self.execute_batch_queries(db_id, queries, 0).await?;
    let results = validate_result_count(results, queries.len())?;
    let rows_written = sum_rows_written(&results).ok_or_else(|| D1Error::ApiError("D1 response omitted or overflowed rows_written metadata; consumed the estimated write upper bound".to_owned()))?;
    let rows_read: u64 = results.iter().filter_map(|result| result.meta.as_ref()).map(|meta| meta.rows_read).sum();
    let _ = write_reservation.settle(rows_written);
    Ok(D1Usage { rows_read, rows_written })
  }

  pub async fn execute_import_batch(&self, db_id: &str, queries: &[D1Query<'_>], estimated_writes: u64) -> Result<D1Usage, D1Error> {
    if queries.is_empty() {
      return Ok(D1Usage::default());
    }
    let write_reservation = self.reserve_import_write(estimated_writes).await?;
    self.execute_reserved_import_batch(write_reservation, db_id, queries).await
  }

  async fn metadata_batch(&self, db_id: &str, queries: &[D1Query<'_>], maximum_writes: u64) -> Result<Vec<D1Result>, D1Error> {
    let reservation = self.budget.reserve_metadata(maximum_writes).await?;
    self.reserved_metadata_batch(db_id, queries, reservation).await
  }

  async fn reserved_metadata_batch(&self, db_id: &str, queries: &[D1Query<'_>], reservation: WriteReservation) -> Result<Vec<D1Result>, D1Error> {
    let results = self.execute_batch_queries(db_id, queries, 0).await?;
    let results = validate_result_count(results, queries.len())?;
    let actual = sum_rows_written(&results).ok_or_else(|| D1Error::ApiError("Missing metadata write accounting".into()))?;
    let claimed = reservation.claimed();
    if reservation.settle(actual) {
      println!("Warning: D1 reported {actual} metadata row writes for a {claimed}-write reservation on {db_id}; settled actual usage");
    }
    Ok(results)
  }

  async fn execute_single_query(&self, db_id: &str, query: &D1Query<'_>, max_retries: u32) -> Result<Vec<D1Result>, D1Error> {
    self.execute_with_retries(max_retries, || self.transport.query(db_id, query)).await
  }

  async fn execute_batch_queries(&self, db_id: &str, queries: &[D1Query<'_>], max_retries: u32) -> Result<Vec<D1Result>, D1Error> {
    self.execute_with_retries(max_retries, || self.transport.query_batch(db_id, queries)).await
  }

  async fn execute_with_retries<'a, F>(&self, max_retries: u32, mut send: F) -> Result<Vec<D1Result>, D1Error>
  where
    F: FnMut() -> BoxFuture<'a, Result<Vec<D1Result>, D1AttemptError>>,
  {
    let mut retries = 0;
    let mut backoff = Duration::from_secs(1);

    loop {
      let permit = self.acquire_permit().await?;
      self.budget.check_daily_window()?;
      let attempt = send().await;
      drop(permit);

      match attempt {
        Ok(results) => {
          self.record_usage(&results);
          return Ok(results);
        }
        Err(attempt_error) => {
          if !attempt_error.retryable || retries >= max_retries {
            return Err(attempt_error.error);
          }
          let delay = attempt_error.retry_after.unwrap_or(backoff).min(self.max_retry_delay);
          println!("D1 error ({}); retrying {}/{} in {:?}", attempt_error.error, retries + 1, max_retries, delay);
          retries += 1;
          tokio::time::sleep(delay).await;
          backoff = (backoff * 2).min(self.max_retry_delay);
        }
      }
    }
  }

  fn record_usage(&self, results: &[D1Result]) {
    let rows_read = results.iter().filter_map(|result| result.meta.as_ref()).map(|meta| meta.rows_read).sum();
    let rows_written = results.iter().filter_map(D1Result::rows_written).sum();
    self.rows_read.fetch_add(rows_read, Ordering::Relaxed);
    self.rows_written.fetch_add(rows_written, Ordering::Relaxed);
  }

  pub async fn get_dataset_version(&self, db_id: &str) -> Result<Option<String>, D1Error> {
    let res = self.query_rows(db_id, D1Query { sql: GET_DATASET_VERSION_SQL, params: Vec::new() }).await?;
    Ok(res.first().and_then(|result| result.results.first()).and_then(|row| row.get("ETag")).and_then(serde_json::Value::as_str).map(str::to_owned))
  }

  pub async fn set_dataset_version(&self, db_id: &str, etag: &str) -> Result<(), D1Error> {
    self.metadata_batch(db_id, &[D1Query { sql: SET_DATASET_VERSION_SQL, params: vec![etag.into()] }], 2).await?;
    Ok(())
  }

  pub async fn get_files_progress(&self, db_id: &str) -> Result<Vec<StoredFileProgress>, D1Error> {
    let res = self.query_rows(db_id, D1Query { sql: GET_FILES_PROGRESS_SQL, params: Vec::new() }).await?;
    let rows = res.into_iter().next().map(|result| result.results).unwrap_or_default();
    let mut parsed = Vec::with_capacity(rows.len());
    for row in rows {
      let Some(file_name) = row.get("FileName").and_then(serde_json::Value::as_str).map(str::to_owned) else {
        continue;
      };
      let crc = row.get("CRC").and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
      let line = row.get("LastProcessedLine").and_then(serde_json::Value::as_u64).unwrap_or(0);
      let byte = row.get("LastProcessedByte").and_then(serde_json::Value::as_u64).unwrap_or(0);
      let raw_status = row.get("Status").and_then(serde_json::Value::as_i64).ok_or_else(|| D1Error::ApiError(format!("0_import_progress row for {file_name} omitted integer Status")))?;
      let status = FileImportStatus::from_i64(raw_status).ok_or_else(|| D1Error::ApiError(format!("0_import_progress row for {file_name} had invalid Status {raw_status}")))?;
      parsed.push(StoredFileProgress { file_name, crc, checkpoint: Checkpoint { line, byte }, status });
    }
    Ok(parsed)
  }

  pub async fn prepare_staging(&self, db_id: &str, orphans: &[OrphanStagingCleanup<'_>], setups: &[StagingTableSetup<'_>]) -> Result<(), D1Error> {
    if orphans.is_empty() && setups.is_empty() {
      return Ok(());
    }
    let mut owned_sql = Vec::with_capacity(orphans.len() + setups.len());
    for orphan in orphans {
      owned_sql.push(SchemaCatalog::drop_table_if_exists_sql(&SchemaCatalog::staging_table_name(orphan.table_name)));
    }
    for setup in setups {
      owned_sql.push(SchemaCatalog::drop_table_if_exists_sql(&SchemaCatalog::staging_table_name(setup.table_name)));
    }

    let mut queries = Vec::with_capacity(orphans.len() * 2 + setups.len() * 3);
    for (index, orphan) in orphans.iter().enumerate() {
      queries.push(D1Query { sql: &owned_sql[index], params: Vec::new() });
      queries.push(D1Query { sql: DELETE_FILE_PROGRESS_SQL, params: vec![orphan.file.into()] });
    }
    for (index, setup) in setups.iter().enumerate() {
      let drop_sql = &owned_sql[orphans.len() + index];
      queries.push(D1Query { sql: drop_sql, params: Vec::new() });
      queries.push(D1Query { sql: setup.create_sql, params: Vec::new() });
      queries.push(D1Query { sql: INIT_FILE_PROGRESS_SQL, params: vec![setup.file.into(), setup.crc.into(), 0_u64.into(), 0_u64.into()] });
    }
    self.metadata_batch(db_id, &queries, queries.len() as u64 * METADATA_STATEMENT_WRITE_RESERVE).await?;
    Ok(())
  }

  pub async fn save_file_progress(&self, reservation: WriteReservation, db_id: &str, update: FileProgressUpdate<'_>) -> Result<(), D1Error> {
    let query = save_file_progress_query(update.file, update.checkpoint, update.status);
    self.reserved_metadata_batch(db_id, &[query], reservation).await?;
    Ok(())
  }

  pub async fn skip_empty_staged_file(&self, reservation: WriteReservation, db_id: &str, file: &str, table_name: &str, checkpoint: Checkpoint) -> Result<(), D1Error> {
    let drop_sql = SchemaCatalog::drop_table_if_exists_sql(&SchemaCatalog::staging_table_name(table_name));
    let queries = [D1Query { sql: &drop_sql, params: Vec::new() }, save_file_progress_query(file, checkpoint, FileImportStatus::Completed)];
    self.reserved_metadata_batch(db_id, &queries, reservation).await?;
    Ok(())
  }

  pub async fn commit_provider_swap(&self, db_id: &str, swaps: &[StagedTableSwap<'_>], remote_etag: Option<&str>) -> Result<(), D1Error> {
    let etag = remote_etag.filter(|etag| !etag.is_empty());
    if swaps.is_empty() && etag.is_none() {
      return Ok(());
    }
    let mut owned_sql = Vec::with_capacity(swaps.len() * 2);
    for swap in swaps {
      let staging_table = SchemaCatalog::staging_table_name(swap.table_name);
      owned_sql.push(SchemaCatalog::drop_table_if_exists_sql(swap.table_name));
      owned_sql.push(SchemaCatalog::rename_table_sql(&staging_table, swap.table_name));
    }

    let mut queries = Vec::with_capacity(swaps.len() * 3 + usize::from(etag.is_some()));
    for (index, swap) in swaps.iter().enumerate() {
      queries.push(D1Query { sql: &owned_sql[index * 2], params: Vec::new() });
      queries.push(D1Query { sql: &owned_sql[index * 2 + 1], params: Vec::new() });
      queries.push(D1Query { sql: COMPLETE_FILE_PROGRESS_SQL, params: vec![swap.file.into()] });
    }
    if let Some(etag) = etag {
      queries.push(D1Query { sql: SET_DATASET_VERSION_SQL, params: vec![etag.into()] });
    }
    let swap_writes = swaps.len() as u64 * (METADATA_STATEMENT_WRITE_RESERVE + SWAP_RENAME_WRITE_RESERVE + METADATA_STATEMENT_WRITE_RESERVE);
    let etag_writes = u64::from(etag.is_some()) * METADATA_STATEMENT_WRITE_RESERVE;
    self.metadata_batch(db_id, &queries, swap_writes + etag_writes).await?;
    Ok(())
  }

  pub async fn get_database_info(&self, db_id: &str) -> Result<D1DatabaseInfo, D1Error> {
    let _permit = self.acquire_permit().await?;
    self.transport.get_database_info(db_id).await
  }

  pub async fn get_database_count(&self) -> Result<u64, D1Error> {
    let _permit = self.acquire_permit().await?;
    self.transport.get_database_count().await
  }

  pub async fn create_database(&self, name: &str) -> Result<String, D1Error> {
    let _permit = self.acquire_permit().await?;
    self.transport.create_database(name).await
  }

  pub async fn execute_schema(&self, db_id: &str, statements: &[&str]) -> Result<(), D1Error> {
    let queries = statements.iter().map(|statement| statement.trim()).filter(|statement| !statement.is_empty()).map(|sql| D1Query { sql, params: Vec::new() }).collect::<Vec<_>>();
    if queries.is_empty() {
      return Ok(());
    }
    // Schema statements are pre-split at compile time, ordered, and transactional,
    // so send them as one batch instead of one REST request per statement.
    // Keep retries disabled because an ambiguous DDL response is not
    // necessarily safe to replay.
    self.metadata_batch(db_id, &queries, queries.len() as u64 * METADATA_STATEMENT_WRITE_RESERVE).await?;
    Ok(())
  }

  pub async fn delete_database(&self, db_id: &str) -> Result<(), D1Error> {
    let _permit = self.acquire_permit().await?;
    self.transport.delete_database(db_id).await
  }
}

fn save_file_progress_query(file: &str, checkpoint: Checkpoint, status: FileImportStatus) -> D1Query<'static> {
  let status = status.as_i64();
  D1Query { sql: SAVE_FILE_PROGRESS_SQL, params: vec![checkpoint.line.into(), checkpoint.byte.into(), status.into(), file.into(), checkpoint.line.into(), checkpoint.byte.into(), status.into()] }
}

fn sum_rows_written(results: &[D1Result]) -> Option<u64> {
  results.iter().try_fold(0_u64, |total, result| result.rows_written().and_then(|writes| total.checked_add(writes)))
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
#[path = "../../test/unit/importer/d1.rs"]
pub(crate) mod tests;
