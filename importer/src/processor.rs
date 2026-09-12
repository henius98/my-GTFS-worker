use crate::config::{ProviderConfig, RuntimeConfig};
use crate::d1::{D1Client, D1Error, D1Query, FileProgressInit};
use reqwest::{
  StatusCode,
  header::{CONTENT_LENGTH, ETAG, IF_NONE_MATCH},
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{
  Arc, Mutex,
  atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::task::{JoinError, JoinSet};
use zip::ZipArchive;

static TEMP_FEED_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_ARCHIVE_ENTRIES: usize = 256;
// Every imported table has at most one primary/unique index. D1 bills one row
// write for the table and one for that index in the worst case.
const MAX_D1_WRITES_PER_LOGICAL_ROW: u64 = 2;
// The CSV reader buffers ahead. This small allowance lets it find the end of a
// record before the exact byte-span check below rejects an oversized record.
const CSV_RECORD_READ_AHEAD_BYTES: u64 = 64 * 1024;

pub struct TempFeedStorage {
  used_bytes: AtomicU64,
  maximum_bytes: u64,
}

impl TempFeedStorage {
  pub fn new(maximum_bytes: u64) -> Self {
    Self {
      used_bytes: AtomicU64::new(0),
      maximum_bytes,
    }
  }

  fn reserve(&self, bytes: u64) -> Result<(), io::Error> {
    self
      .used_bytes
      .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(bytes).filter(|total| *total <= self.maximum_bytes))
      .map(|_| ())
      .map_err(|current| {
        io::Error::other(format!(
          "temporary feed storage would exceed its {}-byte limit ({current} bytes already reserved, {bytes} requested)",
          self.maximum_bytes
        ))
      })
  }

  fn release(&self, bytes: u64) {
    self.used_bytes.fetch_sub(bytes, Ordering::Relaxed);
  }
}

#[derive(Error, Debug)]
pub enum ProcessorError {
  #[error("D1 error: {0}")]
  D1(#[from] D1Error),
  #[error("Reqwest error: {0}")]
  Reqwest(#[from] reqwest::Error),
  #[error("Zip error: {0}")]
  Zip(#[from] zip::result::ZipError),
  #[error("CSV error: {0}")]
  Csv(#[from] csv::Error),
  #[error("I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("JSON error: {0}")]
  Json(#[from] serde_json::Error),
  #[error("serialized JSON was unexpectedly not UTF-8: {0}")]
  JsonUtf8(#[from] std::string::FromUtf8Error),
  #[error("task failed to join: {0}")]
  TaskJoin(#[from] JoinError),
  #[error("batch uploader closed before extraction completed")]
  UploaderClosed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Checkpoint {
  line: u64,
  byte: u64,
}

struct SerializedBatch {
  payload: String,
  start: Checkpoint,
  end: Checkpoint,
}

enum BatchMessage {
  InitSql(String),
  Data(SerializedBatch),
}

struct BatchTaskResult {
  ranges: Vec<(Checkpoint, Checkpoint)>,
  result: Result<(), ProcessorError>,
}

struct UploadOutcome {
  committed_through: Checkpoint,
  had_error: bool,
  write_budget_exhausted: bool,
}

struct TemporaryFeed {
  path: PathBuf,
  storage: Arc<TempFeedStorage>,
  reserved_bytes: u64,
}

impl Drop for TemporaryFeed {
  fn drop(&mut self) {
    match std::fs::remove_file(&self.path) {
      Ok(()) => self.storage.release(self.reserved_bytes),
      Err(error) if error.kind() == io::ErrorKind::NotFound => self.storage.release(self.reserved_bytes),
      Err(error) => {
        eprintln!("Failed to remove temporary feed {}: {error}", self.path.display());
      }
    }
  }
}

impl TemporaryFeed {
  fn reserve(&mut self, bytes: u64) -> Result<(), io::Error> {
    self.storage.reserve(bytes)?;
    match self.reserved_bytes.checked_add(bytes) {
      Some(total) => {
        self.reserved_bytes = total;
        Ok(())
      }
      None => {
        self.storage.release(bytes);
        Err(io::Error::other("temporary feed reservation overflowed u64"))
      }
    }
  }
}

struct DownloadedFeed {
  file: Arc<TemporaryFeed>,
  size: u64,
  previous_etag: String,
  remote_etag: String,
}

struct PreparedFile {
  csv_file: String,
  crc: String,
  table_name: String,
  db_columns: &'static [&'static str],
  checkpoint: Checkpoint,
}

struct DiscoveredFile {
  csv_file: String,
  crc: String,
  table_name: String,
  db_columns: &'static [&'static str],
}

pub struct PreparedProvider {
  provider: ProviderConfig,
  download: DownloadedFeed,
  files: Vec<PreparedFile>,
}

pub struct ProviderRunOutcome {
  pub remaining: Option<PreparedProvider>,
  pub rows_processed: u64,
  pub write_budget_exhausted: bool,
}

impl PreparedProvider {
  pub fn name(&self) -> &str {
    &self.provider.name
  }
}

#[derive(Clone)]
struct BatchWorker {
  d1_client: D1Client,
  database_id: Arc<str>,
  insert_sql: Arc<str>,
  database_semaphore: Arc<tokio::sync::Semaphore>,
  provider_name: Arc<str>,
}

impl BatchWorker {
  async fn flush_group(self: Arc<Self>, batches: Vec<SerializedBatch>) -> BatchTaskResult {
    let ranges = batches.iter().map(|batch| (batch.start, batch.end)).collect();
    let result = async {
      let _permit = self
        .database_semaphore
        .acquire()
        .await
        .map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to acquire per-database D1 permit: {error}"))))?;
      let logical_rows = batches.iter().try_fold(0_u64, |total, batch| {
        total
          .checked_add(batch.end.line.saturating_sub(batch.start.line))
          .ok_or_else(|| ProcessorError::D1(D1Error::ApiError("D1 batch logical-row reservation overflowed u64".to_owned())))
      })?;
      let maximum_writes = logical_rows
        .checked_mul(MAX_D1_WRITES_PER_LOGICAL_ROW)
        .ok_or_else(|| ProcessorError::D1(D1Error::ApiError("D1 batch write reservation overflowed u64".to_owned())))?;
      let reservation = self.d1_client.reserve_import_writes(maximum_writes).await?;
      let queries = batches
        .into_iter()
        .map(|batch| D1Query {
          sql: self.insert_sql.as_ref(),
          params: vec![serde_json::Value::String(batch.payload)],
        })
        .collect::<Vec<_>>();
      let (results, had_ambiguous_retry) = match self.d1_client.batch_with_retry_state(self.database_id.as_ref(), &queries).await {
        Ok(outcome) => outcome,
        Err(error) => {
          // A terminal transport failure may be ambiguous. Consuming the
          // full reservation prevents later work from spending it again.
          reservation.consume_all();
          return Err(error.into());
        }
      };
      let actual_writes = results.iter().try_fold(0_u64, |total, result| result.rows_written().and_then(|writes| total.checked_add(writes)));
      let Some(actual_writes) = actual_writes else {
        reservation.consume_all();
        return Err(ProcessorError::D1(D1Error::ApiError(
          "D1 response omitted or overflowed rows_written metadata; consumed the full write reservation".to_owned(),
        )));
      };
      println!(
        "[{}] {}: {logical_rows} logical rows, {actual_writes} D1 writes",
        self.provider_name,
        self.insert_sql.split_whitespace().nth(2).unwrap_or("unknown table")
      );
      if had_ambiguous_retry {
        reservation.consume_all();
      } else {
        reservation.finish(actual_writes)?;
      }
      Ok(())
    }
    .await;

    BatchTaskResult { ranges, result }
  }

  fn spawn_group(self: &Arc<Self>, batch_tasks: &mut JoinSet<BatchTaskResult>, pending: &mut Vec<SerializedBatch>) {
    let next_capacity = pending.capacity();
    let batches = std::mem::replace(pending, Vec::with_capacity(next_capacity));
    let worker = self.clone();
    batch_tasks.spawn(async move { worker.flush_group(batches).await });
  }
}

pub fn parse_provider_schemas(provider_name: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
  include!(concat!(env!("OUT_DIR"), "/schemas.rs"))
}

pub fn get_provider_schema_sql(provider_name: &str) -> Option<&'static [&'static str]> {
  include!(concat!(env!("OUT_DIR"), "/schema_sql.rs"))
}

fn get_resume_state(row: Option<&serde_json::Value>, csv_file: &str, file_crc: &str, provider_name: &str) -> Option<Checkpoint> {
  let Some(row) = row else {
    return Some(Checkpoint::default());
  };

  let db_crc = row.get("CRC").and_then(serde_json::Value::as_str).unwrap_or("");
  if db_crc != file_crc {
    println!("[{}] File {} changed (CRC: {} -> {}). Restarting file.", provider_name, csv_file, db_crc, file_crc);
    return Some(Checkpoint::default());
  }

  let status = row.get("Status").and_then(serde_json::Value::as_i64).unwrap_or(-1);
  if status == 0 {
    println!("[{}] Skipping {}, already completed.", provider_name, csv_file);
    return None;
  }

  Some(Checkpoint {
    line: row.get("LastProcessedLine").and_then(serde_json::Value::as_u64).unwrap_or(0),
    byte: row.get("LastProcessedByte").and_then(serde_json::Value::as_u64).unwrap_or(0),
  })
}

struct CsvExtractJob {
  feed: Arc<TemporaryFeed>,
  provider_name: String,
  csv_file: String,
  table_name: String,
  db_columns: &'static [&'static str],
  checkpoint: Checkpoint,
  batch_size: usize,
  max_rows: u64,
  max_record_bytes: u64,
  max_payload_bytes: u64,
}

struct CsvExtractOutcome {
  file_done: bool,
  checkpoint: Checkpoint,
  write_budget_exhausted: bool,
}

struct RecordSizeLimiter<R> {
  inner: R,
  maximum_bytes: u64,
  remaining_bytes: u64,
}

impl<R> RecordSizeLimiter<R> {
  fn new(inner: R, maximum_bytes: u64) -> Self {
    Self {
      inner,
      maximum_bytes,
      remaining_bytes: maximum_bytes.saturating_add(CSV_RECORD_READ_AHEAD_BYTES),
    }
  }

  fn reset(&mut self) {
    self.remaining_bytes = self.maximum_bytes.saturating_add(CSV_RECORD_READ_AHEAD_BYTES);
  }
}

impl<R: Read> Read for RecordSizeLimiter<R> {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
      return Ok(0);
    }
    if self.remaining_bytes == 0 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("CSV record exceeded the configured {}-byte limit", self.maximum_bytes),
      ));
    }

    let allowed = usize::try_from(self.remaining_bytes.min(buffer.len() as u64)).unwrap_or(buffer.len());
    let read = self.inner.read(&mut buffer[..allowed])?;
    self.remaining_bytes = self.remaining_bytes.saturating_sub(read as u64);
    Ok(read)
  }
}

