use crate::config::{ProviderConfig, RuntimeConfig};
use crate::d1::{Checkpoint, D1Client, D1Error, D1Query, FileProgressInit, FileProgressUpdate, StoredFileProgress};
pub use crate::schema::{SchemaCatalog, get_provider_schema_sql, parse_provider_schemas, positional_insert_sql, primary_keys, provider_maintenance_queries};
use reqwest::{
  Client, StatusCode,
  header::{CONTENT_LENGTH, ETAG, IF_NONE_MATCH},
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek};
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
// write for the table and up to one for that index in the worst case.
const MAX_D1_WRITES_PER_LOGICAL_ROW: u64 = 2;
// The CSV reader buffers ahead. This small allowance lets it find the end of a
// record before the exact byte-span check below rejects an oversized record.
const CSV_RECORD_READ_AHEAD_BYTES: u64 = 64 * 1024;
const STOP_TIMES_KEY_PAGE_SIZE: usize = 10_000;
const SELECT_STOP_TIMES_KEYS_FIRST_PAGE_SQL: &str =
  "SELECT \"rowid\" AS rowid, \"stop_id\", \"trip_id\", \"stop_sequence\" FROM \"stop_times\" ORDER BY \"stop_id\", \"trip_id\", \"stop_sequence\" LIMIT ?";
const SELECT_STOP_TIMES_KEYS_AFTER_SQL: &str = "SELECT \"rowid\" AS rowid, \"stop_id\", \"trip_id\", \"stop_sequence\" FROM \"stop_times\" WHERE (\"stop_id\", \"trip_id\", \"stop_sequence\") > (?, ?, ?) ORDER BY \"stop_id\", \"trip_id\", \"stop_sequence\" LIMIT ?";
const DELETE_STALE_STOP_TIMES_SQL: &str = "DELETE FROM \"stop_times\" WHERE rowid IN (SELECT value FROM json_each(?))";

pub struct TempFeedStorage {
  used_bytes: AtomicU64,
  maximum_bytes: u64,
}

impl TempFeedStorage {
  pub fn new(maximum_bytes: u64) -> Self {
    Self { used_bytes: AtomicU64::new(0), maximum_bytes }
  }

  fn reserve(&self, bytes: u64) -> Result<(), io::Error> {
    self
      .used_bytes
      .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(bytes).filter(|total| *total <= self.maximum_bytes))
      .map(|_| ())
      .map_err(|current| io::Error::other(format!("temporary feed storage would exceed its {}-byte limit ({current} bytes already reserved, {bytes} requested)", self.maximum_bytes)))
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializedBatch {
  pub payload: String,
  pub start: Checkpoint,
  pub end: Checkpoint,
  pub rows: u64,
}

struct JsonBatchBuilder {
  json: Vec<u8>,
  batch_rows: usize,
  batch_start: Checkpoint,
  batch_size: usize,
  max_payload_bytes: u64,
  item_description: &'static str,
}

impl JsonBatchBuilder {
  fn new(initial: Checkpoint, batch_size: usize, max_payload_bytes: u64, row_bytes_hint: usize, item_description: &'static str) -> Self {
    let maximum_payload_capacity = usize::try_from(max_payload_bytes).unwrap_or(usize::MAX);
    let initial_capacity = batch_size.saturating_mul(row_bytes_hint).min(256 * 1024).min(maximum_payload_capacity);
    let mut json = Vec::with_capacity(initial_capacity);
    json.push(b'[');
    Self { json, batch_rows: 0, batch_start: initial, batch_size, max_payload_bytes, item_description }
  }

  fn push_row<W, F>(&mut self, start: Checkpoint, end: Checkpoint, write_row: W, mut on_batch: F) -> Result<(), ProcessorError>
  where
    W: FnOnce(&mut Vec<u8>) -> Result<(), serde_json::Error>,
    F: FnMut(SerializedBatch) -> Result<(), ProcessorError>,
  {
    let prefix_length = self.json.len();
    let previous_batch_rows = self.batch_rows;
    if previous_batch_rows == 0 {
      self.batch_start = start;
    } else {
      self.json.push(b',');
    }
    let row_start = self.json.len();
    write_row(&mut self.json)?;
    self.batch_rows += 1;

    let mut serialized_bytes = u64::try_from(self.json.len().saturating_add(1)).unwrap_or(u64::MAX);
    if serialized_bytes > self.max_payload_bytes && previous_batch_rows != 0 {
      let row_payload = self.json.split_off(row_start);
      self.json.truncate(prefix_length);
      self.batch_rows = previous_batch_rows;
      let flushed = self.take_batch(self.batch_start, start)?;
      on_batch(flushed)?;
      self.batch_start = start;
      self.json.extend_from_slice(&row_payload);
      self.batch_rows = 1;
      serialized_bytes = u64::try_from(self.json.len().saturating_add(1)).unwrap_or(u64::MAX);
    }
    if serialized_bytes > self.max_payload_bytes {
      return Err(
        io::Error::new(
          io::ErrorKind::InvalidData,
          format!("serialized {} made its statement payload {serialized_bytes} bytes, exceeding the configured {}-byte limit", self.item_description, self.max_payload_bytes),
        )
        .into(),
      );
    }
    if self.batch_rows >= self.batch_size || serialized_bytes == self.max_payload_bytes {
      let flushed = self.take_batch(self.batch_start, end)?;
      on_batch(flushed)?;
    }
    Ok(())
  }

  fn finish<F>(&mut self, end: Checkpoint, mut on_batch: F) -> Result<(), ProcessorError>
  where
    F: FnMut(SerializedBatch) -> Result<(), ProcessorError>,
  {
    if self.batch_rows != 0 {
      let flushed = self.take_batch(self.batch_start, end)?;
      on_batch(flushed)?;
    }
    Ok(())
  }

  fn take_batch(&mut self, start: Checkpoint, end: Checkpoint) -> Result<SerializedBatch, ProcessorError> {
    self.json.push(b']');
    let next_capacity = self.json.capacity();
    let payload = String::from_utf8(std::mem::replace(&mut self.json, Vec::with_capacity(next_capacity)))?;
    self.json.push(b'[');
    let rows = u64::try_from(self.batch_rows).unwrap_or(u64::MAX);
    self.batch_rows = 0;
    Ok(SerializedBatch { payload, start, end, rows })
  }
}

struct BatchTaskResult {
  ranges: Vec<(Checkpoint, Checkpoint)>,
  result: Result<(), ProcessorError>,
}

struct UploadOutcome {
  committed_through: Checkpoint,
  had_error: bool,
  write_budget_exhausted: bool,
  first_error: Option<ProcessorError>,
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
struct StatementGroupExecutor {
  d1_client: D1Client,
  database_id: Arc<str>,
  sql: Arc<str>,
  database_semaphore: Arc<tokio::sync::Semaphore>,
  provider_name: Arc<str>,
  statements_per_request: usize,
  database_concurrency_limit: usize,
  failure_label: &'static str,
}

impl StatementGroupExecutor {
  async fn execute_payloads(&self, payloads: Vec<String>, logical_rows: u64) -> Result<(), ProcessorError> {
    let queries = payloads.into_iter().map(|payload| D1Query { sql: self.sql.as_ref(), params: vec![serde_json::Value::String(payload)] }).collect::<Vec<_>>();
    let _permit = self.database_semaphore.acquire().await.map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to acquire per-database D1 permit: {error}"))))?;
    let maximum_writes = logical_rows.checked_mul(MAX_D1_WRITES_PER_LOGICAL_ROW).ok_or_else(|| ProcessorError::D1(D1Error::ApiError("D1 batch write reservation overflowed u64".to_owned())))?;
    let reservation = self.d1_client.reserve_import_writes(maximum_writes).await?;
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
      return Err(ProcessorError::D1(D1Error::ApiError("D1 response omitted or overflowed rows_written metadata; consumed the full write reservation".to_owned())));
    };
    let operation = self.sql.split_whitespace().next().unwrap_or("SQL");
    let table = self.sql.split_whitespace().nth(2).unwrap_or("unknown table");
    println!("[{}] {operation} {table}: {logical_rows} logical rows, {actual_writes} D1 writes", self.provider_name);
    if had_ambiguous_retry {
      reservation.consume_all();
    } else {
      reservation.finish(actual_writes)?;
    }
    Ok(())
  }

