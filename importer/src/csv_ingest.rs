use crate::budget::WriteReservation;
use crate::config::RuntimeConfig;
use crate::d1::{Checkpoint, D1Client, D1Error, D1Query};
use crate::error::ProcessorError;
use crate::feed::TemporaryFeed;
use crate::schema::{PlannedColumn, SchemaCatalog, TableImportPlan};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::sync::{
  Arc,
  atomic::{AtomicU64, Ordering},
};
use tokio::task::{JoinError, JoinSet};
use zip::ZipArchive;

// Every imported table has at most one primary/unique index. D1 bills one row
// write for the table and up to one for that index in the worst case.
const MAX_D1_WRITES_PER_LOGICAL_ROW: u64 = 2;
// The CSV reader buffers ahead. This small allowance lets it find the end of a
// record before the exact byte-span check below rejects an oversized record.
const CSV_RECORD_READ_AHEAD_BYTES: u64 = 64 * 1024;
const JSON_BATCH_ROW_BYTES_HINT: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SerializedBatch {
  payload: String,
  start: Checkpoint,
  end: Checkpoint,
  rows: u64,
}

struct JsonBatchBuilder {
  json: Vec<u8>,
  batch_rows: usize,
  batch_start: Checkpoint,
  batch_size: usize,
  max_payload_bytes: u64,
}

impl JsonBatchBuilder {
  fn new(initial: Checkpoint, batch_size: usize, max_payload_bytes: u64) -> Self {
    let maximum_payload_capacity = usize::try_from(max_payload_bytes).unwrap_or(usize::MAX);
    let initial_capacity = batch_size.saturating_mul(JSON_BATCH_ROW_BYTES_HINT).min(256 * 1024).min(maximum_payload_capacity);
    let mut json = Vec::with_capacity(initial_capacity);
    json.push(b'[');
    Self { json, batch_rows: 0, batch_start: initial, batch_size, max_payload_bytes }
  }

  fn push_row(&mut self, start: Checkpoint, end: Checkpoint, record: &csv::StringRecord, columns: &[PlannedColumn], tx: &tokio::sync::mpsc::Sender<SerializedBatch>) -> Result<(), ProcessorError> {
    let prefix_length = self.json.len();
    let previous_batch_rows = self.batch_rows;
    if previous_batch_rows == 0 {
      self.batch_start = start;
    } else {
      self.json.push(b',');
    }
    let row_start = self.json.len();
    append_positional_json_row(&mut self.json, record, columns)?;
    self.batch_rows += 1;

    let mut serialized_bytes = u64::try_from(self.json.len().saturating_add(1)).unwrap_or(u64::MAX);
    if serialized_bytes > self.max_payload_bytes && previous_batch_rows != 0 {
      let row_payload = self.json.split_off(row_start);
      self.json.truncate(prefix_length);
      self.batch_rows = previous_batch_rows;
      let flushed = self.take_batch(self.batch_start, start)?;
      tx.blocking_send(flushed).map_err(|_| ProcessorError::UploaderClosed)?;
      self.batch_start = start;
      self.json.extend_from_slice(&row_payload);
      self.batch_rows = 1;
      serialized_bytes = u64::try_from(self.json.len().saturating_add(1)).unwrap_or(u64::MAX);
    }
    if serialized_bytes > self.max_payload_bytes {
      return Err(
        io::Error::new(io::ErrorKind::InvalidData, format!("serialized CSV row made its statement payload {serialized_bytes} bytes, exceeding the configured {}-byte limit", self.max_payload_bytes))
          .into(),
      );
    }
    if self.batch_rows >= self.batch_size || serialized_bytes == self.max_payload_bytes {
      let flushed = self.take_batch(self.batch_start, end)?;
      tx.blocking_send(flushed).map_err(|_| ProcessorError::UploaderClosed)?;
    }
    Ok(())
  }