fn validate_record_span(start: u64, end: u64, maximum_bytes: u64) -> Result<(), ProcessorError> {
  let record_bytes = end.saturating_sub(start);
  if record_bytes > maximum_bytes {
    return Err(
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("CSV record is {record_bytes} bytes, exceeding the configured {maximum_bytes}-byte limit"),
      )
      .into(),
    );
  }
  Ok(())
}

fn read_bounded_record<R: Read>(reader: &mut csv::Reader<RecordSizeLimiter<R>>, record: &mut csv::StringRecord, maximum_bytes: u64) -> Result<bool, ProcessorError> {
  let start = reader.position().byte();
  reader.get_mut().reset();
  let has_record = reader.read_record(record)?;
  validate_record_span(start, reader.position().byte(), maximum_bytes)?;
  Ok(has_record)
}

fn extract_and_batch_csv(job: CsvExtractJob, rows_processed_this_run: Arc<AtomicU64>, tx: tokio::sync::mpsc::Sender<BatchMessage>) -> Result<CsvExtractOutcome, ProcessorError> {
  if rows_processed_this_run.load(Ordering::Relaxed) >= job.max_rows {
    return Ok(CsvExtractOutcome {
      file_done: false,
      checkpoint: job.checkpoint,
      write_budget_exhausted: false,
    });
  }

  let mut archive = ZipArchive::new(File::open(&job.feed.path)?)?;
  let (headers, header_end) = {
    let file = archive.by_name(&job.csv_file)?;
    let mut header_reader = csv::ReaderBuilder::new()
      .has_headers(true)
      .flexible(true)
      .from_reader(RecordSizeLimiter::new(file, job.max_record_bytes));
    let headers = header_reader.headers()?.clone();
    let header_end = header_reader.position().byte();
    validate_record_span(0, header_end, job.max_record_bytes)?;
    (headers, header_end)
  };

  let mut csv_indices = Vec::with_capacity(job.db_columns.len());
  let mut matched_cols = Vec::with_capacity(job.db_columns.len());
  for &db_column in job.db_columns {
    if let Some(position) = headers.iter().position(|header| header == db_column) {
      csv_indices.push(position);
      matched_cols.push(db_column);
    }
  }

  if matched_cols.is_empty() {
    println!("[{}] No matching columns in {}, skipping", job.provider_name, job.csv_file);
    return Ok(CsvExtractOutcome {
      file_done: true,
      checkpoint: job.checkpoint,
      write_budget_exhausted: false,
    });
  }

  let insert_sql = positional_insert_sql(&job.table_name, &matched_cols, primary_keys(&job.provider_name, &job.table_name));
  tx.blocking_send(BatchMessage::InitSql(insert_sql)).map_err(|_| ProcessorError::UploaderClosed)?;

  let file = archive.by_name(&job.csv_file)?;
  let uncompressed_size = file.size();
  let has_byte_checkpoint = job.checkpoint.byte != 0;
  if has_byte_checkpoint && job.checkpoint.byte < header_end {
    return Err(
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("saved byte offset {} for {} precedes the CSV header boundary {header_end}", job.checkpoint.byte, job.csv_file),
      )
      .into(),
    );
  }

  let file = if has_byte_checkpoint {
    let mut prefix = file.take(job.checkpoint.byte);
    let skipped = io::copy(&mut prefix, &mut io::sink())?;
    let file = prefix.into_inner();
    if skipped != job.checkpoint.byte {
      return Err(
        io::Error::new(
          io::ErrorKind::UnexpectedEof,
          format!("saved byte offset {} for {} exceeds its {uncompressed_size}-byte uncompressed size", job.checkpoint.byte, job.csv_file),
        )
        .into(),
      );
    }
    file
  } else {
    file
  };

  let mut reader = csv::ReaderBuilder::new()
    .has_headers(!has_byte_checkpoint)
    .flexible(true)
    .from_reader(RecordSizeLimiter::new(file, job.max_record_bytes));
  let mut record = csv::StringRecord::new();
  if !has_byte_checkpoint {
    reader.get_mut().reset();
    let _ = reader.headers()?;
    validate_record_span(0, reader.position().byte(), job.max_record_bytes)?;
    for _ in 0..job.checkpoint.line {
      if !read_bounded_record(&mut reader, &mut record, job.max_record_bytes)? {
        return Ok(CsvExtractOutcome {
          file_done: true,
          checkpoint: Checkpoint {
            line: job.checkpoint.line,
            byte: reader.position().byte(),
          },
          write_budget_exhausted: false,
        });
      }
    }
  }

  let maximum_payload_capacity = usize::try_from(job.max_payload_bytes).unwrap_or(usize::MAX);
  let initial_capacity = job.batch_size.saturating_mul(64).min(256 * 1024).min(maximum_payload_capacity);
  let mut json = Vec::with_capacity(initial_capacity);
  json.push(b'[');
  let mut batch_rows = 0_usize;
  let mut batch_start = job.checkpoint;
  let mut local_checkpoint = job.checkpoint;
  if !has_byte_checkpoint {
    local_checkpoint.byte = reader.position().byte();
  }
  let mut file_done = true;

  loop {
    if rows_processed_this_run.load(Ordering::Relaxed) >= job.max_rows {
      file_done = local_checkpoint.byte >= uncompressed_size;
      break;
    }
    if !read_bounded_record(&mut reader, &mut record, job.max_record_bytes)? {
      break;
    }
    if !try_claim_row(&rows_processed_this_run, job.max_rows) {
      file_done = false;
      break;
    }

    let record_start = local_checkpoint;
    let prefix_length = json.len();
    let previous_batch_rows = batch_rows;
    if previous_batch_rows == 0 {
      batch_start = record_start;
    } else {
      json.push(b',');
    }
    let row_start = json.len();
    append_positional_json_row(&mut json, &record, &csv_indices)?;
    batch_rows += 1;
    let record_end = Checkpoint {
      line: record_start.line.saturating_add(1),
      byte: job.checkpoint.byte.saturating_add(reader.position().byte()),
    };

    let mut serialized_bytes = u64::try_from(json.len().saturating_add(1)).unwrap_or(u64::MAX);
    if serialized_bytes > job.max_payload_bytes && previous_batch_rows != 0 {
      let row_payload = json.split_off(row_start);
      json.truncate(prefix_length);
      batch_rows = previous_batch_rows;
      send_serialized_batch(&tx, &mut json, &mut batch_rows, batch_start, record_start)?;
      batch_start = record_start;
      json.extend_from_slice(&row_payload);
      batch_rows = 1;
      serialized_bytes = u64::try_from(json.len().saturating_add(1)).unwrap_or(u64::MAX);
    }
    if serialized_bytes > job.max_payload_bytes {
      return Err(
        io::Error::new(
          io::ErrorKind::InvalidData,
          format!(
            "serialized CSV row made its statement payload {serialized_bytes} bytes, exceeding the configured {}-byte limit",
            job.max_payload_bytes
          ),
        )
        .into(),
      );
    }
    local_checkpoint = record_end;

    if batch_rows >= job.batch_size || serialized_bytes == job.max_payload_bytes {
      send_serialized_batch(&tx, &mut json, &mut batch_rows, batch_start, local_checkpoint)?;
    }
  }

  if batch_rows != 0 {
    send_serialized_batch(&tx, &mut json, &mut batch_rows, batch_start, local_checkpoint)?;
  }

  Ok(CsvExtractOutcome {
    file_done,
    checkpoint: local_checkpoint,
    write_budget_exhausted: false,
  })
}