  async fn flush_group(self: Arc<Self>, batches: Vec<SerializedBatch>) -> BatchTaskResult {
    let ranges = batches.iter().map(|batch| (batch.start, batch.end)).collect();
    let result = async {
      let overflow = || ProcessorError::D1(D1Error::ApiError("D1 batch logical-row reservation overflowed u64".to_owned()));
      let logical_rows = batches.iter().try_fold(0_u64, |total, batch| total.checked_add(batch.rows).ok_or_else(overflow))?;
      let payloads = batches.into_iter().map(|batch| batch.payload).collect();
      self.execute_payloads(payloads, logical_rows).await
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

  async fn execute_channel(self: Arc<Self>, initial: Checkpoint, mut receiver: tokio::sync::mpsc::Receiver<SerializedBatch>) -> UploadOutcome {
    let mut pending = Vec::with_capacity(self.statements_per_request);
    let mut batch_tasks = JoinSet::new();
    let mut statuses = BTreeMap::new();
    let mut had_error = false;
    let mut write_budget_exhausted = false;
    let mut first_error = None;

    while let Some(batch) = receiver.recv().await {
      pending.push(batch);
      if pending.len() < self.statements_per_request {
        continue;
      }

      while batch_tasks.len() >= self.database_concurrency_limit {
        if let Some(result) = batch_tasks.join_next().await {
          let (completion, error) = record_task_result(result, &self.provider_name, self.failure_label, &mut statuses);
          match completion {
            BatchCompletion::Success => {}
            BatchCompletion::WriteBudgetExhausted => write_budget_exhausted = true,
            BatchCompletion::Failed => {
              had_error = true;
              if first_error.is_none() {
                first_error = error;
              }
            }
          }
          if had_error || write_budget_exhausted {
            break;
          }
        }
      }
      if had_error || write_budget_exhausted {
        break;
      }
      self.spawn_group(&mut batch_tasks, &mut pending);
    }

    if !had_error && !write_budget_exhausted && !pending.is_empty() {
      self.spawn_group(&mut batch_tasks, &mut pending);
    }
    drop(receiver);

    while let Some(result) = batch_tasks.join_next().await {
      let (completion, error) = record_task_result(result, &self.provider_name, self.failure_label, &mut statuses);
      match completion {
        BatchCompletion::Success => {}
        BatchCompletion::WriteBudgetExhausted => write_budget_exhausted = true,
        BatchCompletion::Failed => {
          had_error = true;
          if first_error.is_none() {
            first_error = error;
          }
        }
      }
    }

    UploadOutcome { committed_through: contiguous_committed_checkpoint(initial, &statuses), had_error, write_budget_exhausted, first_error }
  }

  async fn execute_batches(self: Arc<Self>, batches: Vec<SerializedBatch>) -> Result<bool, ProcessorError> {
    if batches.is_empty() {
      return Ok(true);
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(batches.len().max(1));
    for batch in batches {
      let _ = sender.send(batch).await;
    }
    drop(sender);
    let outcome = self.execute_channel(Checkpoint::default(), receiver).await;
    if let Some(error) = outcome.first_error {
      return Err(error);
    }
    if outcome.had_error {
      return Err(ProcessorError::D1(D1Error::ApiError("One or more D1 delete batch groups failed".to_owned())));
    }
    Ok(!outcome.write_budget_exhausted)
  }
}

fn resolve_resume_checkpoint(stored: Option<&StoredFileProgress>, csv_file: &str, file_crc: &str, provider_name: &str) -> Option<Checkpoint> {
  let Some(stored) = stored else {
    return Some(Checkpoint::default());
  };

  if stored.crc != file_crc {
    println!("[{}] File {} changed (CRC: {} -> {}). Restarting file.", provider_name, csv_file, stored.crc, file_crc);
    return Some(Checkpoint::default());
  }

  if stored.completed {
    println!("[{}] Skipping {}, already completed.", provider_name, csv_file);
    return None;
  }

  Some(stored.checkpoint)
}

struct CsvExtractJob {
  feed: Arc<TemporaryFeed>,
  csv_file: String,
  csv_indices: Vec<usize>,
  header_end: u64,
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

pub struct RecordSizeLimiter<R> {
  inner: R,
  maximum_bytes: u64,
  remaining_bytes: u64,
}

impl<R> RecordSizeLimiter<R> {
  pub fn new(inner: R, maximum_bytes: u64) -> Self {
    Self { inner, maximum_bytes, remaining_bytes: maximum_bytes.saturating_add(CSV_RECORD_READ_AHEAD_BYTES) }
  }

  pub fn reset(&mut self) {
    self.remaining_bytes = self.maximum_bytes.saturating_add(CSV_RECORD_READ_AHEAD_BYTES);
  }
}

impl<R: Read> Read for RecordSizeLimiter<R> {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
      return Ok(0);
    }
    if self.remaining_bytes == 0 {
      return Err(io::Error::new(io::ErrorKind::InvalidData, format!("CSV record exceeded the configured {}-byte limit", self.maximum_bytes)));
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
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("CSV record is {record_bytes} bytes, exceeding the configured {maximum_bytes}-byte limit")).into());
  }
  Ok(())
}

pub fn read_bounded_record<R: Read>(reader: &mut csv::Reader<RecordSizeLimiter<R>>, record: &mut csv::StringRecord, maximum_bytes: u64) -> Result<bool, ProcessorError> {
  let start = reader.position().byte();
  reader.get_mut().reset();
  let has_record = reader.read_record(record)?;
  validate_record_span(start, reader.position().byte(), maximum_bytes)?;
  Ok(has_record)
}

pub fn read_zip_csv_headers<R: Read + Seek>(archive: &mut ZipArchive<R>, csv_file: &str, max_record_bytes: u64) -> Result<(csv::StringRecord, u64), ProcessorError> {
  let file = archive.by_name(csv_file)?;
  let mut header_reader = csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(RecordSizeLimiter::new(file, max_record_bytes));
  let headers = header_reader.headers()?.clone();
  let header_end = header_reader.position().byte();
  validate_record_span(0, header_end, max_record_bytes)?;
  Ok((headers, header_end))
}

pub type ZipCsvReader<'a, R> = csv::Reader<RecordSizeLimiter<zip::read::ZipFile<'a, R>>>;

pub fn resume_zip_csv_at_byte_offset<'a, R: Read + Seek>(
  archive: &'a mut ZipArchive<R>,
  csv_file: &str,
  header_end: u64,
  resume_byte: u64,
  max_record_bytes: u64,
) -> Result<(ZipCsvReader<'a, R>, u64), ProcessorError> {
  let file = archive.by_name(csv_file)?;
  let uncompressed_size = file.size();
  let has_byte_checkpoint = resume_byte != 0;
  if has_byte_checkpoint && resume_byte < header_end {
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("saved byte offset {resume_byte} for {csv_file} precedes the CSV header boundary {header_end}")).into());
  }

  let file = if has_byte_checkpoint {
    let mut prefix = file.take(resume_byte);
    let skipped = io::copy(&mut prefix, &mut io::sink())?;
    let file = prefix.into_inner();
    if skipped != resume_byte {
      return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("saved byte offset {resume_byte} for {csv_file} exceeds its {uncompressed_size}-byte uncompressed size")).into());
    }
    file
  } else {
    file
  };

  let reader = csv::ReaderBuilder::new().has_headers(!has_byte_checkpoint).flexible(true).from_reader(RecordSizeLimiter::new(file, max_record_bytes));
  Ok((reader, uncompressed_size))
}

fn extract_and_batch_csv(job: CsvExtractJob, rows_processed_this_run: Arc<AtomicU64>, tx: tokio::sync::mpsc::Sender<SerializedBatch>) -> Result<CsvExtractOutcome, ProcessorError> {
  if rows_processed_this_run.load(Ordering::Relaxed) >= job.max_rows {
    return Ok(CsvExtractOutcome { file_done: false, checkpoint: job.checkpoint, write_budget_exhausted: false });
  }

  let mut archive = ZipArchive::new(File::open(&job.feed.path)?)?;
  let has_byte_checkpoint = job.checkpoint.byte != 0;
  let (mut reader, uncompressed_size) = resume_zip_csv_at_byte_offset(&mut archive, &job.csv_file, job.header_end, job.checkpoint.byte, job.max_record_bytes)?;
  let mut record = csv::StringRecord::new();
  if !has_byte_checkpoint {
    reader.get_mut().reset();
    let _ = reader.headers()?;
    validate_record_span(0, reader.position().byte(), job.max_record_bytes)?;
    for _ in 0..job.checkpoint.line {
      if !read_bounded_record(&mut reader, &mut record, job.max_record_bytes)? {
        return Ok(CsvExtractOutcome { file_done: true, checkpoint: Checkpoint { line: job.checkpoint.line, byte: reader.position().byte() }, write_budget_exhausted: false });
      }
    }
  }

  let mut local_checkpoint = job.checkpoint;
  if !has_byte_checkpoint {
    local_checkpoint.byte = reader.position().byte();
  }
  let mut builder = JsonBatchBuilder::new(local_checkpoint, job.batch_size, job.max_payload_bytes, 64, "CSV row");
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
    let record_end = Checkpoint { line: record_start.line.saturating_add(1), byte: job.checkpoint.byte.saturating_add(reader.position().byte()) };
    let write = |json: &mut Vec<u8>| append_positional_json_row(json, &record, &job.csv_indices);
    let send = |batch| tx.blocking_send(batch).map_err(|_| ProcessorError::UploaderClosed);
    builder.push_row(record_start, record_end, write, send)?;
    local_checkpoint = record_end;
  }

  builder.finish(local_checkpoint, |batch| tx.blocking_send(batch).map_err(|_| ProcessorError::UploaderClosed))?;
  Ok(CsvExtractOutcome { file_done, checkpoint: local_checkpoint, write_budget_exhausted: false })
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

