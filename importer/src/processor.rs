use crate::config::{ProviderConfig, RuntimeConfig};
use crate::csv_ingest::{CsvIngestRequest, ingest_csv_file};
use crate::d1::{Checkpoint, D1Client, D1Error, FileImportStatus, FileProgressUpdate, OrphanStagingCleanup, StagedTableSwap, StagingTableSetup, StoredFileProgress};
use crate::error::ProcessorError;
use crate::feed::{DownloadedFeed, FeedStore, TemporaryFeed};
use crate::rotation::DatabaseRotator;
use crate::scheduler::{NamedWorkItem, WaveStepOutcome};
use crate::schema::SchemaCatalog;
use std::sync::{
  Arc,
  atomic::{AtomicU64, Ordering},
};
use tokio::task::JoinSet;

struct PreparedFile {
  csv_file: String,
  table_name: String,
  db_columns: &'static [&'static str],
  checkpoint: Checkpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StagedFile {
  csv_file: String,
  table_name: String,
}

pub(crate) struct PreparedProvider {
  provider: ProviderConfig,
  download: DownloadedFeed,
  files: Vec<PreparedFile>,
  staged_files: Vec<StagedFile>,
}

impl PreparedProvider {
  pub(crate) fn name(&self) -> &str {
    &self.provider.name
  }
}