fn primary_keys(provider_name: &str, table_name: &str) -> &'static [&'static str] {
  include!(concat!(env!("OUT_DIR"), "/primary_keys.rs"))
}

fn positional_insert_sql(table_name: &str, columns: &[&str], keys: &[&str]) -> String {
  let column_list = columns.iter().map(|column| quote_identifier(column)).collect::<Vec<_>>().join(", ");
  let selects = (0..columns.len()).map(|index| format!("json_extract(value, '$[{index}]')")).collect::<Vec<_>>().join(", ");
  let assignments = columns
    .iter()
    .filter(|column| !keys.contains(column))
    .map(|column| {
      let quoted = quote_identifier(column);
      format!("{quoted} = excluded.{quoted}")
    })
    .collect::<Vec<_>>()
    .join(", ");
  let quoted_table = quote_identifier(table_name);
  if assignments.is_empty() {
    return format!("INSERT INTO {quoted_table} ({column_list}) SELECT {selects} FROM json_each(?) WHERE TRUE ON CONFLICT DO NOTHING");
  }
  let changed = columns
    .iter()
    .filter(|column| !keys.contains(column))
    .map(|column| {
      let quoted = quote_identifier(column);
      format!("{quoted_table}.{quoted} IS NOT excluded.{quoted}")
    })
    .collect::<Vec<_>>()
    .join(" OR ");
  format!("INSERT INTO {quoted_table} ({column_list}) SELECT {selects} FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET {assignments} WHERE {changed}")
}