  fn finish(&mut self, end: Checkpoint, tx: &tokio::sync::mpsc::Sender<SerializedBatch>) -> Result<(), ProcessorError> {
    if self.batch_rows != 0 {
      let flushed = self.take_batch(self.batch_start, end)?;
      tx.blocking_send(flushed).map_err(|_| ProcessorError::UploaderClosed)?;
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
  committed_ranges: Vec<(Checkpoint, Checkpoint)>,
  uncommitted_ranges: Vec<(Checkpoint, Checkpoint)>,
  result: Result<(), ProcessorError>,
}

struct UploadOutcome {
  committed_through: Checkpoint,
  had_error: bool,
  write_budget_exhausted: bool,
  first_error: Option<ProcessorError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchCompletion {
  Success,
  WriteBudgetExhausted,
  Failed,
}

#[derive(Default)]
struct UploadAccumulator {
  statuses: BTreeMap<u64, (Checkpoint, bool)>,
  had_error: bool,
  write_budget_exhausted: bool,
  first_error: Option<ProcessorError>,
}

impl UploadAccumulator {
  fn should_stop(&self) -> bool {
    self.had_error || self.write_budget_exhausted
  }

  fn record(&mut self, result: Result<BatchTaskResult, JoinError>, provider_name: &str) {
    let (completion, error) = record_task_result(result, provider_name, &mut self.statuses);
    match completion {
      BatchCompletion::Success => {}
      BatchCompletion::WriteBudgetExhausted => self.write_budget_exhausted = true,
      BatchCompletion::Failed => {
        self.had_error = true;
        if self.first_error.is_none() {
          self.first_error = error;
        }
      }
    }
  }
}

struct StatementGroupExecutor {
  d1_client: D1Client,
  database_id: Arc<str>,
  sql: Arc<str>,
  table_name: Arc<str>,
  database_semaphore: Arc<tokio::sync::Semaphore>,
  provider_name: Arc<str>,
  statements_per_request: usize,
  database_concurrency_limit: usize,
}

impl StatementGroupExecutor {
  async fn execute_reserved_group(self: Arc<Self>, batches: Vec<SerializedBatch>, logical_rows: u64, write_reservation: WriteReservation) -> BatchTaskResult {
    let queries = batches.iter().map(|batch| D1Query { sql: self.sql.as_ref(), params: vec![serde_json::Value::String(batch.payload.clone())] }).collect::<Vec<_>>();
    let permit = match self.database_semaphore.acquire().await {
      Ok(permit) => permit,
      Err(error) => {
        // The request was never dispatched, so return the unused write claim.
        let _ = write_reservation.settle(0);
        let uncommitted_ranges = batches.iter().map(|batch| (batch.start, batch.end)).collect();
        return BatchTaskResult { committed_ranges: Vec::new(), uncommitted_ranges, result: Err(ProcessorError::Invariant(format!("Failed to acquire per-database D1 permit: {error}"))) };
      }
    };
    let result = self.d1_client.execute_reserved_import_batch(write_reservation, self.database_id.as_ref(), &queries).await;
    drop(permit);
    match result {
      Ok(usage) => {
        println!("[{}] INSERT {}: {logical_rows} logical rows, {} D1 reads, {} D1 writes", self.provider_name, self.table_name, usage.rows_read, usage.rows_written);
        let committed_ranges = batches.iter().map(|batch| (batch.start, batch.end)).collect();
        BatchTaskResult { committed_ranges, uncommitted_ranges: Vec::new(), result: Ok(()) }
      }
      Err(error) => {
        let uncommitted_ranges = batches.iter().map(|batch| (batch.start, batch.end)).collect();
        BatchTaskResult { committed_ranges: Vec::new(), uncommitted_ranges, result: Err(error.into()) }
      }
    }
  }

  fn record_local_failure(&self, batches: &[SerializedBatch], error: ProcessorError, accumulator: &mut UploadAccumulator) {
    let uncommitted_ranges = batches.iter().map(|batch| (batch.start, batch.end)).collect();
    accumulator.record(Ok(BatchTaskResult { committed_ranges: Vec::new(), uncommitted_ranges, result: Err(error) }), &self.provider_name);
  }

  async fn reserve_and_spawn_pending(self: &Arc<Self>, batch_tasks: &mut JoinSet<BatchTaskResult>, pending: &mut Vec<SerializedBatch>, accumulator: &mut UploadAccumulator) {
    while !pending.is_empty() {
      while batch_tasks.len() >= self.database_concurrency_limit {
        if let Some(result) = batch_tasks.join_next().await {
          accumulator.record(result, &self.provider_name);
          if accumulator.should_stop() {
            pending.clear();
            return;
          }
        }
      }

      while let Some(result) = batch_tasks.try_join_next() {
        accumulator.record(result, &self.provider_name);
        if accumulator.should_stop() {
          pending.clear();
          return;
        }
      }

      // Reserve write budget in sequential file order before spawning concurrent HTTP tasks
      // so later/smaller batches cannot jump ahead of earlier groups and stall checkpoints at 0.
      let mut end = pending.len();
      let (write_reservation, logical_rows) = loop {
        let slice = &pending[..end];
        let logical_rows = match sum_batch_rows(slice) {
          Ok(rows) => rows,
          Err(error) => {
            self.record_local_failure(pending, error, accumulator);
            pending.clear();
            return;
          }
        };
        let Some(estimated_writes) = logical_rows.checked_mul(MAX_D1_WRITES_PER_LOGICAL_ROW) else {
          self.record_local_failure(pending, ProcessorError::Invariant("D1 batch write estimate overflowed u64".to_owned()), accumulator);
          pending.clear();
          return;
        };
        match self.d1_client.reserve_import_write(estimated_writes).await {
          Ok(reservation) => break (reservation, logical_rows),
          Err(D1Error::WriteBudgetExhausted { remaining, .. }) if slice.len() > 1 => {
            let fitted = largest_fitting_prefix(slice, remaining);
            if fitted == 0 {
              let error = ProcessorError::D1(D1Error::WriteBudgetExhausted { requested: pending[0].rows.saturating_mul(MAX_D1_WRITES_PER_LOGICAL_ROW), remaining });
              self.record_local_failure(pending, error, accumulator);
              pending.clear();
              return;
            }
            end = fitted;
          }
          Err(error) => {
            self.record_local_failure(pending, error.into(), accumulator);
            pending.clear();
            return;
          }
        }
      };

      let remainder = pending.split_off(end);
      let batches = std::mem::replace(pending, remainder);
      let worker = self.clone();
      batch_tasks.spawn(async move { worker.execute_reserved_group(batches, logical_rows, write_reservation).await });
    }
  }

  async fn execute_channel(self: Arc<Self>, initial: Checkpoint, mut receiver: tokio::sync::mpsc::Receiver<SerializedBatch>) -> UploadOutcome {
    let mut pending = Vec::with_capacity(self.statements_per_request);
    let mut batch_tasks = JoinSet::new();
    let mut accumulator = UploadAccumulator::default();

    while let Some(batch) = receiver.recv().await {
      pending.push(batch);
      if pending.len() < self.statements_per_request {
        continue;
      }

      self.reserve_and_spawn_pending(&mut batch_tasks, &mut pending, &mut accumulator).await;
      if accumulator.should_stop() {
        break;
      }
    }

    if !accumulator.should_stop() && !pending.is_empty() {
      self.reserve_and_spawn_pending(&mut batch_tasks, &mut pending, &mut accumulator).await;
    }
    drop(receiver);

    while let Some(result) = batch_tasks.join_next().await {
      accumulator.record(result, &self.provider_name);
    }

    UploadOutcome {
      committed_through: contiguous_committed_checkpoint(initial, &accumulator.statuses),
      had_error: accumulator.had_error,
      write_budget_exhausted: accumulator.write_budget_exhausted,
      first_error: accumulator.first_error,
    }
  }
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

pub(crate) struct CsvIngestRequest<'a> {
  pub(crate) d1_client: &'a D1Client,
  pub(crate) database_id: &'a str,
  pub(crate) provider_name: &'a str,
  pub(crate) feed: Arc<TemporaryFeed>,
  pub(crate) csv_file: &'a str,
  pub(crate) table_name: &'a str,
  pub(crate) staging_table: &'a str,
  pub(crate) db_columns: &'static [&'static str],
  pub(crate) checkpoint: Checkpoint,
  pub(crate) csv_semaphore: &'a Arc<tokio::sync::Semaphore>,
  pub(crate) database_semaphore: &'a Arc<tokio::sync::Semaphore>,
  pub(crate) runtime: &'a RuntimeConfig,
  pub(crate) max_rows: u64,
  pub(crate) rows_processed_this_run: &'a Arc<AtomicU64>,
}

pub(crate) struct CsvIngestOutcome {
  pub(crate) checkpoint: Checkpoint,
  pub(crate) file_done: bool,
  pub(crate) write_budget_exhausted: bool,
  pub(crate) error: Option<ProcessorError>,
}

pub(crate) async fn ingest_csv_file(request: CsvIngestRequest<'_>) -> CsvIngestOutcome {
  let checkpoint = request.checkpoint;
  let csv_permit = match request.csv_semaphore.clone().acquire_owned().await {
    Ok(permit) => permit,
    Err(error) => {
      return CsvIngestOutcome { checkpoint, file_done: false, write_budget_exhausted: false, error: Some(ProcessorError::Invariant(format!("Failed to acquire CSV producer permit: {error}"))) };
    }
  };

  let (plan_tx, plan_rx) = tokio::sync::oneshot::channel();
  let (sender, receiver) = tokio::sync::mpsc::channel(2);
  let job = CsvExtractJob {
    feed: request.feed,
    provider_name: request.provider_name.to_owned(),
    csv_file: request.csv_file.to_owned(),
    table_name: request.table_name.to_owned(),
    db_columns: request.db_columns,
    checkpoint,
    batch_size: request.runtime.query_statement_batch_size,
    max_rows: request.max_rows,
    max_record_bytes: request.runtime.max_csv_record_bytes,
    max_payload_bytes: request.runtime.max_statement_payload_bytes,
  };
  let rows_processed = request.rows_processed_this_run.clone();
  let blocking_handle = tokio::task::spawn_blocking(move || {
    let _csv_permit = csv_permit;
    extract_and_batch_csv(job, rows_processed, plan_tx, sender)
  });

  let Some(insert_sql) = plan_rx.await.ok().flatten() else {
    drop(receiver);
    return match blocking_handle.await {
      Ok(Ok(outcome)) => CsvIngestOutcome { checkpoint: outcome.checkpoint, file_done: outcome.file_done, write_budget_exhausted: false, error: None },
      Ok(Err(error)) => CsvIngestOutcome { checkpoint, file_done: false, write_budget_exhausted: false, error: Some(error) },
      Err(error) => CsvIngestOutcome { checkpoint, file_done: false, write_budget_exhausted: false, error: Some(ProcessorError::TaskJoin(error)) },
    };
  };

  let executor = Arc::new(StatementGroupExecutor {
    d1_client: request.d1_client.clone(),
    database_id: Arc::from(request.database_id),
    sql: insert_sql,
    table_name: Arc::from(request.staging_table),
    database_semaphore: request.database_semaphore.clone(),
    provider_name: Arc::from(request.provider_name),
    statements_per_request: request.runtime.d1_statements_per_request,
    database_concurrency_limit: request.runtime.d1_database_concurrency_limit,
  });
  let upload = executor.execute_channel(checkpoint, receiver).await;
  let extraction = match blocking_handle.await {
    Ok(result) => result,
    Err(error) => Err(ProcessorError::TaskJoin(error)),
  };

  if upload.had_error {
    let error = upload
      .first_error
      .or_else(|| extraction.err().filter(|error| !matches!(error, ProcessorError::UploaderClosed)))
      .unwrap_or_else(|| ProcessorError::Invariant("One or more D1 batch groups failed".to_owned()));
    return CsvIngestOutcome { checkpoint: upload.committed_through, file_done: false, write_budget_exhausted: false, error: Some(error) };
  }

  if upload.write_budget_exhausted {
    if let Err(error) = extraction
      && !matches!(error, ProcessorError::UploaderClosed)
    {
      return CsvIngestOutcome { checkpoint: upload.committed_through, file_done: false, write_budget_exhausted: false, error: Some(error) };
    }
    return CsvIngestOutcome { checkpoint: upload.committed_through, file_done: false, write_budget_exhausted: true, error: None };
  }

  let extraction = match extraction {
    Ok(extraction) => extraction,
    Err(error) => return CsvIngestOutcome { checkpoint: upload.committed_through, file_done: false, write_budget_exhausted: false, error: Some(error) },
  };

  if !producer_checkpoint_is_committed(extraction.checkpoint, upload.committed_through) {
    return CsvIngestOutcome {
      checkpoint: upload.committed_through,
      file_done: false,
      write_budget_exhausted: false,
      error: Some(ProcessorError::Invariant(format!(
        "CSV producer reached row {} at byte {}, but only row {} at byte {} was committed",
        extraction.checkpoint.line, extraction.checkpoint.byte, upload.committed_through.line, upload.committed_through.byte,
      ))),
    };
  }

  CsvIngestOutcome { checkpoint: extraction.checkpoint, file_done: extraction.file_done, write_budget_exhausted: false, error: None }
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
}

fn resolve_table_plan(job: &CsvExtractJob, headers: &csv::StringRecord, plan_tx: tokio::sync::oneshot::Sender<Option<Arc<str>>>) -> Option<TableImportPlan> {
  let Some(plan) = SchemaCatalog::plan_table_import(&job.provider_name, &job.table_name, job.db_columns, headers) else {
    println!("[{}] No matching columns in {}, skipping", job.provider_name, job.csv_file);
    let _ = plan_tx.send(None);
    return None;
  };
  let _ = plan_tx.send(Some(plan.insert_sql.clone()));
  Some(plan)
}

fn extract_and_batch_csv(
  job: CsvExtractJob,
  rows_processed_this_run: Arc<AtomicU64>,
  plan_tx: tokio::sync::oneshot::Sender<Option<Arc<str>>>,
  tx: tokio::sync::mpsc::Sender<SerializedBatch>,
) -> Result<CsvExtractOutcome, ProcessorError> {
  if rows_processed_this_run.load(Ordering::Relaxed) >= job.max_rows {
    let _ = plan_tx.send(None);
    return Ok(CsvExtractOutcome { file_done: false, checkpoint: job.checkpoint });
  }

  let mut archive = ZipArchive::new(File::open(job.feed.path())?)?;
  if job.checkpoint.byte == 0 {
    let file = archive.by_name(&job.csv_file)?;
    let uncompressed_size = file.size();
    let mut reader = csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(RecordSizeLimiter::new(file, job.max_record_bytes));
    let headers = reader.headers()?.clone();
    validate_record_span(0, reader.position().byte(), job.max_record_bytes)?;

    let Some(plan) = resolve_table_plan(&job, &headers, plan_tx) else {
      return Ok(CsvExtractOutcome { file_done: true, checkpoint: job.checkpoint });
    };

    if job.checkpoint.line != 0 {
      let mut skip_record = csv::StringRecord::new();
      for _ in 0..job.checkpoint.line {
        if !read_bounded_record(&mut reader, &mut skip_record, job.max_record_bytes)? {
          return Ok(CsvExtractOutcome { file_done: true, checkpoint: Checkpoint { line: job.checkpoint.line, byte: reader.position().byte() } });
        }
      }
    }

    return stream_csv_records(&mut reader, uncompressed_size, 0, &plan.columns, &job, &rows_processed_this_run, &tx);
  }

  let (headers, header_end) = read_zip_csv_headers(&mut archive, &job.csv_file, job.max_record_bytes)?;
  let Some(plan) = resolve_table_plan(&job, &headers, plan_tx) else {
    return Ok(CsvExtractOutcome { file_done: true, checkpoint: job.checkpoint });
  };

  let (mut reader, uncompressed_size) = resume_zip_csv_at_byte_offset(&mut archive, &job.csv_file, header_end, job.checkpoint.byte, job.max_record_bytes)?;
  stream_csv_records(&mut reader, uncompressed_size, job.checkpoint.byte, &plan.columns, &job, &rows_processed_this_run, &tx)
}

fn stream_csv_records<R: Read>(
  reader: &mut csv::Reader<RecordSizeLimiter<R>>,
  uncompressed_size: u64,
  base_byte_offset: u64,
  columns: &[PlannedColumn],
  job: &CsvExtractJob,
  rows_processed_this_run: &AtomicU64,
  tx: &tokio::sync::mpsc::Sender<SerializedBatch>,
) -> Result<CsvExtractOutcome, ProcessorError> {
  let mut local_checkpoint = Checkpoint { line: job.checkpoint.line, byte: base_byte_offset.saturating_add(reader.position().byte()) };
  let mut record = csv::StringRecord::new();
  let mut builder = JsonBatchBuilder::new(local_checkpoint, job.batch_size, job.max_payload_bytes);
  let mut file_done = true;

  loop {
    if rows_processed_this_run.load(Ordering::Relaxed) >= job.max_rows {
      file_done = local_checkpoint.byte >= uncompressed_size;
      break;
    }
    if !read_bounded_record(reader, &mut record, job.max_record_bytes)? {
      break;
    }
    if !try_claim_row(rows_processed_this_run, job.max_rows) {
      file_done = false;
      break;
    }

    let record_start = local_checkpoint;
    let record_end = Checkpoint { line: record_start.line.saturating_add(1), byte: base_byte_offset.saturating_add(reader.position().byte()) };
    builder.push_row(record_start, record_end, &record, columns, tx)?;
    local_checkpoint = record_end;
  }

  builder.finish(local_checkpoint, tx)?;
  Ok(CsvExtractOutcome { file_done, checkpoint: local_checkpoint })
}

fn append_positional_json_row(json: &mut Vec<u8>, record: &csv::StringRecord, columns: &[PlannedColumn]) -> Result<(), serde_json::Error> {
  json.push(b'[');
  for (position, column) in columns.iter().enumerate() {
    if position != 0 {
      json.push(b',');
    }
    let formatted = record.get(column.csv_index).map(|value| column.formatter.format(value));
    serde_json::to_writer(&mut *json, &formatted)?;
  }
  json.push(b']');
  Ok(())
}

fn try_claim_row(counter: &AtomicU64, maximum: u64) -> bool {
  counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| (current < maximum).then_some(current + 1)).is_ok()
}

fn classify_batch_result(result: Result<(), ProcessorError>, provider_name: &str) -> (BatchCompletion, Option<ProcessorError>) {
  match result {
    Ok(()) => (BatchCompletion::Success, None),
    Err(ProcessorError::D1(error @ D1Error::WriteBudgetExhausted { .. })) => {
      println!("[{provider_name}] {error}");
      (BatchCompletion::WriteBudgetExhausted, None)
    }
    Err(error) => {
      println!("[{provider_name}] D1 batch group failed: {error}");
      (BatchCompletion::Failed, Some(error))
    }
  }
}

fn sum_batch_rows(batches: &[SerializedBatch]) -> Result<u64, ProcessorError> {
  let overflow = || ProcessorError::Invariant("D1 batch logical-row reservation overflowed u64".to_owned());
  batches.iter().try_fold(0_u64, |total, batch| total.checked_add(batch.rows).ok_or_else(overflow))
}

fn largest_fitting_prefix(batches: &[SerializedBatch], remaining_writes: u64) -> usize {
  let mut accumulated = 0_u64;
  let mut count = 0;
  let max_prefix = batches.len().saturating_sub(1);
  for batch in batches.iter().take(max_prefix) {
    let Some(estimated) = batch.rows.checked_mul(MAX_D1_WRITES_PER_LOGICAL_ROW) else {
      break;
    };
    let Some(next_total) = accumulated.checked_add(estimated) else {
      break;
    };
    if next_total > remaining_writes {
      break;
    }
    accumulated = next_total;
    count += 1;
  }
  count
}

fn record_task_result(result: Result<BatchTaskResult, JoinError>, provider_name: &str, statuses: &mut BTreeMap<u64, (Checkpoint, bool)>) -> (BatchCompletion, Option<ProcessorError>) {
  match result {
    Ok(BatchTaskResult { committed_ranges, uncommitted_ranges, result }) => {
      for (start, end) in committed_ranges {
        statuses.insert(start.line, (end, true));
      }
      let (completion, error) = classify_batch_result(result, provider_name);
      let succeeded = completion == BatchCompletion::Success;
      for (start, end) in uncommitted_ranges {
        statuses.insert(start.line, (end, succeeded));
      }
      (completion, error)
    }
    Err(error) => {
      println!("[{provider_name}] D1 batch group task failed to join: {error}");
      (BatchCompletion::Failed, Some(ProcessorError::TaskJoin(error)))
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

#[cfg(test)]
#[path = "../../test/unit/importer/csv_ingest.rs"]
mod tests;