impl NamedWorkItem for PreparedProvider {
  fn name(&self) -> &str {
    PreparedProvider::name(self)
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileResumeAction {
  SkipCompleted,
  AlreadyStaged,
  ResumeInProgress(Checkpoint),
  StartFresh,
}

fn classify_file_progress(stored: Option<&StoredFileProgress>, csv_file: &str, file_crc: &str, provider_name: &str) -> FileResumeAction {
  let Some(stored) = stored else {
    return FileResumeAction::StartFresh;
  };

  if stored.crc != file_crc {
    println!("[{}] File {} changed (CRC: {} -> {}). Staging fresh table.", provider_name, csv_file, stored.crc, file_crc);
    return FileResumeAction::StartFresh;
  }

  match stored.status {
    FileImportStatus::Completed => {
      println!("[{}] Skipping {}, already completed.", provider_name, csv_file);
      FileResumeAction::SkipCompleted
    }
    FileImportStatus::Staged => {
      println!("[{}] {} already staged; awaiting provider swap.", provider_name, csv_file);
      FileResumeAction::AlreadyStaged
    }
    FileImportStatus::InProgress => FileResumeAction::ResumeInProgress(stored.checkpoint),
  }
}

fn table_name_from_csv_file(csv_file: &str) -> Option<&str> {
  csv_file.rsplit('/').next().and_then(|base_name| base_name.strip_suffix(".txt")).filter(|table_name| !table_name.is_empty())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsvFileCompletion {
  InProgress,
  Staged,
  SkippedEmpty,
}

struct CsvFileOutcome {
  completion: CsvFileCompletion,
  checkpoint: Checkpoint,
  write_budget_exhausted: bool,
}

#[derive(Clone)]
struct ProviderProcessor {
  d1_client: D1Client,
  provider: ProviderConfig,
  csv_semaphore: Arc<tokio::sync::Semaphore>,
  database_semaphore: Arc<tokio::sync::Semaphore>,
  runtime: Arc<RuntimeConfig>,
  max_rows: u64,
  rows_processed_this_run: Arc<AtomicU64>,
}

impl ProviderProcessor {
  fn new(d1_client: D1Client, provider: ProviderConfig, csv_semaphore: Arc<tokio::sync::Semaphore>, runtime: Arc<RuntimeConfig>, max_rows: u64) -> Self {
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

  async fn process_csv_file(&self, feed: Arc<TemporaryFeed>, csv_file: &str, table_name: &str, db_columns: &'static [&'static str], checkpoint: Checkpoint) -> Result<CsvFileOutcome, ProcessorError> {
    let staging_table = SchemaCatalog::staging_table_name(table_name);
    println!("[{}] Importing {} into {} (resuming from row {}, byte {})", self.provider.name, csv_file, staging_table, checkpoint.line, checkpoint.byte);

    if self.rows_processed_this_run.load(Ordering::Relaxed) >= self.max_rows {
      return Ok(CsvFileOutcome { completion: CsvFileCompletion::InProgress, checkpoint, write_budget_exhausted: false });
    }

    let progress_reservation = match self.d1_client.reserve_checkpoint().await {
      Ok(reservation) => reservation,
      Err(D1Error::WriteBudgetExhausted { .. }) => {
        return Ok(CsvFileOutcome { completion: CsvFileCompletion::InProgress, checkpoint, write_budget_exhausted: true });
      }
      Err(error) => return Err(error.into()),
    };

    let outcome = ingest_csv_file(CsvIngestRequest {
      d1_client: &self.d1_client,
      database_id: &self.provider.database_id,
      provider_name: &self.provider.name,
      feed,
      csv_file,
      table_name,
      staging_table: &staging_table,
      db_columns,
      checkpoint,
      csv_semaphore: &self.csv_semaphore,
      database_semaphore: &self.database_semaphore,
      runtime: &self.runtime,
      max_rows: self.max_rows,
      rows_processed_this_run: &self.rows_processed_this_run,
    })
    .await;

    if outcome.file_done && outcome.checkpoint.line == 0 {
      println!("[{}] Warning: {} contains 0 data rows; skipping table swap and preserving existing {}", self.provider.name, csv_file, table_name);
      self.d1_client.skip_empty_staged_file(progress_reservation, &self.provider.database_id, csv_file, table_name, outcome.checkpoint).await?;
      if let Some(error) = outcome.error {
        return Err(error);
      }
      return Ok(CsvFileOutcome { completion: CsvFileCompletion::SkippedEmpty, checkpoint: outcome.checkpoint, write_budget_exhausted: outcome.write_budget_exhausted });
    }

    let status = if outcome.file_done { FileImportStatus::Staged } else { FileImportStatus::InProgress };
    let update = FileProgressUpdate { file: csv_file, checkpoint: outcome.checkpoint, status };
    self.d1_client.save_file_progress(progress_reservation, &self.provider.database_id, update).await?;
    if let Some(error) = outcome.error {
      return Err(error);
    }
    Ok(CsvFileOutcome {
      completion: if outcome.file_done { CsvFileCompletion::Staged } else { CsvFileCompletion::InProgress },
      checkpoint: outcome.checkpoint,
      write_budget_exhausted: outcome.write_budget_exhausted,
    })
  }
}

type FeedDownloadResult = (DownloadedFeed, Option<Vec<StoredFileProgress>>);

async fn download_provider_feed(d1_client: &D1Client, feed_store: &FeedStore, provider: &ProviderConfig) -> Result<Option<FeedDownloadResult>, ProcessorError> {
  let target_url = format!("{}{}", provider.static_url, provider.static_provider);
  let previous_etag = d1_client.get_dataset_version(&provider.database_id).await?.unwrap_or_default();

  println!("[{}] Fetching GTFS zip from {}", provider.name, target_url);
  let (download, cached_progress) = match feed_store.download(&target_url, (!previous_etag.is_empty()).then_some(previous_etag.as_str())).await? {
    Some(download) => (download, None),
    None => {
      let progress_rows = d1_client.get_files_progress(&provider.database_id).await?;
      let total = progress_rows.len();
      let incomplete = progress_rows.iter().filter(|row| row.status != FileImportStatus::Completed).count();
      if total != 0 && incomplete == 0 {
        println!("[{}] Feed unchanged and all {} files are complete. Skipping.", provider.name, total);
        return Ok(None);
      }
      println!("[{}] Feed unchanged, but progress is missing or {} files are incomplete. Resuming.", provider.name, incomplete);
      let download = feed_store.download(&target_url, None).await?.ok_or_else(|| ProcessorError::Invariant("unconditional feed fetch unexpectedly returned 304 Not Modified".to_owned()))?;
      (download, Some(progress_rows))
    }
  };

  if download.remote_etag.is_empty() {
    println!("[{}] Feed supplied no ETag; file CRCs will determine whether work is needed.", provider.name);
  } else if download.remote_etag != previous_etag {
    println!("[{}] Feed ETag changed ({} -> {}).", provider.name, previous_etag, download.remote_etag);
  }
  println!("[{}] Downloaded {} bytes to temporary storage", provider.name, download.size);
  Ok(Some((download, cached_progress)))
}

pub(crate) async fn prepare_provider(d1_client: &D1Client, rotator: &DatabaseRotator, feed_store: &FeedStore, mut provider: ProviderConfig) -> Result<Option<PreparedProvider>, ProcessorError> {
  rotator.rotate_if_needed(&mut provider).await?;

  let Some((download, cached_progress)) = download_provider_feed(d1_client, feed_store, &provider).await? else {
    return Ok(None);
  };

  let schemas = SchemaCatalog::provider_schemas(&provider.name).unwrap_or(&[]);
  let discovered = feed_store.discover_supported_files(download.file.clone(), provider.name.clone(), schemas).await?;

  let progress_rows = match cached_progress {
    Some(rows) => rows,
    None => d1_client.get_files_progress(&provider.database_id).await?,
  };
  let mut orphan_entries = Vec::new();
  for row in &progress_rows {
    if row.status == FileImportStatus::Completed {
      continue;
    }
    if discovered.iter().any(|file| file.csv_file == row.file_name) {
      continue;
    }
    if let Some(table_name) = table_name_from_csv_file(&row.file_name)
      && SchemaCatalog::staging_create_sql(&provider.name, table_name).is_some()
    {
      orphan_entries.push((row.file_name.as_str(), table_name));
    }
  }
  orphan_entries.sort();

  let mut files = Vec::with_capacity(discovered.len());
  let mut staged_files = Vec::new();
  let mut fresh_setups: Vec<(String, String, String, &'static str)> = Vec::new();
  for file in discovered {
    let stored = progress_rows.iter().find(|row| row.file_name == file.csv_file);
    match classify_file_progress(stored, &file.csv_file, &file.crc, &provider.name) {
      FileResumeAction::SkipCompleted => {}
      FileResumeAction::AlreadyStaged => {
        staged_files.push(StagedFile { csv_file: file.csv_file, table_name: file.table_name });
      }
      FileResumeAction::ResumeInProgress(checkpoint) => {
        files.push(PreparedFile { csv_file: file.csv_file, table_name: file.table_name, db_columns: file.db_columns, checkpoint });
      }
      FileResumeAction::StartFresh => {
        let create_sql = SchemaCatalog::staging_create_sql(&provider.name, &file.table_name)
          .ok_or_else(|| ProcessorError::Invariant(format!("Missing staging CREATE TABLE DDL for {}.{}", provider.name, file.table_name)))?;
        fresh_setups.push((file.csv_file.clone(), file.crc, file.table_name.clone(), create_sql));
        files.push(PreparedFile { csv_file: file.csv_file, table_name: file.table_name, db_columns: file.db_columns, checkpoint: Checkpoint::default() });
      }
    }
  }
  files.sort_by(|left, right| left.csv_file.cmp(&right.csv_file));
  staged_files.sort_by(|left, right| left.csv_file.cmp(&right.csv_file));
  fresh_setups.sort_by(|left, right| left.0.cmp(&right.0));

  if !orphan_entries.is_empty() || !fresh_setups.is_empty() {
    let orphans = orphan_entries.iter().map(|&(file, table_name)| OrphanStagingCleanup { file, table_name }).collect::<Vec<_>>();
    let setups = fresh_setups.iter().map(|(file, crc, table_name, create_sql)| StagingTableSetup { file, crc, table_name, create_sql }).collect::<Vec<_>>();
    d1_client.prepare_staging(&provider.database_id, &orphans, &setups).await?;
  }

  if files.is_empty() && staged_files.is_empty() {
    if !download.remote_etag.is_empty() {
      d1_client.set_dataset_version(&provider.database_id, &download.remote_etag).await?;
    }
    println!("[{}] No files require import.", provider.name);
    return Ok(None);
  }

  Ok(Some(PreparedProvider { provider, download, files, staged_files }))
}

pub(crate) async fn process_prepared_provider(
  d1_client: &D1Client,
  prepared: PreparedProvider,
  csv_semaphore: Arc<tokio::sync::Semaphore>,
  runtime: Arc<RuntimeConfig>,
  max_rows: u64,
) -> Result<WaveStepOutcome<PreparedProvider>, ProcessorError> {
  if max_rows == 0 {
    return Err(ProcessorError::Invariant("Provider row allocation must be greater than zero".to_owned()));
  }

  let PreparedProvider { provider, download, files, mut staged_files } = prepared;
  let processor = Arc::new(ProviderProcessor::new(d1_client.clone(), provider.clone(), csv_semaphore, runtime, max_rows));
  let mut csv_tasks = JoinSet::new();

  for mut file in files {
    let processor = processor.clone();
    let feed = download.file.clone();
    let initial_line = file.checkpoint.line;
    csv_tasks.spawn(async move {
      match processor.process_csv_file(feed, &file.csv_file, &file.table_name, file.db_columns, file.checkpoint).await {
        Ok(outcome) => {
          let committed_rows = outcome.checkpoint.line.saturating_sub(initial_line);
          file.checkpoint = outcome.checkpoint;
          Ok((file, outcome.completion, outcome.write_budget_exhausted, committed_rows))
        }
        Err(error) => Err((file.csv_file, error)),
      }
    });
  }

  let mut has_csv_error = false;
  let mut write_budget_exhausted = false;
  let mut rows_processed = 0_u64;
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
      Ok(Ok((file, completion, file_write_budget_exhausted, committed_rows))) => {
        rows_processed = rows_processed.saturating_add(committed_rows);
        write_budget_exhausted |= file_write_budget_exhausted;
        match completion {
          CsvFileCompletion::InProgress => remaining_files.push(file),
          CsvFileCompletion::Staged => staged_files.push(StagedFile { csv_file: file.csv_file, table_name: file.table_name }),
          CsvFileCompletion::SkippedEmpty => {}
        }
      }
    }
  }
  if has_csv_error {
    return Err(ProcessorError::Invariant("One or more CSV files failed".to_owned()));
  }

  remaining_files.sort_by(|left, right| left.csv_file.cmp(&right.csv_file));
  staged_files.sort_by(|left, right| left.csv_file.cmp(&right.csv_file));
  if remaining_files.is_empty() {
    let swaps = staged_files.iter().map(|file| StagedTableSwap { file: &file.csv_file, table_name: &file.table_name }).collect::<Vec<_>>();
    if !swaps.is_empty() {
      println!("[{}] All files staged. Committing atomic swap for {} table(s)...", provider.name, swaps.len());
    }
    let remote_etag = (!download.remote_etag.is_empty()).then_some(download.remote_etag.as_str());
    match d1_client.commit_provider_swap(&provider.database_id, &swaps, remote_etag).await {
      Ok(()) => {
        staged_files.clear();
      }
      Err(error @ D1Error::WriteBudgetExhausted { .. }) => {
        println!("[{}] Deferred atomic table swap due to write budget: {error}", provider.name);
        write_budget_exhausted = true;
      }
      Err(error) => return Err(error.into()),
    }
  }

  println!("[{}] Completed run. Processed {} logical rows from a {}-row allocation.", provider.name, rows_processed, max_rows);
  let remaining = (!remaining_files.is_empty() || !staged_files.is_empty()).then_some(PreparedProvider { provider, download, files: remaining_files, staged_files });
  Ok(WaveStepOutcome { remaining, rows_processed, write_budget_exhausted })
}

#[cfg(test)]
#[path = "../../test/unit/importer/processor.rs"]
mod tests;