fn quote_identifier(identifier: &str) -> String {
  format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn append_positional_json_row(json: &mut Vec<u8>, record: &csv::StringRecord, csv_indices: &[usize]) -> Result<(), serde_json::Error> {
  json.push(b'[');
  for (position, &csv_index) in csv_indices.iter().enumerate() {
    if position != 0 {
      json.push(b',');
    }
    serde_json::to_writer(&mut *json, &record.get(csv_index))?;
  }
  json.push(b']');
  Ok(())
}

fn send_serialized_batch(tx: &tokio::sync::mpsc::Sender<BatchMessage>, json: &mut Vec<u8>, batch_rows: &mut usize, start: Checkpoint, end: Checkpoint) -> Result<(), ProcessorError> {
  json.push(b']');
  let next_capacity = json.capacity();
  let payload = String::from_utf8(std::mem::replace(json, Vec::with_capacity(next_capacity)))?;
  json.push(b'[');
  *batch_rows = 0;
  tx.blocking_send(BatchMessage::Data(SerializedBatch { payload, start, end }))
    .map_err(|_| ProcessorError::UploaderClosed)
}

fn try_claim_row(counter: &AtomicU64, maximum: u64) -> bool {
  counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| (current < maximum).then_some(current + 1)).is_ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchCompletion {
  Success,
  WriteBudgetExhausted,
  Failed,
}

fn record_task_result(result: Result<BatchTaskResult, JoinError>, provider_name: &str, statuses: &mut BTreeMap<u64, (Checkpoint, bool)>) -> BatchCompletion {
  match result {
    Ok(BatchTaskResult { ranges, result }) => {
      let completion = match result {
        Ok(()) => BatchCompletion::Success,
        Err(ProcessorError::D1(error @ D1Error::WriteBudgetExhausted { .. })) => {
          println!("[{provider_name}] {error}");
          BatchCompletion::WriteBudgetExhausted
        }
        Err(error) => {
          println!("[{}] D1 batch group failed: {}", provider_name, error);
          BatchCompletion::Failed
        }
      };
      let succeeded = completion == BatchCompletion::Success;
      for (start, end) in ranges {
        statuses.insert(start.line, (end, succeeded));
      }
      completion
    }
    Err(error) => {
      println!("[{}] D1 batch task failed to join: {}", provider_name, error);
      BatchCompletion::Failed
    }
  }
}

fn contiguous_committed_checkpoint(initial: Checkpoint, statuses: &BTreeMap<u64, (Checkpoint, bool)>) -> Checkpoint {
  let mut committed = initial;
  while let Some(&(end, true)) = statuses.get(&committed.line) {
    if end.line <= committed.line || end.byte < committed.byte {
      break;
    }
    committed = end;
  }
  committed
}

fn producer_checkpoint_is_committed(producer: Checkpoint, committed: Checkpoint) -> bool {
  producer.line == committed.line && producer.byte >= committed.byte
}

#[derive(Clone)]
pub struct ProviderProcessor {
  d1_client: D1Client,
  provider: ProviderConfig,
  csv_semaphore: Arc<tokio::sync::Semaphore>,
  database_semaphore: Arc<tokio::sync::Semaphore>,
  runtime: Arc<RuntimeConfig>,
  max_rows: u64,
  rows_processed_this_run: Arc<AtomicU64>,
}

impl ProviderProcessor {
  pub fn new(d1_client: D1Client, provider: ProviderConfig, csv_semaphore: Arc<tokio::sync::Semaphore>, runtime: Arc<RuntimeConfig>, max_rows: u64) -> Self {
    Self {
      d1_client,
      provider,
      csv_semaphore,
      database_semaphore: Arc::new(tokio::sync::Semaphore::new(runtime.d1_database_concurrency_limit)),
      runtime,
      max_rows,
      rows_processed_this_run: Arc::new(AtomicU64::new(0)),
    }
  }

  async fn upload_batches(&self, initial: Checkpoint, mut receiver: tokio::sync::mpsc::Receiver<BatchMessage>) -> UploadOutcome {
    let mut worker: Option<Arc<BatchWorker>> = None;
    let mut pending = Vec::with_capacity(self.runtime.d1_statements_per_request);
    let mut batch_tasks = JoinSet::new();
    let mut statuses = BTreeMap::new();
    let mut had_error = false;
    let mut write_budget_exhausted = false;

    while let Some(message) = receiver.recv().await {
      match message {
        BatchMessage::InitSql(sql) => {
          if worker.is_none() {
            worker = Some(Arc::new(BatchWorker {
              d1_client: self.d1_client.clone(),
              database_id: Arc::from(self.provider.database_id.as_str()),
              insert_sql: Arc::from(sql),
              database_semaphore: self.database_semaphore.clone(),
              provider_name: Arc::from(self.provider.name.as_str()),
            }));
          }
        }
        BatchMessage::Data(batch) => {
          pending.push(batch);
          if pending.len() < self.runtime.d1_statements_per_request {
            continue;
          }

          while batch_tasks.len() >= self.runtime.d1_database_concurrency_limit {
            if let Some(result) = batch_tasks.join_next().await {
              match record_task_result(result, &self.provider.name, &mut statuses) {
                BatchCompletion::Success => {}
                BatchCompletion::WriteBudgetExhausted => write_budget_exhausted = true,
                BatchCompletion::Failed => had_error = true,
              }
              if had_error || write_budget_exhausted {
                break;
              }
            }
          }
          if had_error || write_budget_exhausted {
            break;
          }
          if let Some(worker) = worker.as_ref() {
            worker.spawn_group(&mut batch_tasks, &mut pending);
          } else {
            println!("[{}] Received CSV data before insert SQL initialization", self.provider.name);
            had_error = true;
            break;
          }
        }
      }
    }

    if !had_error && !write_budget_exhausted && !pending.is_empty() {
      if let Some(worker) = worker.as_ref() {
        worker.spawn_group(&mut batch_tasks, &mut pending);
      } else {
        had_error = true;
      }
    }
    drop(receiver);

    while let Some(result) = batch_tasks.join_next().await {
      match record_task_result(result, &self.provider.name, &mut statuses) {
        BatchCompletion::Success => {}
        BatchCompletion::WriteBudgetExhausted => write_budget_exhausted = true,
        BatchCompletion::Failed => had_error = true,
      }
    }

    UploadOutcome {
      committed_through: contiguous_committed_checkpoint(initial, &statuses),
      had_error,
      write_budget_exhausted,
    }
  }

  #[allow(clippy::too_many_arguments)]
  async fn process_csv_file(
    &self,
    feed: Arc<TemporaryFeed>,
    csv_file: &str,
    table_name: &str,
    db_columns: &'static [&'static str],
    checkpoint: Checkpoint,
  ) -> Result<CsvExtractOutcome, ProcessorError> {
    println!("[{}] Importing {} (resuming from row {}, byte {})", self.provider.name, csv_file, checkpoint.line, checkpoint.byte);

    if self.rows_processed_this_run.load(Ordering::Relaxed) >= self.max_rows {
      return Ok(CsvExtractOutcome {
        file_done: false,
        checkpoint,
        write_budget_exhausted: false,
      });
    }

    let progress_reservation = match self.d1_client.reserve_checkpoint().await {
      Ok(reservation) => reservation,
      Err(D1Error::WriteBudgetExhausted { .. }) => {
        return Ok(CsvExtractOutcome {
          file_done: false,
          checkpoint,
          write_budget_exhausted: true,
        });
      }
      Err(error) => return Err(error.into()),
    };
    let csv_permit = self
      .csv_semaphore
      .clone()
      .acquire_owned()
      .await
      .map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to acquire CSV producer permit: {error}"))))?;
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let job = CsvExtractJob {
      feed,
      provider_name: self.provider.name.clone(),
      csv_file: csv_file.to_owned(),
      table_name: table_name.to_owned(),
      db_columns,
      checkpoint,
      batch_size: self.runtime.query_statement_batch_size,
      max_rows: self.max_rows,
      max_record_bytes: self.runtime.max_csv_record_bytes,
      max_payload_bytes: self.runtime.max_statement_payload_bytes,
    };
    let rows_processed = self.rows_processed_this_run.clone();
    let blocking_handle = tokio::task::spawn_blocking(move || {
      let _csv_permit = csv_permit;
      extract_and_batch_csv(job, rows_processed, sender)
    });

    let upload = self.upload_batches(checkpoint, receiver).await;
    let extraction = match blocking_handle.await {
      Ok(result) => result,
      Err(error) => Err(ProcessorError::TaskJoin(error)),
    };

    if upload.had_error {
      self
        .d1_client
        .update_file_progress(
          progress_reservation,
          &self.provider.database_id,
          &self.provider.name,
          csv_file,
          upload.committed_through.line,
          upload.committed_through.byte,
          1,
        )
        .await?;
      if let Err(error) = extraction {
        return Err(error);
      }
      return Err(ProcessorError::D1(D1Error::ApiError("One or more D1 batch groups failed".to_owned())));
    }

    if upload.write_budget_exhausted {
      self
        .d1_client
        .update_file_progress(
          progress_reservation,
          &self.provider.database_id,
          &self.provider.name,
          csv_file,
          upload.committed_through.line,
          upload.committed_through.byte,
          1,
        )
        .await?;
      match extraction {
        Ok(_) | Err(ProcessorError::UploaderClosed) => {}
        Err(error) => return Err(error),
      }
      return Ok(CsvExtractOutcome {
        file_done: false,
        checkpoint: upload.committed_through,
        write_budget_exhausted: true,
      });
    }

    let extraction = match extraction {
      Ok(extraction) => extraction,
      Err(error) => {
        self
          .d1_client
          .update_file_progress(
            progress_reservation,
            &self.provider.database_id,
            &self.provider.name,
            csv_file,
            upload.committed_through.line,
            upload.committed_through.byte,
            1,
          )
          .await?;
        return Err(error);
      }
    };
    if !producer_checkpoint_is_committed(extraction.checkpoint, upload.committed_through) {
      self
        .d1_client
        .update_file_progress(
          progress_reservation,
          &self.provider.database_id,
          &self.provider.name,
          csv_file,
          upload.committed_through.line,
          upload.committed_through.byte,
          1,
        )
        .await?;
      return Err(ProcessorError::D1(D1Error::ApiError(format!(
        "CSV producer reached row {} at byte {}, but only row {} at byte {} was committed",
        extraction.checkpoint.line, extraction.checkpoint.byte, upload.committed_through.line, upload.committed_through.byte,
      ))));
    }

    self
      .d1_client
      .update_file_progress(
        progress_reservation,
        &self.provider.database_id,
        &self.provider.name,
        csv_file,
        extraction.checkpoint.line,
        extraction.checkpoint.byte,
        if extraction.file_done { 0 } else { 1 },
      )
      .await?;
    Ok(extraction)
  }
}

