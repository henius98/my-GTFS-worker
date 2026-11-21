use crate::config::{ProvidersToml, RuntimeConfig};
use crate::d1::{D1Client, D1Usage};
use crate::error::ProcessorError;
use crate::feed::{FeedFetcher, FeedStore, TempFeedStorage};
use crate::processor;
use crate::rotation::DatabaseRotator;
use crate::scheduler;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSummary {
  pub deferred: bool,
  pub total_rows_processed: u64,
  pub remaining_providers: usize,
  pub write_budget_exhausted: bool,
  pub has_error: bool,
  pub waves_executed: u64,
  pub usage: D1Usage,
}

pub async fn run_import(
  providers: ProvidersToml,
  runtime: Arc<RuntimeConfig>,
  mut d1_client: D1Client,
  feed_fetcher: Arc<dyn FeedFetcher>,
  providers_path: impl Into<PathBuf>,
) -> Result<WorkflowSummary, ProcessorError> {
  let budget_database_id = providers.budget_database()?.database_id.clone();
  if !d1_client.acquire_daily_budget(&budget_database_id, runtime.d1_daily_write_limit, runtime.d1_daily_write_limit).await? {
    return Ok(WorkflowSummary { deferred: true, total_rows_processed: 0, remaining_providers: 0, write_budget_exhausted: false, has_error: false, waves_executed: 0, usage: d1_client.usage() });
  }

  let csv_semaphore = Arc::new(Semaphore::new(runtime.csv_concurrency_limit));
  let temp_storage = Arc::new(TempFeedStorage::new(runtime.max_temp_feed_storage_bytes));
  let feed_store = FeedStore::new(feed_fetcher, temp_storage, runtime.max_feed_download_bytes, runtime.max_uncompressed_feed_bytes, runtime.max_feed_archive_entries);
  let rotator = DatabaseRotator::new(d1_client.clone(), providers_path, runtime.db_size_threshold_bytes, runtime.d1_max_databases);
  let mut preparation_tasks = JoinSet::new();

  println!(
    "Importer limits: {} logical workflow rows, {} daily D1 write limit, {} CSV producers, {} global D1 requests, {} request(s) per database, {} rows per statement, {} statements per request, {} databases, {}-byte downloads, {} uncompressed bytes/feed, {} archive entries/feed, {} bytes/CSV record, {} bytes/statement, {} bytes temporary feed storage, {} read retries (max {}s delay)",
    runtime.max_rows_per_workflow,
    runtime.d1_daily_write_limit,
    runtime.csv_concurrency_limit,
    runtime.d1_global_concurrency_limit,
    runtime.d1_database_concurrency_limit,
    runtime.query_statement_batch_size,
    runtime.d1_statements_per_request,
    runtime.d1_max_databases,
    runtime.max_feed_download_bytes,
    runtime.max_uncompressed_feed_bytes,
    runtime.max_feed_archive_entries,
    runtime.max_csv_record_bytes,
    runtime.max_statement_payload_bytes,
    runtime.max_temp_feed_storage_bytes,
    runtime.d1_max_query_retries,
    runtime.d1_max_retry_delay.as_secs(),
  );

  for provider in providers.providers {
    if !provider.is_active || provider.database_id.is_empty() {
      println!("[{}] Skipping provider: database_id is empty or is_active false", provider.name);
      continue;
    }

    let client = d1_client.clone();
    let rotator = rotator.clone();
    let feed_store = feed_store.clone();
    preparation_tasks.spawn(async move {
      let provider_name = provider.name.clone();
      println!("[{provider_name}] Preparing provider");
      let result = processor::prepare_provider(&client, &rotator, &feed_store, provider).await;
      (provider_name, result)
    });
  }

  let mut preparation_has_error = false;
  let mut prepared = Vec::new();
  while let Some(result) = preparation_tasks.join_next().await {
    match result {
      Ok((_, Ok(Some(provider)))) => prepared.push(provider),
      Ok((_, Ok(None))) => {}
      Ok((provider_name, Err(error))) => {
        println!("[{provider_name}] Error preparing provider: {error}");
        preparation_has_error = true;
      }
      Err(error) => {
        println!("Provider preparation task failed to join: {error}");
        preparation_has_error = true;
      }
    }
  }

  let schedule = scheduler::run_waves(prepared, runtime.max_rows_per_workflow, |prepared_provider, max_rows| {
    let client = d1_client.clone();
    let csv_sem = csv_semaphore.clone();
    let runtime = runtime.clone();
    async move { processor::process_prepared_provider(&client, prepared_provider, csv_sem, runtime, max_rows).await }
  })
  .await?;

  let has_error = preparation_has_error || schedule.has_error;
  println!(
    "Workflow logical-row budget: processed {} of {} rows; {} provider(s) remain incomplete; write budget exhausted: {}",
    schedule.total_rows_processed,
    runtime.max_rows_per_workflow,
    schedule.remaining_providers.len(),
    schedule.write_budget_exhausted
  );

  if !has_error {
    d1_client.release_daily_budget().await?;
  }
  let usage = d1_client.usage();
  println!("D1 query metadata for this workflow: {} rows read, {} rows written", usage.rows_read, usage.rows_written);

  Ok(WorkflowSummary {
    deferred: false,
    total_rows_processed: schedule.total_rows_processed,
    remaining_providers: schedule.remaining_providers.len(),
    write_budget_exhausted: schedule.write_budget_exhausted,
    has_error,
    waves_executed: schedule.waves_executed,
    usage,
  })
}

#[cfg(test)]
#[path = "../../test/unit/importer/workflow.rs"]
mod tests;