fn try_claim_row(counter: &AtomicU64, maximum: u64) -> bool {
  counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| (current < maximum).then_some(current + 1)).is_ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchCompletion {
  Success,
  WriteBudgetExhausted,
  Failed,
}

fn classify_batch_result(result: Result<(), ProcessorError>, provider_name: &str, kind: &str) -> (BatchCompletion, Option<ProcessorError>) {
  match result {
    Ok(()) => (BatchCompletion::Success, None),
    Err(ProcessorError::D1(error @ D1Error::WriteBudgetExhausted { .. })) => {
      println!("[{provider_name}] {error}");
      (BatchCompletion::WriteBudgetExhausted, None)
    }
    Err(error) => {
      println!("[{provider_name}] D1 {kind} failed: {error}");
      (BatchCompletion::Failed, Some(error))
    }
  }
}

fn record_task_result(result: Result<BatchTaskResult, JoinError>, provider_name: &str, kind: &str, statuses: &mut BTreeMap<u64, (Checkpoint, bool)>) -> (BatchCompletion, Option<ProcessorError>) {
  match result {
    Ok(BatchTaskResult { ranges, result }) => {
      let (completion, error) = classify_batch_result(result, provider_name, kind);
      let succeeded = completion == BatchCompletion::Success;
      for (start, end) in ranges {
        statuses.insert(start.line, (end, succeeded));
      }
      (completion, error)
    }
    Err(error) => {
      println!("[{}] D1 {} task failed to join: {}", provider_name, kind, error);
      (BatchCompletion::Failed, Some(ProcessorError::TaskJoin(error)))
    }
  }
}

#[derive(Default)]
struct CsvStopTimeKeySet {
  stop_ids: HashMap<Box<str>, u32>,
  trip_ids: HashMap<Box<str>, u32>,
  int_keys: HashSet<(u32, u32, i64)>,
  text_keys: HashSet<(Box<str>, Box<str>, Box<str>)>,
}

fn intern_key_part(map: &mut HashMap<Box<str>, u32>, value: &str) -> Result<u32, ProcessorError> {
  if let Some(&id) = map.get(value) {
    return Ok(id);
  }
  let id = u32::try_from(map.len()).map_err(|_| io::Error::other("stop_times interner exceeded u32::MAX entries"))?;
  map.insert(Box::from(value), id);
  Ok(id)
}

fn parse_stop_sequence_integer(raw: &str) -> Option<i64> {
  let trimmed = raw.trim();
  if let Ok(value) = trimmed.parse::<i64>() {
    return Some(value);
  }
  let (whole, fraction) = trimmed.split_once('.')?;
  if !fraction.is_empty() && fraction.bytes().all(|byte| byte == b'0') { whole.parse::<i64>().ok() } else { None }
}

impl CsvStopTimeKeySet {
  fn is_empty(&self) -> bool {
    self.int_keys.is_empty() && self.text_keys.is_empty()
  }

  fn insert(&mut self, stop_id: &str, trip_id: &str, stop_sequence: &str) -> Result<(), ProcessorError> {
    if let Some(sequence) = parse_stop_sequence_integer(stop_sequence) {
      let stop_index = intern_key_part(&mut self.stop_ids, stop_id)?;
      let trip_index = intern_key_part(&mut self.trip_ids, trip_id)?;
      self.int_keys.insert((stop_index, trip_index, sequence));
    } else {
      self.text_keys.insert((Box::from(stop_id), Box::from(trip_id), Box::from(stop_sequence)));
    }
    Ok(())
  }

  fn contains(&self, stop_id: &serde_json::Value, trip_id: &serde_json::Value, stop_sequence: &serde_json::Value) -> bool {
    let (Some(stop_id), Some(trip_id)) = (stop_id.as_str(), trip_id.as_str()) else {
      return false;
    };
    if let Some(sequence) = stop_sequence.as_i64() {
      let (Some(&stop_index), Some(&trip_index)) = (self.stop_ids.get(stop_id), self.trip_ids.get(trip_id)) else {
        return false;
      };
      return self.int_keys.contains(&(stop_index, trip_index, sequence));
    }
    if let Some(sequence) = stop_sequence.as_str() {
      if self.text_keys.is_empty() {
        return false;
      }
      return self.text_keys.contains(&(Box::from(stop_id), Box::from(trip_id), Box::from(sequence)));
    }
    false
  }
}

fn extract_stop_time_keys(feed: &TemporaryFeed, csv_file: &str, max_record_bytes: u64) -> Result<Option<CsvStopTimeKeySet>, ProcessorError> {
  let mut archive = ZipArchive::new(File::open(&feed.path)?)?;
  let file = archive.by_name(csv_file)?;
  let mut reader = csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(RecordSizeLimiter::new(file, max_record_bytes));
  reader.get_mut().reset();
  let headers = reader.headers()?.clone();
  validate_record_span(0, reader.position().byte(), max_record_bytes)?;

  let Some(stop_id_index) = headers.iter().position(|header| header == "stop_id") else {
    return Ok(None);
  };
  let Some(trip_id_index) = headers.iter().position(|header| header == "trip_id") else {
    return Ok(None);
  };
  let Some(stop_sequence_index) = headers.iter().position(|header| header == "stop_sequence") else {
    return Ok(None);
  };

  let mut keys = CsvStopTimeKeySet::default();
  let mut record = csv::StringRecord::new();
  while read_bounded_record(&mut reader, &mut record, max_record_bytes)? {
    if let (Some(stop_id), Some(trip_id), Some(stop_sequence)) = (record.get(stop_id_index), record.get(trip_id_index), record.get(stop_sequence_index)) {
      keys.insert(stop_id, trip_id, stop_sequence)?;
    }
  }

  Ok(Some(keys))
}

struct StopTimePageScan {
  stale_rowids: Vec<i64>,
  last_cursor: Option<[serde_json::Value; 3]>,
}

fn collect_stale_stop_times_from_page(rows: Vec<serde_json::Value>, csv_keys: &CsvStopTimeKeySet) -> Result<StopTimePageScan, ProcessorError> {
  let mut stale_rowids = Vec::new();
  let mut last_cursor = None;

  for row in rows {
    let serde_json::Value::Object(mut object) = row else {
      return Err(ProcessorError::D1(D1Error::ApiError("D1 stop_times key query returned a non-object row".to_owned())));
    };
    let Some(rowid_value) = object.remove("rowid") else {
      return Err(ProcessorError::D1(D1Error::ApiError("D1 stop_times key query omitted rowid".to_owned())));
    };
    let Some(rowid) = rowid_value.as_i64() else {
      return Err(ProcessorError::D1(D1Error::ApiError(format!("D1 stop_times key query returned non-integer rowid: {rowid_value}"))));
    };
    let Some(stop_id) = object.remove("stop_id") else {
      return Err(ProcessorError::D1(D1Error::ApiError("D1 stop_times key query omitted stop_id".to_owned())));
    };
    let Some(trip_id) = object.remove("trip_id") else {
      return Err(ProcessorError::D1(D1Error::ApiError("D1 stop_times key query omitted trip_id".to_owned())));
    };
    let Some(stop_sequence) = object.remove("stop_sequence") else {
      return Err(ProcessorError::D1(D1Error::ApiError("D1 stop_times key query omitted stop_sequence".to_owned())));
    };

    if stop_id.is_null() || trip_id.is_null() || stop_sequence.is_null() {
      stale_rowids.push(rowid);
      continue;
    }

    let key = [stop_id, trip_id, stop_sequence];
    let [stop_id_ref, trip_id_ref, stop_sequence_ref] = &key;
    if !csv_keys.contains(stop_id_ref, trip_id_ref, stop_sequence_ref) {
      stale_rowids.push(rowid);
    }
    last_cursor = Some(key);
  }

  Ok(StopTimePageScan { stale_rowids, last_cursor })
}