fn discover_supported_files(
  feed: &TemporaryFeed,
  provider_name: &str,
  schemas: &'static [(&'static str, &'static [&'static str])],
  maximum_uncompressed_bytes: u64,
) -> Result<Vec<DiscoveredFile>, ProcessorError> {
  let mut archive = ZipArchive::new(File::open(&feed.path)?)?;
  if archive.len() > MAX_ARCHIVE_ENTRIES {
    return Err(
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("feed contains {} archive entries, exceeding the supported maximum of {MAX_ARCHIVE_ENTRIES}", archive.len()),
      )
      .into(),
    );
  }
  let mut files = Vec::new();
  let mut uncompressed_bytes = 0_u64;
  for index in 0..archive.len() {
    let file = archive.by_index(index)?;
    let csv_file = file.name().to_owned();
    let base_name = csv_file.rsplit('/').next().unwrap_or("");
    if !base_name.ends_with(".txt") || csv_file.contains("__MACOSX") || base_name.starts_with("._") {
      continue;
    }
    let Some(table_name) = base_name.strip_suffix(".txt") else {
      continue;
    };
    if table_name == "daily_import_budget" {
      continue;
    }
    let Some(db_columns) = schemas.iter().find(|(schema_table, _)| *schema_table == table_name).map(|(_, columns)| *columns) else {
      println!("[{provider_name}] Skipping unsupported GTFS file: {table_name} (no schema)");
      continue;
    };
    if db_columns.is_empty() {
      println!("[{provider_name}] Skipping unsupported GTFS file: {table_name} (empty schema)");
      continue;
    }

    uncompressed_bytes = uncompressed_bytes
      .checked_add(file.size())
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "supported CSV sizes overflowed u64"))?;
    if uncompressed_bytes > maximum_uncompressed_bytes {
      return Err(
        io::Error::new(
          io::ErrorKind::InvalidData,
          format!("supported CSV files expand to {uncompressed_bytes} bytes, exceeding the configured {maximum_uncompressed_bytes}-byte limit"),
        )
        .into(),
      );
    }

    let table_name = table_name.to_owned();
    files.push(DiscoveredFile {
      csv_file,
      crc: format!("{:08x}", file.crc32()),
      table_name,
      db_columns,
    });
  }
  Ok(files)
}

pub async fn prepare_provider(
  d1_client: &D1Client,
  mut provider: ProviderConfig,
  providers_file_lock: Arc<Mutex<()>>,
  database_rotation_lock: Arc<tokio::sync::Mutex<()>>,
  runtime: Arc<RuntimeConfig>,
  temp_storage: Arc<TempFeedStorage>,
) -> Result<Option<PreparedProvider>, ProcessorError> {
  rotate_database_if_needed(
    d1_client,
    &mut provider,
    providers_file_lock,
    database_rotation_lock,
    runtime.db_size_threshold_bytes,
    runtime.d1_max_databases,
  )
  .await?;

  let Some(download) = download_feed(d1_client, &provider, runtime.max_feed_download_bytes, temp_storage).await? else {
    return Ok(None);
  };
  if download.remote_etag.is_empty() {
    println!("[{}] Feed supplied no ETag; file CRCs will determine whether work is needed.", provider.name);
  } else if download.remote_etag != download.previous_etag {
    println!("[{}] Feed ETag changed ({} -> {}).", provider.name, download.previous_etag, download.remote_etag);
  }
  println!("[{}] Downloaded {} bytes to temporary storage", provider.name, download.size);

  let schemas = parse_provider_schemas(&provider.name).unwrap_or(&[]);
  let scan_feed = download.file.clone();
  let provider_name = provider.name.clone();
  let maximum_uncompressed_bytes = runtime.max_uncompressed_feed_bytes;
  let discovered = tokio::task::spawn_blocking(move || discover_supported_files(&scan_feed, &provider_name, schemas, maximum_uncompressed_bytes)).await??;

  let progress_rows = d1_client.get_all_files_progress(&provider.database_id, &provider.name).await?;
  let mut files = Vec::with_capacity(discovered.len());
  for file in discovered {
    let row = progress_rows.iter().find(|row| row.get("FileName").and_then(serde_json::Value::as_str) == Some(file.csv_file.as_str()));
    let Some(checkpoint) = get_resume_state(row, &file.csv_file, &file.crc, &provider.name) else {
      continue;
    };
    files.push(PreparedFile {
      csv_file: file.csv_file,
      crc: file.crc,
      table_name: file.table_name,
      db_columns: file.db_columns,
      checkpoint,
    });
  }

  if files.is_empty() {
    if !download.remote_etag.is_empty() {
      d1_client.set_dataset_version(&provider.database_id, &provider.name, &download.remote_etag).await?;
    }
    println!("[{}] No files require import.", provider.name);
    return Ok(None);
  }

  let progress = files
    .iter()
    .map(|file| FileProgressInit {
      file: &file.csv_file,
      crc: &file.crc,
      line: file.checkpoint.line,
      byte: file.checkpoint.byte,
    })
    .collect::<Vec<_>>();
  d1_client.init_files_progress(&provider.database_id, &provider.name, &progress).await?;

  Ok(Some(PreparedProvider { provider, download, files }))
}

pub async fn process_prepared_provider(
  d1_client: &D1Client,
  prepared: PreparedProvider,
  csv_semaphore: Arc<tokio::sync::Semaphore>,
  runtime: Arc<RuntimeConfig>,
  max_rows: u64,
) -> Result<ProviderRunOutcome, ProcessorError> {
  if max_rows == 0 {
    return Err(ProcessorError::D1(D1Error::ApiError("Provider row allocation must be greater than zero".into())));
  }

  let PreparedProvider { provider, download, files } = prepared;
  let processor = Arc::new(ProviderProcessor::new(d1_client.clone(), provider.clone(), csv_semaphore, runtime, max_rows));
  let mut csv_tasks = JoinSet::new();

  for mut file in files {
    let processor = processor.clone();
    let feed = download.file.clone();
    csv_tasks.spawn(async move {
      match processor.process_csv_file(feed, &file.csv_file, &file.table_name, file.db_columns, file.checkpoint).await {
        Ok(outcome) => {
          file.checkpoint = outcome.checkpoint;
          Ok((file, outcome.file_done, outcome.write_budget_exhausted))
        }
        Err(error) => Err((file.csv_file, error)),
      }
    });
  }

  let mut has_csv_error = false;
  let mut write_budget_exhausted = false;
  let mut remaining_files = Vec::new();
  while let Some(result) = csv_tasks.join_next().await {
    match result {
      Ok(Err((csv_file, error))) => {
        println!("[{}] Error processing {}: {}", provider.name, csv_file, error);
        has_csv_error = true;
      }
      Err(error) => {
        println!("[{}] CSV task join error: {}", provider.name, error);
        has_csv_error = true;
      }
      Ok(Ok((file, file_done, file_write_budget_exhausted))) => {
        write_budget_exhausted |= file_write_budget_exhausted;
        if !file_done {
          remaining_files.push(file);
        }
      }
    }
  }
  if has_csv_error {
    return Err(ProcessorError::D1(D1Error::ApiError("One or more CSV files failed".into())));
  }

  remaining_files.sort_by(|left, right| left.csv_file.cmp(&right.csv_file));
  if remaining_files.is_empty() && !download.remote_etag.is_empty() {
    d1_client.set_dataset_version(&provider.database_id, &provider.name, &download.remote_etag).await?;
  }

  let rows_processed = processor.rows_processed_this_run.load(Ordering::Relaxed);
  println!("[{}] Completed run. Processed {} logical rows from a {}-row allocation.", provider.name, rows_processed, max_rows);
  let remaining = (!remaining_files.is_empty()).then_some(PreparedProvider {
    provider,
    download,
    files: remaining_files,
  });
  Ok(ProviderRunOutcome {
    remaining,
    rows_processed,
    write_budget_exhausted,
  })
}

async fn create_temporary_feed(storage: Arc<TempFeedStorage>) -> Result<(tokio::fs::File, TemporaryFeed), ProcessorError> {
  for _ in 0..16 {
    let sequence = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-{}-{timestamp}-{sequence}.zip", std::process::id()));
    match tokio::fs::OpenOptions::new().write(true).create_new(true).open(&path).await {
      Ok(file) => {
        return Ok((file, TemporaryFeed { path, storage, reserved_bytes: 0 }));
      }
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error.into()),
    }
  }
  Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate a unique temporary feed path after 16 attempts").into())
}

async fn download_feed(d1_client: &D1Client, provider: &ProviderConfig, maximum_bytes: u64, storage: Arc<TempFeedStorage>) -> Result<Option<DownloadedFeed>, ProcessorError> {
  let target_url = format!("{}{}", provider.static_url, provider.static_provider);
  let previous_etag = d1_client.get_dataset_version(&provider.database_id, &provider.name).await?.unwrap_or_default();

  println!("[{}] Fetching GTFS zip from {}", provider.name, target_url);
  let mut request = d1_client.client.get(&target_url);
  if !previous_etag.is_empty() {
    request = request.header(IF_NONE_MATCH, previous_etag.as_str());
  }
  let mut response = request.send().await?;

  if response.status() == StatusCode::NOT_MODIFIED {
    let (total, incomplete) = d1_client.get_file_progress_summary(&provider.database_id, &provider.name).await?;
    if total != 0 && incomplete == 0 {
      println!("[{}] Feed unchanged and all {} files are complete. Skipping.", provider.name, total);
      return Ok(None);
    }
    println!("[{}] Feed unchanged, but progress is missing or {} files are incomplete. Resuming.", provider.name, incomplete);
    response = d1_client.client.get(&target_url).send().await?;
  }

  response = response.error_for_status()?;
  let remote_etag = response.headers().get(ETAG).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
  if let Some(content_length) = response.headers().get(CONTENT_LENGTH).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok())
    && content_length > maximum_bytes
  {
    return Err(
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("feed declares {content_length} bytes, exceeding the configured {maximum_bytes}-byte download limit"),
      )
      .into(),
    );
  }
  let (mut file, mut temporary_feed) = create_temporary_feed(storage).await?;
  let mut size = 0_u64;
  while let Some(chunk) = response.chunk().await? {
    size = size
      .checked_add(u64::try_from(chunk.len()).map_err(|_| io::Error::other("feed chunk length does not fit in u64"))?)
      .ok_or_else(|| io::Error::other("downloaded feed size overflowed u64"))?;
    if size > maximum_bytes {
      drop(file);
      return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed exceeded the configured {maximum_bytes}-byte download limit while streaming")).into());
    }
    temporary_feed.reserve(u64::try_from(chunk.len()).map_err(|_| io::Error::other("feed chunk length does not fit in u64"))?)?;
    file.write_all(&chunk).await?;
  }
  file.flush().await?;
  drop(file);
  Ok(Some(DownloadedFeed {
    file: Arc::new(temporary_feed),
    size,
    previous_etag,
    remote_etag,
  }))
}