fn serialize_delete_rowid_batches(rowids: &[i64], batch_size: usize, max_payload_bytes: u64) -> Result<Vec<SerializedBatch>, ProcessorError> {
  if rowids.is_empty() {
    return Ok(Vec::new());
  }

  let mut batches = Vec::new();
  let mut builder = JsonBatchBuilder::new(Checkpoint::default(), batch_size, max_payload_bytes, 16, "stop_times rowid");
  for (index, &rowid) in rowids.iter().enumerate() {
    let line = u64::try_from(index).unwrap_or(u64::MAX);
    let start = Checkpoint { line, byte: line };
    let end = Checkpoint { line: line.saturating_add(1), byte: line.saturating_add(1) };
    builder.push_row(
      start,
      end,
      |json| serde_json::to_writer(json, &rowid),
      |batch| {
        batches.push(batch);
        Ok(())
      },
    )?;
  }
  let final_line = u64::try_from(rowids.len()).unwrap_or(u64::MAX);
  builder.finish(Checkpoint { line: final_line, byte: final_line }, |batch| {
    batches.push(batch);
    Ok(())
  })?;
  Ok(batches)
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

  fn group_executor(&self, sql: Arc<str>, failure_label: &'static str) -> Arc<StatementGroupExecutor> {
    Arc::new(StatementGroupExecutor {
      d1_client: self.d1_client.clone(),
      database_id: Arc::from(self.provider.database_id.as_str()),
      sql,
      database_semaphore: self.database_semaphore.clone(),
      provider_name: Arc::from(self.provider.name.as_str()),
      statements_per_request: self.runtime.d1_statements_per_request,
      database_concurrency_limit: self.runtime.d1_database_concurrency_limit,
      failure_label,
    })
  }

  async fn purge_stale_stop_times(&self, feed: Arc<TemporaryFeed>, csv_file: &str) -> Result<bool, ProcessorError> {
    self.purge_stale_stop_times_with_page_size(feed, csv_file, STOP_TIMES_KEY_PAGE_SIZE).await
  }

  async fn purge_stale_stop_times_with_page_size(&self, feed: Arc<TemporaryFeed>, csv_file: &str, page_size: usize) -> Result<bool, ProcessorError> {
    let csv_permit = self.csv_semaphore.clone().acquire_owned().await.map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to acquire CSV producer permit: {error}"))))?;
    let csv_file_owned = csv_file.to_owned();
    let max_record_bytes = self.runtime.max_csv_record_bytes;
    let Some(csv_keys) = tokio::task::spawn_blocking(move || {
      let _csv_permit = csv_permit;
      extract_stop_time_keys(&feed, &csv_file_owned, max_record_bytes)
    })
    .await??
    else {
      return Ok(true);
    };

    if csv_keys.is_empty() {
      println!("[{}] Skipping stale stop_times cleanup because {} contained zero rows", self.provider.name, csv_file);
      return Ok(true);
    }

    let delete_executor = self.group_executor(Arc::from(DELETE_STALE_STOP_TIMES_SQL), "delete batch group");
    let mut cursor: Option<[serde_json::Value; 3]> = None;
    let mut deleted_total = 0_u64;

    loop {
      let query = match cursor.as_ref() {
        None => D1Query { sql: SELECT_STOP_TIMES_KEYS_FIRST_PAGE_SQL, params: vec![serde_json::Value::Number(page_size.into())] },
        Some([stop_id, trip_id, stop_sequence]) => {
          D1Query { sql: SELECT_STOP_TIMES_KEYS_AFTER_SQL, params: vec![stop_id.clone(), trip_id.clone(), stop_sequence.clone(), serde_json::Value::Number(page_size.into())] }
        }
      };
      let mut results = match self.d1_client.query_rows(&self.provider.database_id, query).await {
        Ok(results) => results,
        Err(error @ D1Error::WriteBudgetExhausted { .. }) => {
          println!("[{}] {error}", self.provider.name);
          return Ok(false);
        }
        Err(error) => return Err(error.into()),
      };
      let rows = results.pop().map(|result| result.results).unwrap_or_default();
      let page_len = rows.len();
      let scan = collect_stale_stop_times_from_page(rows, &csv_keys)?;

      if !scan.stale_rowids.is_empty() {
        let stale_count = u64::try_from(scan.stale_rowids.len()).unwrap_or(u64::MAX);
        let batches = serialize_delete_rowid_batches(&scan.stale_rowids, self.runtime.query_statement_batch_size, self.runtime.max_statement_payload_bytes)?;
        if !delete_executor.clone().execute_batches(batches).await? {
          return Ok(false);
        }
        deleted_total = deleted_total.saturating_add(stale_count);
      }

      if page_len < page_size {
        break;
      }
      match scan.last_cursor {
        Some(next_cursor) => cursor = Some(next_cursor),
        None if !scan.stale_rowids.is_empty() => {}
        None => {
          return Err(ProcessorError::D1(D1Error::ApiError("stop_times keyset pagination did not produce a cursor".to_owned())));
        }
      }
    }

    if deleted_total != 0 {
      println!("[{}] Removed {} stale stop_times row(s)", self.provider.name, deleted_total);
    }
    Ok(true)
  }

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
      return Ok(CsvExtractOutcome { file_done: false, checkpoint, write_budget_exhausted: false });
    }

    let progress_reservation = match self.d1_client.reserve_checkpoint().await {
      Ok(reservation) => reservation,
      Err(D1Error::WriteBudgetExhausted { .. }) => {
        return Ok(CsvExtractOutcome { file_done: false, checkpoint, write_budget_exhausted: true });
      }
      Err(error) => return Err(error.into()),
    };

    let (checkpoint_to_save, completed, outcome_result) = self.run_csv_import_session(feed, csv_file, table_name, db_columns, checkpoint).await;
    let update = FileProgressUpdate { file: csv_file, checkpoint: checkpoint_to_save, completed };
    self.d1_client.save_file_progress(progress_reservation, &self.provider.database_id, update).await?;
    outcome_result
  }

  async fn run_csv_import_session(
    &self,
    feed: Arc<TemporaryFeed>,
    csv_file: &str,
    table_name: &str,
    db_columns: &'static [&'static str],
    checkpoint: Checkpoint,
  ) -> (Checkpoint, bool, Result<CsvExtractOutcome, ProcessorError>) {
    let csv_permit = match self.csv_semaphore.clone().acquire_owned().await {
      Ok(permit) => permit,
      Err(error) => {
        return (checkpoint, false, Err(ProcessorError::D1(D1Error::ApiError(format!("Failed to acquire CSV producer permit: {error}")))));
      }
    };

    let header_feed = feed.clone();
    let header_csv_file = csv_file.to_owned();
    let max_record_bytes = self.runtime.max_csv_record_bytes;
    let header_scan = tokio::task::spawn_blocking(move || -> Result<(csv::StringRecord, u64), ProcessorError> {
      let mut archive = ZipArchive::new(File::open(&header_feed.path)?)?;
      read_zip_csv_headers(&mut archive, &header_csv_file, max_record_bytes)
    })
    .await;

    let (headers, header_end) = match header_scan {
      Ok(Ok(result)) => result,
      Ok(Err(error)) => return (checkpoint, false, Err(error)),
      Err(error) => return (checkpoint, false, Err(ProcessorError::TaskJoin(error))),
    };

    let Some(plan) = SchemaCatalog::plan_table_import(&self.provider.name, table_name, db_columns, &headers) else {
      println!("[{}] No matching columns in {}, skipping", self.provider.name, csv_file);
      return (checkpoint, true, Ok(CsvExtractOutcome { file_done: true, checkpoint, write_budget_exhausted: false }));
    };

    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let job = CsvExtractJob {
      feed: feed.clone(),
      csv_file: csv_file.to_owned(),
      csv_indices: plan.csv_indices,
      header_end,
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

    let upload = self.group_executor(plan.insert_sql, "batch group").execute_channel(checkpoint, receiver).await;
    let extraction = match blocking_handle.await {
      Ok(result) => result,
      Err(error) => Err(ProcessorError::TaskJoin(error)),
    };

    if upload.had_error {
      let error = upload
        .first_error
        .or_else(|| extraction.err().filter(|error| !matches!(error, ProcessorError::UploaderClosed)))
        .unwrap_or_else(|| ProcessorError::D1(D1Error::ApiError("One or more D1 batch groups failed".to_owned())));
      return (upload.committed_through, false, Err(error));
    }

    if upload.write_budget_exhausted {
      if let Err(error) = extraction
        && !matches!(error, ProcessorError::UploaderClosed)
      {
        return (upload.committed_through, false, Err(error));
      }
      return (upload.committed_through, false, Ok(CsvExtractOutcome { file_done: false, checkpoint: upload.committed_through, write_budget_exhausted: true }));
    }

    let extraction = match extraction {
      Ok(extraction) => extraction,
      Err(error) => return (upload.committed_through, false, Err(error)),
    };

    if !producer_checkpoint_is_committed(extraction.checkpoint, upload.committed_through) {
      return (
        upload.committed_through,
        false,
        Err(ProcessorError::D1(D1Error::ApiError(format!(
          "CSV producer reached row {} at byte {}, but only row {} at byte {} was committed",
          extraction.checkpoint.line, extraction.checkpoint.byte, upload.committed_through.line, upload.committed_through.byte,
        )))),
      );
    }

    if extraction.file_done && table_name == "stop_times" {
      match self.purge_stale_stop_times(feed, csv_file).await {
        Ok(true) => {}
        Ok(false) => {
          return (extraction.checkpoint, false, Ok(CsvExtractOutcome { file_done: false, checkpoint: extraction.checkpoint, write_budget_exhausted: true }));
        }
        Err(error) => {
          return (extraction.checkpoint, false, Err(error));
        }
      }
    }

    (extraction.checkpoint, extraction.file_done, Ok(extraction))
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
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed contains {} archive entries, exceeding the supported maximum of {MAX_ARCHIVE_ENTRIES}", archive.len())).into());
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
    if SchemaCatalog::is_infrastructure_table(table_name) {
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

    uncompressed_bytes = uncompressed_bytes.checked_add(file.size()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "supported CSV sizes overflowed u64"))?;
    if uncompressed_bytes > maximum_uncompressed_bytes {
      return Err(
        io::Error::new(io::ErrorKind::InvalidData, format!("supported CSV files expand to {uncompressed_bytes} bytes, exceeding the configured {maximum_uncompressed_bytes}-byte limit")).into(),
      );
    }

    let table_name = table_name.to_owned();
    files.push(DiscoveredFile { csv_file, crc: format!("{:08x}", file.crc32()), table_name, db_columns });
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
  rotate_database_if_needed(d1_client, &mut provider, providers_file_lock, database_rotation_lock, runtime.db_size_threshold_bytes, runtime.d1_max_databases).await?;

  let user_agent = concat!("my-GTFS-worker/", env!("CARGO_PKG_VERSION"));
  let feed_http_client = Client::builder().connect_timeout(runtime.http_connect_timeout).timeout(runtime.http_request_timeout).user_agent(user_agent).build()?;
  let Some(download) = download_feed(d1_client, &feed_http_client, &provider, runtime.max_feed_download_bytes, temp_storage).await? else {
    return Ok(None);
  };
  if download.remote_etag.is_empty() {
    println!("[{}] Feed supplied no ETag; file CRCs will determine whether work is needed.", provider.name);
  } else if download.remote_etag != download.previous_etag {
    println!("[{}] Feed ETag changed ({} -> {}).", provider.name, download.previous_etag, download.remote_etag);
  }
  println!("[{}] Downloaded {} bytes to temporary storage", provider.name, download.size);

  let schemas = SchemaCatalog::provider_schemas(&provider.name).unwrap_or(&[]);
  let scan_feed = download.file.clone();
  let provider_name = provider.name.clone();
  let maximum_uncompressed_bytes = runtime.max_uncompressed_feed_bytes;
  let discovered = tokio::task::spawn_blocking(move || discover_supported_files(&scan_feed, &provider_name, schemas, maximum_uncompressed_bytes)).await??;

  let progress_rows = d1_client.get_files_progress(&provider.database_id).await?;
  let mut files = Vec::with_capacity(discovered.len());
  for file in discovered {
    let stored = progress_rows.iter().find(|row| row.file_name == file.csv_file);
    let Some(checkpoint) = resolve_resume_checkpoint(stored, &file.csv_file, &file.crc, &provider.name) else {
      continue;
    };
    files.push(PreparedFile { csv_file: file.csv_file, crc: file.crc, table_name: file.table_name, db_columns: file.db_columns, checkpoint });
  }

  if files.is_empty() {
    if !download.remote_etag.is_empty() {
      d1_client.set_dataset_version(&provider.database_id, &download.remote_etag).await?;
    }
    println!("[{}] No files require import.", provider.name);
    return Ok(None);
  }

  let progress = files.iter().map(|file| FileProgressInit { file: &file.csv_file, crc: &file.crc, checkpoint: file.checkpoint }).collect::<Vec<_>>();
  d1_client.init_files_progress(&provider.database_id, &progress).await?;

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
  if remaining_files.is_empty() {
    let maintenance_sqls = SchemaCatalog::maintenance_queries(&provider.name);
    if !maintenance_sqls.is_empty() {
      println!("[{}] All files imported. Running post-ingest data maintenance...", provider.name);
      d1_client.execute_maintenance_statements(&provider.database_id, &maintenance_sqls).await?;
    }
    if !download.remote_etag.is_empty() {
      d1_client.set_dataset_version(&provider.database_id, &download.remote_etag).await?;
    }
  }

  let rows_processed = processor.rows_processed_this_run.load(Ordering::Relaxed);
  println!("[{}] Completed run. Processed {} logical rows from a {}-row allocation.", provider.name, rows_processed, max_rows);
  let remaining = (!remaining_files.is_empty()).then_some(PreparedProvider { provider, download, files: remaining_files });
  Ok(ProviderRunOutcome { remaining, rows_processed, write_budget_exhausted })
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

async fn download_feed(d1_client: &D1Client, http_client: &Client, provider: &ProviderConfig, maximum_bytes: u64, storage: Arc<TempFeedStorage>) -> Result<Option<DownloadedFeed>, ProcessorError> {
  let target_url = format!("{}{}", provider.static_url, provider.static_provider);
  let previous_etag = d1_client.get_dataset_version(&provider.database_id).await?.unwrap_or_default();

  println!("[{}] Fetching GTFS zip from {}", provider.name, target_url);
  let mut request = http_client.get(&target_url);
  if !previous_etag.is_empty() {
    request = request.header(IF_NONE_MATCH, previous_etag.as_str());
  }
  let mut response = request.send().await?;

  if response.status() == StatusCode::NOT_MODIFIED {
    let (total, incomplete) = d1_client.get_file_progress_summary(&provider.database_id).await?;
    if total != 0 && incomplete == 0 {
      println!("[{}] Feed unchanged and all {} files are complete. Skipping.", provider.name, total);
      return Ok(None);
    }
    println!("[{}] Feed unchanged, but progress is missing or {} files are incomplete. Resuming.", provider.name, incomplete);
    response = http_client.get(&target_url).send().await?;
  }

  response = response.error_for_status()?;
  let remote_etag = response.headers().get(ETAG).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
  if let Some(content_length) = response.headers().get(CONTENT_LENGTH).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok())
    && content_length > maximum_bytes
  {
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed declares {content_length} bytes, exceeding the configured {maximum_bytes}-byte download limit")).into());
  }
  let (mut file, mut temporary_feed) = create_temporary_feed(storage).await?;
  let mut size = 0_u64;
  while let Some(chunk) = response.chunk().await? {
    size =
      size.checked_add(u64::try_from(chunk.len()).map_err(|_| io::Error::other("feed chunk length does not fit in u64"))?).ok_or_else(|| io::Error::other("downloaded feed size overflowed u64"))?;
    if size > maximum_bytes {
      drop(file);
      return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed exceeded the configured {maximum_bytes}-byte download limit while streaming")).into());
    }
    temporary_feed.reserve(u64::try_from(chunk.len()).map_err(|_| io::Error::other("feed chunk length does not fit in u64"))?)?;
    file.write_all(&chunk).await?;
  }
  file.flush().await?;
  drop(file);
  Ok(Some(DownloadedFeed { file: Arc::new(temporary_feed), size, previous_etag, remote_etag }))
}

async fn rotate_database_if_needed(
  d1_client: &D1Client,
  provider: &mut ProviderConfig,
  providers_file_lock: Arc<Mutex<()>>,
  database_rotation_lock: Arc<tokio::sync::Mutex<()>>,
  threshold_bytes: u64,
  max_databases: usize,
) -> Result<(), ProcessorError> {
  rotate_database_if_needed_in(d1_client, provider, "providers.toml", providers_file_lock, database_rotation_lock, threshold_bytes, max_databases).await
}

#[allow(clippy::too_many_arguments)]
async fn rotate_database_if_needed_in(
  d1_client: &D1Client,
  provider: &mut ProviderConfig,
  providers_path: &str,
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
    return Err(ProcessorError::D1(D1Error::ApiError("The daily-budget database cannot rotate automatically; preserve its ledger when moving it".into())));
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
    if let Some(schema_sqls) = SchemaCatalog::provider_migrations(&provider.name) {
      println!("[{}] Applying schemas to new database...", provider.name);
      for schema_sql in schema_sqls {
        d1_client.execute_schema(&new_uuid, schema_sql).await?;
      }
    } else {
      return Err(ProcessorError::D1(D1Error::ApiError(format!("No schema SQL found for provider {}", provider.name))));
    }

    persist_database_id(providers_path.to_owned(), provider.name.clone(), provider.database_id.clone(), database.name, new_uuid.clone(), now.to_rfc3339(), providers_file_lock).await
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
    let _guard = file_lock.lock().map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("providers.toml lock was poisoned: {error}"))))?;
    let content = std::fs::read_to_string(&path).map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to read {path}: {error}"))))?;
    let mut document = content.parse::<toml_edit::DocumentMut>().map_err(|error| ProcessorError::D1(D1Error::ApiError(format!("Failed to parse {path}: {error}"))))?;
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
    let already_recorded = retired_databases.iter().any(|database| database.get("database_id").and_then(toml_edit::Item::as_str) == Some(retired_database_id.as_str()));
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
  use crate::d1::{D1DatabaseInfo, D1Meta, D1Result, InMemoryTransport};
  use std::io::{Cursor, Write};
  use std::time::Duration;

  struct TempTestFile(PathBuf);

  impl Drop for TempTestFile {
    fn drop(&mut self) {
      let _ = std::fs::remove_file(&self.0);
    }
  }

  fn unique_temp_path(label: &str, extension: &str) -> PathBuf {
    let sequence = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("my-gtfs-worker-{label}-{}-{sequence}.{extension}", std::process::id()))
  }

  fn temp_zip_feed(label: &str, entry_name: &str, content: &[u8]) -> Result<Arc<TemporaryFeed>, Box<dyn std::error::Error>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file(entry_name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))?;
    writer.write_all(content)?;
    let archive = writer.finish()?.into_inner();
    let path = unique_temp_path(label, "zip");
    std::fs::write(&path, archive)?;
    let reserved_bytes = std::fs::metadata(&path)?.len();
    let storage = Arc::new(TempFeedStorage::new(reserved_bytes));
    storage.reserve(reserved_bytes)?;
    Ok(Arc::new(TemporaryFeed { path, storage, reserved_bytes }))
  }

  fn test_runtime_config() -> RuntimeConfig {
    RuntimeConfig {
      csv_concurrency_limit: 1,
      d1_global_concurrency_limit: 2,
      d1_database_concurrency_limit: 1,
      d1_max_databases: 10,
      query_statement_batch_size: 10,
      d1_statements_per_request: 2,
      max_d1_rows_written_per_workflow: 100,
      max_rows_per_workflow: 10,
      max_feed_download_bytes: 1024 * 1024,
      max_uncompressed_feed_bytes: 1024 * 1024,
      max_csv_record_bytes: 1024,
      max_statement_payload_bytes: 4096,
      max_temp_feed_storage_bytes: 1024 * 1024,
      db_size_threshold_bytes: 1024 * 1024,
      http_connect_timeout: Duration::from_secs(1),
      http_request_timeout: Duration::from_secs(1),
      d1_query_timeout: Duration::from_secs(1),
    }
  }

  fn test_provider(name: &str, database_id: &str) -> ProviderConfig {
    ProviderConfig { name: name.to_owned(), is_active: true, static_url: String::new(), static_provider: String::new(), database_id: database_id.to_owned() }
  }

  fn ok_write_result(rows_written: u64) -> D1Result {
    D1Result { results: Vec::new(), meta: Some(D1Meta { rows_read: 0, rows_written: Some(rows_written) }), success: Some(true) }
  }

  fn ok_rows_result(results: Vec<serde_json::Value>) -> D1Result {
    D1Result { meta: Some(D1Meta { rows_read: u64::try_from(results.len()).unwrap_or(u64::MAX), rows_written: Some(0) }), results, success: Some(true) }
  }

  #[test]
  fn positional_json_escapes_values_and_represents_missing_fields_as_null() {
    let record = csv::StringRecord::from(vec!["quoted \"value\"", "line\nbreak"]);
    let mut json = vec![b'['];
    assert!(append_positional_json_row(&mut json, &record, &[0, 1, 2]).is_ok());
    json.push(b']');
    assert_eq!(String::from_utf8(json).ok().as_deref(), Some(r#"[["quoted \"value\"","line\nbreak",null]]"#));
  }

  #[test]
  fn byte_checkpoint_resumes_at_a_quoted_record_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let csv = "id,name\n1,alpha\n2,\"two\nlines\"\n3,gamma\n4,delta\n";
    let feed = temp_zip_feed("resume-test", "stops.txt", csv.as_bytes())?;
    let header_end = read_zip_csv_headers(&mut ZipArchive::new(File::open(&feed.path)?)?, "stops.txt", 1024)?.1;
    let run_extract = |checkpoint: Checkpoint, max_rows: u64, max_payload_bytes: u64| -> Result<(CsvExtractOutcome, Vec<String>), Box<dyn std::error::Error>> {
      let (tx, mut rx) = tokio::sync::mpsc::channel(4);
      let outcome = extract_and_batch_csv(
        CsvExtractJob { feed: feed.clone(), csv_file: "stops.txt".to_owned(), csv_indices: vec![0, 1], header_end, checkpoint, batch_size: 10, max_rows, max_record_bytes: 1024, max_payload_bytes },
        Arc::new(AtomicU64::new(0)),
        tx,
      )?;
      let mut payloads = Vec::new();
      while let Ok(batch) = rx.try_recv() {
        payloads.push(batch.payload);
      }
      Ok((outcome, payloads))
    };

    let (first, first_payloads) = run_extract(Checkpoint::default(), 2, 32)?;
    assert!(!first.file_done);
    assert_eq!(first.checkpoint.line, 2);
    assert_eq!(usize::try_from(first.checkpoint.byte).ok(), csv.find("3,gamma"));
    assert_eq!(first_payloads, ["[[\"1\",\"alpha\"]]", "[[\"2\",\"two\\nlines\"]]"]);

    let (second, second_payloads) = run_extract(first.checkpoint, 10, 4096)?;
    assert!(second.file_done);
    assert_eq!(second.checkpoint, Checkpoint { line: 4, byte: u64::try_from(csv.len())? });
    assert_eq!(second_payloads.last().map(String::as_str), Some("[[\"3\",\"gamma\"],[\"4\",\"delta\"]]"));
    Ok(())
  }

  #[tokio::test]
  async fn process_csv_file_commits_checkpoint_through_in_memory_d1_seam() -> Result<(), Box<dyn std::error::Error>> {
    let feed = temp_zip_feed("session-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\n")?;
    let transport = InMemoryTransport::new();
    transport.push_query_response(vec![ok_write_result(2)]);
    transport.push_query_response(vec![ok_write_result(1)]);

    let client = D1Client::with_in_memory(transport.clone(), 100, 10, 2);
    let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 2);
    let outcome = processor.process_csv_file(feed, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;

    assert!(!outcome.file_done);
    assert!(!outcome.write_budget_exhausted);
    assert_eq!(outcome.checkpoint.line, 2);
    let recorded = transport.recorded_queries();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].sql.starts_with("INSERT INTO \"stops\""));
    assert!(recorded[1].sql.starts_with("UPDATE \"0_import_progress\""));
    Ok(())
  }

  #[tokio::test]
  async fn process_csv_file_propagates_first_batch_error_and_persists_partial_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
    let feed = temp_zip_feed("batch-err-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\nS2,Beta\nS3,Gamma\nS4,Delta\n")?;
    let transport = InMemoryTransport::new();
    transport.push_query_response(vec![ok_write_result(1)]);
    transport.push_outcome(Err(D1Error::ApiError("injected batch failure".to_owned())));
    transport.push_query_response(vec![ok_write_result(1)]);

    let runtime = Arc::new(RuntimeConfig { query_statement_batch_size: 1, d1_statements_per_request: 1, d1_database_concurrency_limit: 1, ..test_runtime_config() });
    let client = D1Client::with_in_memory(transport.clone(), 100, 10, 1);
    let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), runtime, 10);
    let error = match processor.process_csv_file(feed, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await {
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
  async fn ambiguous_retry_consumes_full_write_reservation() -> Result<(), Box<dyn std::error::Error>> {
    let feed = temp_zip_feed("ambiguous-retry-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\n")?;
    let transport = InMemoryTransport::new();
    transport.push_ambiguous_retry_response(vec![ok_write_result(0)]);
    transport.push_query_response(vec![ok_write_result(1)]);

    let client = D1Client::with_in_memory(transport, 2, 10, 1);
    let processor = ProviderProcessor::new(client.clone(), test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 1);
    let outcome = processor.process_csv_file(feed, "stops.txt", "stops", &["stop_id", "stop_name"], Checkpoint::default()).await?;
    assert!(outcome.file_done);
    assert!(matches!(client.reserve_import_writes(1).await, Err(D1Error::WriteBudgetExhausted { requested: 1, remaining: 0 })));
    Ok(())
  }

  #[tokio::test]
  async fn purge_stale_stop_times_skips_empty_csv_and_paginates_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let transport = InMemoryTransport::new();
    let client = D1Client::with_in_memory(transport.clone(), 100, 10, 1);
    let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10);

    let empty_feed = temp_zip_feed("stale-empty-test", "stop_times.txt", b"trip_id,arrival_time,departure_time,stop_id,stop_sequence\n")?;
    assert!(processor.purge_stale_stop_times_with_page_size(empty_feed, "stop_times.txt", 2).await?);
    assert!(transport.recorded_queries().is_empty());

    let feed = temp_zip_feed("stale-page-test", "stop_times.txt", b"trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,07:00:00,07:00:00,100,1\nT1,07:10:00,07:10:00,100,3\n")?;
    transport.push_query_response(vec![ok_rows_result(vec![
      serde_json::json!({"rowid": 10, "stop_id": "100", "trip_id": "T1", "stop_sequence": 1}),
      serde_json::json!({"rowid": 11, "stop_id": "100", "trip_id": "T1", "stop_sequence": 2}),
    ])]);
    transport.push_query_response(vec![ok_write_result(1)]);
    transport.push_query_response(vec![ok_rows_result(vec![serde_json::json!({"rowid": 12, "stop_id": "100", "trip_id": "T1", "stop_sequence": 3})])]);

    assert!(processor.purge_stale_stop_times_with_page_size(feed, "stop_times.txt", 2).await?);
    let recorded = transport.recorded_queries();
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[0].sql, SELECT_STOP_TIMES_KEYS_FIRST_PAGE_SQL);
    assert_eq!(recorded[0].params, vec![serde_json::json!(2)]);
    assert_eq!(recorded[1].sql, DELETE_STALE_STOP_TIMES_SQL);
    assert_eq!(recorded[1].params, vec![serde_json::json!("[11]")]);
    assert_eq!(recorded[2].sql, SELECT_STOP_TIMES_KEYS_AFTER_SQL);
    assert_eq!(recorded[2].params, vec![serde_json::json!("100"), serde_json::json!("T1"), serde_json::json!(2), serde_json::json!(2)]);
    Ok(())
  }

  #[tokio::test]
  async fn purge_stale_stop_times_write_budget_exhausted_holds_back_completion() -> Result<(), Box<dyn std::error::Error>> {
    let feed = temp_zip_feed("stale-budget-test", "stop_times.txt", b"trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,07:00:00,07:00:00,100,1\n")?;
    let transport = InMemoryTransport::new();
    transport.push_query_response(vec![ok_write_result(2)]);
    transport.push_query_response(vec![ok_rows_result(vec![serde_json::json!({"rowid": 99, "stop_id": "100", "trip_id": "T9", "stop_sequence": 1})])]);
    transport.push_query_response(vec![ok_write_result(1)]);

    let client = D1Client::with_in_memory(transport.clone(), 2, 10, 1);
    let processor = ProviderProcessor::new(client, test_provider("ktmb", "db-ktmb"), Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10);
    let outcome = processor.process_csv_file(feed, "stop_times.txt", "stop_times", &["trip_id", "arrival_time", "departure_time", "stop_id", "stop_sequence"], Checkpoint::default()).await?;

    assert!(!outcome.file_done);
    assert!(outcome.write_budget_exhausted);
    let recorded = transport.recorded_queries();
    let progress = recorded.iter().rfind(|query| query.sql.starts_with("UPDATE \"0_import_progress\"")).ok_or("missing progress update")?;
    assert_eq!(progress.params[2], serde_json::json!(1));
    Ok(())
  }

  #[test]
  fn resolve_resume_checkpoint_handles_new_changed_completed_and_partial_files() {
    let checkpoint = Checkpoint { line: 42, byte: 512 };
    assert_eq!(resolve_resume_checkpoint(None, "stops.txt", "crc-1", "ktmb"), Some(Checkpoint::default()));
    let changed = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-0".to_owned(), checkpoint, completed: true };
    assert_eq!(resolve_resume_checkpoint(Some(&changed), "stops.txt", "crc-1", "ktmb"), Some(Checkpoint::default()));
    let completed = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-1".to_owned(), checkpoint, completed: true };
    assert_eq!(resolve_resume_checkpoint(Some(&completed), "stops.txt", "crc-1", "ktmb"), None);
    let partial = StoredFileProgress { file_name: "stops.txt".to_owned(), crc: "crc-1".to_owned(), checkpoint, completed: false };
    assert_eq!(resolve_resume_checkpoint(Some(&partial), "stops.txt", "crc-1", "ktmb"), Some(checkpoint));
  }

  #[tokio::test]
  async fn rotate_database_if_needed_enforces_guards_and_cleans_up_failed_setup() -> Result<(), Box<dyn std::error::Error>> {
    let transport = InMemoryTransport::new();
    let client = D1Client::with_in_memory(transport.clone(), 100, 100, 1);
    let file_lock = Arc::new(Mutex::new(()));
    let rot_lock = Arc::new(tokio::sync::Mutex::new(()));

    transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 50 }, 1);
    let mut provider = test_provider("ktmb", "db-ktmb");
    rotate_database_if_needed_in(&client, &mut provider, "unused.toml", file_lock.clone(), rot_lock.clone(), 100, 10).await?;
    assert_eq!(provider.database_id, "db-ktmb");
    assert!(transport.deleted_databases().is_empty());

    let mut budget_client = client.clone();
    transport.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "Granted": 5_000 })])]);
    assert!(budget_client.acquire_daily_budget("db-ktmb", 5_000).await?);
    transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 200 }, 1);
    let Err(err) = rotate_database_if_needed_in(&budget_client, &mut provider, "unused.toml", file_lock.clone(), rot_lock.clone(), 100, 10).await else {
      return Err("budget database must refuse automatic rotation".into());
    };
    assert!(err.to_string().contains("daily-budget"));

    transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 200 }, 10);
    let Err(err) = rotate_database_if_needed_in(&client, &mut provider, "unused.toml", file_lock.clone(), rot_lock.clone(), 100, 10).await else {
      return Err("max database limit must block rotation".into());
    };
    assert!(err.to_string().contains("Archive and delete"));

    transport.set_database_info(D1DatabaseInfo { name: "gtfs-unknown".to_owned(), file_size: 200 }, 1);
    let mut unknown = test_provider("unknown-provider", "db-rot");
    assert!(rotate_database_if_needed_in(&client, &mut unknown, "unused.toml", file_lock.clone(), rot_lock.clone(), 100, 10).await.is_err());
    assert_eq!(unknown.database_id, "db-rot");
    assert_eq!(transport.deleted_databases(), vec!["in-memory-uuid-1"]);

    let happy_transport = InMemoryTransport::new();
    let happy_client = D1Client::with_in_memory(happy_transport.clone(), 100, 1_000, 1);
    happy_transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb-old".to_owned(), file_size: 200 }, 1);
    let path = unique_temp_path("rotate-providers-test", "toml");
    let _cleanup = TempTestFile(path.clone());
    std::fs::write(&path, "[[providers]]\nname = \"ktmb\"\ndatabase_id = \"db-old\"\n")?;
    let mut happy_provider = test_provider("ktmb", "db-old");
    rotate_database_if_needed_in(&happy_client, &mut happy_provider, &path.to_string_lossy(), file_lock, rot_lock, 100, 10).await?;
    assert_eq!(happy_provider.database_id, "in-memory-uuid-1");
    assert!(happy_transport.deleted_databases().is_empty());
    let updated = std::fs::read_to_string(&path)?;
    assert!(updated.contains("database_id = \"in-memory-uuid-1\""));
    assert!(updated.contains("database_id = \"db-old\""));
    Ok(())
  }

  #[tokio::test]
  async fn process_prepared_provider_runs_maintenance_and_records_version() -> Result<(), Box<dyn std::error::Error>> {
    let feed = temp_zip_feed("prepared-provider-test", "stops.txt", b"stop_id,stop_name\nS1,Alpha\n")?;
    let transport = InMemoryTransport::new();
    let client = D1Client::with_in_memory(transport.clone(), 100, 100, 1);
    let prepared = PreparedProvider {
      provider: test_provider("ktmb", "db-ktmb"),
      download: DownloadedFeed { size: feed.reserved_bytes, file: feed.clone(), previous_etag: String::new(), remote_etag: "etag-2".to_owned() },
      files: vec![PreparedFile { csv_file: "stops.txt".to_owned(), crc: "crc-1".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default() }],
    };
    let outcome = process_prepared_provider(&client, prepared, Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10).await?;
    assert!(outcome.remaining.is_none());
    assert_eq!(outcome.rows_processed, 1);
    assert!(!outcome.write_budget_exhausted);
    let recorded = transport.recorded_queries();
    let expected_maintenance = SchemaCatalog::maintenance_queries("ktmb")[0];
    assert!(recorded.iter().any(|query| query.sql == expected_maintenance));
    assert!(recorded.iter().any(|query| query.sql.starts_with("INSERT INTO \"0_dataset_versions\"") && query.params == vec![serde_json::json!("etag-2")]));

    let fail_transport = InMemoryTransport::new();
    fail_transport.push_outcome(Err(D1Error::ApiError("batch failed".to_owned())));
    let fail_client = D1Client::with_in_memory(fail_transport.clone(), 100, 100, 1);
    let failing = PreparedProvider {
      provider: test_provider("ktmb", "db-ktmb"),
      download: DownloadedFeed { size: feed.reserved_bytes, file: feed, previous_etag: String::new(), remote_etag: "etag-2".to_owned() },
      files: vec![PreparedFile { csv_file: "stops.txt".to_owned(), crc: "crc-1".to_owned(), table_name: "stops".to_owned(), db_columns: &["stop_id", "stop_name"], checkpoint: Checkpoint::default() }],
    };
    let error = match process_prepared_provider(&fail_client, failing, Arc::new(tokio::sync::Semaphore::new(1)), Arc::new(test_runtime_config()), 10).await {
      Ok(_) => panic!("CSV batch failure must fail process_prepared_provider"),
      Err(error) => error,
    };
    assert!(error.to_string().contains("One or more CSV files failed"));
    assert!(!fail_transport.recorded_queries().iter().any(|query| query.sql.starts_with("INSERT INTO \"0_dataset_versions\"")));
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
    let feed = temp_zip_feed("expansion-test", "stops.txt", b"id,name\n1,a-name-that-exceeds-the-test-limit\n")?;
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
    let (completion, error) = record_task_result(
      Ok(BatchTaskResult {
        ranges: vec![(Checkpoint::default(), Checkpoint { line: 100, byte: 1_000 })],
        result: Err(ProcessorError::D1(D1Error::WriteBudgetExhausted { requested: 200, remaining: 50 })),
      }),
      "test",
      "batch group",
      &mut statuses,
    );
    assert_eq!(completion, BatchCompletion::WriteBudgetExhausted);
    assert!(error.is_none());
    assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint::default());
  }

  #[tokio::test]
  async fn rotation_persists_current_and_retired_database_metadata() -> Result<(), Box<dyn std::error::Error>> {
    let path = unique_temp_path("providers-test", "toml");
    let _cleanup = TempTestFile(path.clone());
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
    Ok(())
  }

  #[test]
  fn csv_stop_time_key_set_matches_trimmed_integers_and_text_fallbacks() -> Result<(), Box<dyn std::error::Error>> {
    let mut keys = CsvStopTimeKeySet::default();
    assert!(keys.is_empty());
    keys.insert("100", "T1", "10")?;
    keys.insert("100", "T1", " 20 ")?;
    keys.insert("100", "T1", "30.0")?;
    keys.insert("200", "T2", "bad_seq")?;
    assert!(!keys.is_empty());

    let has = |stop: serde_json::Value, trip: &str, seq: serde_json::Value| keys.contains(&stop, &serde_json::json!(trip), &seq);
    assert!(has(serde_json::json!("100"), "T1", serde_json::json!(10)));
    assert!(has(serde_json::json!("100"), "T1", serde_json::json!(20)));
    assert!(has(serde_json::json!("100"), "T1", serde_json::json!(30)));
    assert!(has(serde_json::json!("200"), "T2", serde_json::json!("bad_seq")));
    assert!(!has(serde_json::json!("100"), "T1", serde_json::json!(90)));
    assert!(!has(serde_json::json!("999"), "T1", serde_json::json!(10)));
    assert!(!has(serde_json::json!(null), "T1", serde_json::json!(10)));
    Ok(())
  }

  #[test]
  fn extract_stop_time_keys_reads_archive_and_skips_missing_key_columns() -> Result<(), Box<dyn std::error::Error>> {
    let valid_feed = temp_zip_feed("stop-times-keys-test", "stop_times.txt", b"trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,07:00:00,07:00:00,100,1\n")?;
    let keys = extract_stop_time_keys(&valid_feed, "stop_times.txt", 1024)?.ok_or("expected Some(CsvStopTimeKeySet) when key columns are present")?;
    assert!(keys.contains(&serde_json::json!("100"), &serde_json::json!("T1"), &serde_json::json!(1)));
    assert!(!keys.contains(&serde_json::json!("100"), &serde_json::json!("T1"), &serde_json::json!(2)));

    let missing_feed = temp_zip_feed("stop-times-missing-col-test", "stop_times.txt", b"trip_id,stop_id\nT1,100\n")?;
    assert!(extract_stop_time_keys(&missing_feed, "stop_times.txt", 1024)?.is_none());
    Ok(())
  }

  #[test]
  fn collect_stale_stop_times_separates_stale_null_and_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let mut keys = CsvStopTimeKeySet::default();
    keys.insert("100", "T1", "1")?;
    keys.insert("100", "T1", "2")?;

    let rows = vec![
      serde_json::json!({"rowid": 10, "stop_id": null, "trip_id": "T0", "stop_sequence": 1}),
      serde_json::json!({"rowid": 11, "stop_id": "100", "trip_id": "T1", "stop_sequence": 1}),
      serde_json::json!({"rowid": 12, "stop_id": "100", "trip_id": "T1", "stop_sequence": 99}),
      serde_json::json!({"rowid": 13, "stop_id": "100", "trip_id": "T1", "stop_sequence": 2}),
      serde_json::json!({"rowid": 14, "stop_id": "200", "trip_id": "T1", "stop_sequence": null}),
    ];
    let scan = collect_stale_stop_times_from_page(rows, &keys)?;
    assert_eq!(scan.stale_rowids, vec![10, 12, 14]);
    assert_eq!(scan.last_cursor, Some([serde_json::json!("100"), serde_json::json!("T1"), serde_json::json!(2)]));
    Ok(())
  }

  #[test]
  fn serialize_delete_rowid_batches_splits_by_count_and_payload_limit() -> Result<(), Box<dyn std::error::Error>> {
    let rowids = vec![101_i64, 102, 103];
    let by_count = serialize_delete_rowid_batches(&rowids, 2, 4096)?;
    assert_eq!(by_count.len(), 2);
    assert_eq!(by_count[0].rows, 2);
    assert_eq!(by_count[0].payload, "[101,102]");
    assert_eq!(by_count[1].rows, 1);
    assert_eq!(by_count[1].payload, "[103]");

    let by_bytes = serialize_delete_rowid_batches(&rowids, 10, 8)?;
    assert_eq!(by_bytes.len(), 3);
    assert!(by_bytes.iter().all(|batch| batch.rows == 1 && batch.payload.len() <= 8));
    assert!(serialize_delete_rowid_batches(&rowids, 10, 4).is_err());
    Ok(())
  }
}