async fn rotate_database_if_needed(
  d1_client: &D1Client,
  provider: &mut ProviderConfig,
  providers_file_lock: Arc<Mutex<()>>,
  database_rotation_lock: Arc<tokio::sync::Mutex<()>>,
  threshold_bytes: u64,
  max_databases: usize,
) -> Result<(), ProcessorError> {
  let database = d1_client.get_database_info(&provider.database_id).await?;
  if database.file_size < threshold_bytes {
    return Ok(());
  }
  if d1_client.is_budget_database(&provider.database_id) {
    return Err(ProcessorError::D1(D1Error::ApiError(
      "The daily-budget database cannot rotate automatically; preserve its ledger when moving it".into(),
    )));
  }

  let _rotation_guard = database_rotation_lock.lock().await;
  let database_count = d1_client.get_database_count().await?;
  let database_limit = u64::try_from(max_databases).unwrap_or(u64::MAX);
  if database_count >= database_limit {
    return Err(ProcessorError::D1(D1Error::ApiError(format!(
      "Database rotation requires a new D1 database, but the account already has {database_count}/{max_databases}. Archive and delete an obsolete database before retrying."
    ))));
  }

  println!("[{}] Database size {} exceeds threshold {}. Rotating...", provider.name, database.file_size, threshold_bytes);
  let now = chrono::Utc::now();
  let new_db_name = format!("gtfs-{}-db-{}", provider.name, now.format("%Y%m%d%H%M%S"));
  let new_uuid = d1_client.create_database(&new_db_name).await?;
  println!("[{}] Created new database {} with UUID {}", provider.name, new_db_name, new_uuid);

  let setup = async {
    if let Some(schema_sqls) = get_provider_schema_sql(&provider.name) {
      println!("[{}] Applying schemas to new database...", provider.name);
      for schema_sql in schema_sqls {
        d1_client.execute_schema(&new_uuid, schema_sql).await?;
      }
    } else {
      return Err(ProcessorError::D1(D1Error::ApiError(format!("No schema SQL found for provider {}", provider.name))));
    }

    persist_database_id(
      "providers.toml".to_owned(),
      provider.name.clone(),
      provider.database_id.clone(),
      database.name,
      new_uuid.clone(),
      now.to_rfc3339(),
      providers_file_lock,
    )
    .await
  }
  .await;

  if let Err(error) = setup {
    println!("[{}] Failed to set up new database. Deleting orphaned DB {}: {}", provider.name, new_uuid, error);
    let _ = d1_client.delete_database(&new_uuid).await;
    return Err(error);
  }

  provider.database_id = new_uuid;
  Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn persist_database_id(
  path: String,
  provider_name: String,
  retired_database_id: String,
  retired_database_name: String,
  database_id: String,
  retired_at: String,
  file_lock: Arc<Mutex<()>>,
) -> Result<(), ProcessorError> {
  tokio::task::spawn_blocking(move || {
    let _guard = file_lock
      .lock()
      .map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("providers.toml lock was poisoned: {error}"))))?;
    let content = std::fs::read_to_string(&path).map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to read {path}: {error}"))))?;
    let mut document = content
      .parse::<toml_edit::DocumentMut>()
      .map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to parse {path}: {error}"))))?;
    let mut updated = false;
    if let Some(providers) = document.get_mut("providers").and_then(toml_edit::Item::as_array_of_tables_mut) {
      for provider in providers.iter_mut() {
        if provider.get("name").and_then(toml_edit::Item::as_str) == Some(provider_name.as_str()) {
          provider["database_id"] = toml_edit::value(database_id.clone());
          updated = true;
          break;
        }
      }
    }
    if !updated {
      return Err(ProcessorError::D1(D1Error::ApiError(format!("Provider {provider_name} was not found in {path}"))));
    }

    let retired_databases = document
      .entry("retired_databases")
      .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
      .as_array_of_tables_mut()
      .ok_or_else(|| ProcessorError::D1(D1Error::ApiError(format!("retired_databases in {path} is not an array of tables"))))?;
    let already_recorded = retired_databases
      .iter()
      .any(|database| database.get("database_id").and_then(toml_edit::Item::as_str) == Some(retired_database_id.as_str()));
    if !already_recorded {
      let mut retired = toml_edit::Table::new();
      retired["provider"] = toml_edit::value(provider_name);
      retired["name"] = toml_edit::value(retired_database_name);
      retired["database_id"] = toml_edit::value(retired_database_id);
      retired["retired_at"] = toml_edit::value(retired_at);
      retired_databases.push(retired);
    }

    let temporary_path = format!("{path}.tmp");
    std::fs::write(&temporary_path, document.to_string()).map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to write {temporary_path}: {error}"))))?;
    std::fs::rename(&temporary_path, &path).map_err(|error| {
      let _ = std::fs::remove_file(&temporary_path);
      ProcessorError::D1(D1Error::ApiError(format!("Failed to replace {path}: {error}")))
    })
  })
  .await?
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::{Cursor, Write};

  #[test]
  fn positional_json_escapes_values_and_represents_missing_fields_as_null() {
    let record = csv::StringRecord::from(vec!["quoted \"value\"", "line\nbreak"]);
    let mut json = vec![b'['];
    assert!(append_positional_json_row(&mut json, &record, &[0, 1, 2]).is_ok());
    json.push(b']');
    assert_eq!(String::from_utf8(json).ok().as_deref(), Some(r#"[["quoted \"value\"","line\nbreak",null]]"#));
  }

  #[test]
  fn positional_sql_quotes_identifiers() {
    assert_eq!(
      positional_insert_sql("stop_times", &["trip_id", "stop_sequence"], &[]),
      "INSERT INTO \"stop_times\" (\"trip_id\", \"stop_sequence\") SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]') FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET \"trip_id\" = excluded.\"trip_id\", \"stop_sequence\" = excluded.\"stop_sequence\" WHERE \"stop_times\".\"trip_id\" IS NOT excluded.\"trip_id\" OR \"stop_times\".\"stop_sequence\" IS NOT excluded.\"stop_sequence\""
    );
  }

  #[test]
  fn upserts_preserve_primary_keys_and_skip_key_only_conflicts() {
    let keys = primary_keys("ktmb", "stop_times");
    assert_eq!(keys, &["trip_id", "stop_sequence"]);
    let sql = positional_insert_sql("stop_times", &["trip_id", "stop_sequence", "arrival_time"], keys);
    let update = sql.split_once("DO UPDATE SET").map(|(_, update)| update).unwrap_or_default();
    assert!(update.contains("\"arrival_time\" = excluded.\"arrival_time\""));
    assert!(!update.contains("\"trip_id\""));
    assert!(!update.contains("\"stop_sequence\""));
    assert!(positional_insert_sql("stop_times", keys, keys).ends_with("DO NOTHING"));
    assert_eq!(primary_keys("mybas-johor", "areas"), &["area_id"]);
    assert!(primary_keys("mybas-johor", "fare_leg_rules").is_empty());
  }

  #[test]
  fn byte_checkpoint_resumes_at_a_quoted_record_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let csv = "id,name\n1,alpha\n2,\"two\nlines\"\n3,gamma\n4,delta\n";
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file("stops.txt", options)?;
    writer.write_all(csv.as_bytes())?;
    let archive = writer.finish()?.into_inner();

    let sequence = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-resume-test-{}-{sequence}.zip", std::process::id()));
    std::fs::write(&path, archive)?;
    let reserved_bytes = std::fs::metadata(&path)?.len();
    let storage = Arc::new(TempFeedStorage::new(reserved_bytes));
    storage.reserve(reserved_bytes)?;
    let feed = Arc::new(TemporaryFeed { path, storage, reserved_bytes });

    let (first_tx, mut first_rx) = tokio::sync::mpsc::channel(4);
    let first = extract_and_batch_csv(
      CsvExtractJob {
        feed: feed.clone(),
        provider_name: "test".to_owned(),
        csv_file: "stops.txt".to_owned(),
        table_name: "stops".to_owned(),
        db_columns: &["id", "name"],
        checkpoint: Checkpoint::default(),
        batch_size: 10,
        max_rows: 2,
        max_record_bytes: 1024,
        max_payload_bytes: 32,
      },
      Arc::new(AtomicU64::new(0)),
      first_tx,
    )?;
    assert!(!first.file_done);
    assert_eq!(first.checkpoint.line, 2);
    assert_eq!(usize::try_from(first.checkpoint.byte).ok(), csv.find("3,gamma"));

    let mut first_payloads = Vec::new();
    while let Ok(message) = first_rx.try_recv() {
      if let BatchMessage::Data(batch) = message {
        first_payloads.push(batch.payload);
      }
    }
    assert_eq!(first_payloads, ["[[\"1\",\"alpha\"]]", "[[\"2\",\"two\\nlines\"]]"]);

    let (second_tx, mut second_rx) = tokio::sync::mpsc::channel(4);
    let second = extract_and_batch_csv(
      CsvExtractJob {
        feed,
        provider_name: "test".to_owned(),
        csv_file: "stops.txt".to_owned(),
        table_name: "stops".to_owned(),
        db_columns: &["id", "name"],
        checkpoint: first.checkpoint,
        batch_size: 10,
        max_rows: 10,
        max_record_bytes: 1024,
        max_payload_bytes: 4096,
      },
      Arc::new(AtomicU64::new(0)),
      second_tx,
    )?;
    assert!(second.file_done);
    assert_eq!(
      second.checkpoint,
      Checkpoint {
        line: 4,
        byte: u64::try_from(csv.len())?
      }
    );

    let mut second_payload = None;
    while let Ok(message) = second_rx.try_recv() {
      if let BatchMessage::Data(batch) = message {
        second_payload = Some(batch.payload);
      }
    }
    assert_eq!(second_payload.as_deref(), Some("[[\"3\",\"gamma\"],[\"4\",\"delta\"]]"));
    Ok(())
  }

  #[test]
  fn row_budget_is_exact_under_contention() {
    let counter = Arc::new(AtomicU64::new(0));
    let claimed = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();
    for _ in 0..8 {
      let counter = counter.clone();
      let claimed = claimed.clone();
      threads.push(std::thread::spawn(move || {
        while try_claim_row(&counter, 10_000) {
          claimed.fetch_add(1, Ordering::Relaxed);
        }
      }));
    }
    for thread in threads {
      assert!(thread.join().is_ok());
    }
    assert_eq!(counter.load(Ordering::Relaxed), 10_000);
    assert_eq!(claimed.load(Ordering::Relaxed), 10_000);
  }

  #[test]
  fn temporary_feed_storage_is_globally_bounded() {
    let storage = TempFeedStorage::new(10);
    assert!(storage.reserve(6).is_ok());
    assert!(storage.reserve(5).is_err());
    assert_eq!(storage.used_bytes.load(Ordering::Relaxed), 6);
    storage.release(6);
    assert!(storage.reserve(10).is_ok());
    assert_eq!(storage.used_bytes.load(Ordering::Relaxed), 10);
  }

  #[test]
  fn oversized_csv_record_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let input = format!("id,name\n1,{}\n", "x".repeat(128 * 1024));
    let mut reader = csv::ReaderBuilder::new().has_headers(true).from_reader(RecordSizeLimiter::new(Cursor::new(input), 1024));
    let _ = reader.headers()?;
    let mut record = csv::StringRecord::new();
    let error = match read_bounded_record(&mut reader, &mut record, 1024) {
      Ok(_) => panic!("oversized record must fail"),
      Err(error) => error,
    };
    assert!(error.to_string().contains("CSV record"));
    Ok(())
  }

  #[test]
  fn declared_uncompressed_feed_size_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("stops.txt", zip::write::SimpleFileOptions::default())?;
    writer.write_all(b"id,name\n1,a-name-that-exceeds-the-test-limit\n")?;
    let archive = writer.finish()?.into_inner();

    let sequence = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-expansion-test-{}-{sequence}.zip", std::process::id()));
    std::fs::write(&path, archive)?;
    let reserved_bytes = std::fs::metadata(&path)?.len();
    let storage = Arc::new(TempFeedStorage::new(reserved_bytes));
    storage.reserve(reserved_bytes)?;
    let feed = TemporaryFeed { path, storage, reserved_bytes };

    let error = match discover_supported_files(&feed, "test", &[("stops", &["id", "name"])], 8) {
      Ok(_) => panic!("expanded feed must fail"),
      Err(error) => error,
    };
    assert!(error.to_string().contains("expand"));
    Ok(())
  }

  #[test]
  fn committed_progress_stops_at_first_gap_or_failure() {
    let statuses = BTreeMap::from([
      (0, (Checkpoint { line: 100, byte: 1_000 }, true)),
      (100, (Checkpoint { line: 200, byte: 2_000 }, true)),
      (200, (Checkpoint { line: 300, byte: 3_000 }, false)),
      (300, (Checkpoint { line: 400, byte: 4_000 }, true)),
    ]);
    assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint { line: 200, byte: 2_000 });

    let statuses = BTreeMap::from([(100, (Checkpoint { line: 200, byte: 2_000 }, true))]);
    assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint::default());
  }

  #[test]
  fn byte_only_producer_progress_does_not_require_a_d1_batch() {
    let initial = Checkpoint::default();
    let header_only = Checkpoint { line: 0, byte: 31 };

    assert!(producer_checkpoint_is_committed(header_only, initial));
    assert!(!producer_checkpoint_is_committed(Checkpoint { line: 1, byte: 31 }, initial));
    assert!(!producer_checkpoint_is_committed(initial, header_only));
  }

  #[test]
  fn write_budget_exhaustion_is_not_reported_as_a_batch_failure() {
    let mut statuses = BTreeMap::new();
    let completion = record_task_result(
      Ok(BatchTaskResult {
        ranges: vec![(Checkpoint::default(), Checkpoint { line: 100, byte: 1_000 })],
        result: Err(ProcessorError::D1(D1Error::WriteBudgetExhausted { requested: 200, remaining: 50 })),
      }),
      "test",
      &mut statuses,
    );
    assert_eq!(completion, BatchCompletion::WriteBudgetExhausted);
    assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint::default());
  }

  #[tokio::test]
  async fn rotation_persists_current_and_retired_database_metadata() -> Result<(), Box<dyn std::error::Error>> {
    let sequence = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-providers-test-{}-{sequence}.toml", std::process::id()));
    let temporary = TemporaryFeed {
      path: path.clone(),
      storage: Arc::new(TempFeedStorage::new(1)),
      reserved_bytes: 0,
    };
    std::fs::write(
      &path,
      r#"[[providers]]
name = "test-provider"
database_id = "old-id"
"#,
    )?;

    persist_database_id(
      path.to_string_lossy().into_owned(),
      "test-provider".to_owned(),
      "old-id".to_owned(),
      "old-name".to_owned(),
      "new-id".to_owned(),
      "2026-08-28T00:00:00Z".to_owned(),
      Arc::new(Mutex::new(())),
    )
    .await?;

    let content = std::fs::read_to_string(&path)?;
    assert!(content.parse::<toml_edit::DocumentMut>().is_ok());
    assert!(content.contains("database_id = \"new-id\""));
    assert!(content.contains("[[retired_databases]]"));
    assert!(content.contains("provider = \"test-provider\""));
    assert!(content.contains("name = \"old-name\""));
    assert!(content.contains("database_id = \"old-id\""));
    drop(temporary);
    Ok(())
  }
}
