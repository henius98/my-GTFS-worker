use importer::config;
use importer::d1::D1Client;
use importer::processor;
use importer::scheduler::{WaveScheduler, WaveStepOutcome};
use std::env;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let account_id = env::var("CLOUDFLARE_ACCOUNT_ID").map_err(|_| "CLOUDFLARE_ACCOUNT_ID must be set")?;
  let api_token = env::var("CLOUDFLARE_API_TOKEN").map_err(|_| "CLOUDFLARE_API_TOKEN must be set")?;

  let providers = config::load_config("providers.toml")?;
  let runtime = Arc::new(config::RuntimeConfig::from_env()?);

  let budget_database = providers
    .providers
    .iter()
    .find(|provider| provider.name == providers.budget_provider)
    .filter(|provider| !provider.database_id.is_empty())
    .ok_or("budget_provider must name a configured provider with a database_id")?;
  let mut d1_client = D1Client::new(account_id, api_token, &runtime)?;
  if !d1_client.acquire_daily_budget(&budget_database.database_id, runtime.max_d1_rows_written_per_workflow).await? {
    return Ok(());
  }
  let csv_semaphore = Arc::new(Semaphore::new(runtime.csv_concurrency_limit));
  let temp_storage = Arc::new(processor::TempFeedStorage::new(runtime.max_temp_feed_storage_bytes));
  let providers_file_lock = Arc::new(Mutex::new(()));
  let database_rotation_lock = Arc::new(tokio::sync::Mutex::new(()));
  let mut preparation_tasks = JoinSet::new();

  println!(
    "Importer limits: {} logical workflow rows, {} configured maximum D1 writes, {} CSV producers, {} global D1 requests, {} request(s) per database, {} rows per statement, {} statements per request, {} databases, {}-byte downloads, {} uncompressed bytes/feed, {} bytes/CSV record, {} bytes/statement, {} bytes temporary feed storage",
    runtime.max_rows_per_workflow,
    runtime.max_d1_rows_written_per_workflow,
    runtime.csv_concurrency_limit,
    runtime.d1_global_concurrency_limit,
    runtime.d1_database_concurrency_limit,
    runtime.query_statement_batch_size,
    runtime.d1_statements_per_request,
    runtime.d1_max_databases,
    runtime.max_feed_download_bytes,
    runtime.max_uncompressed_feed_bytes,
    runtime.max_csv_record_bytes,
    runtime.max_statement_payload_bytes,
    runtime.max_temp_feed_storage_bytes,
  );

  for provider in providers.providers {
    if !provider.is_active || provider.database_id.is_empty() {
      println!("[{}] Skipping provider: database_id is empty or is_active false", provider.name);
      continue;
    }

    let client = d1_client.clone();
    let file_lock = providers_file_lock.clone();
    let rotation_lock = database_rotation_lock.clone();
    let runtime = runtime.clone();
    let temp_storage = temp_storage.clone();
    preparation_tasks.spawn(async move {
      let provider_name = provider.name.clone();
      println!("[{provider_name}] Preparing provider");
      let result = processor::prepare_provider(&client, provider, file_lock, rotation_lock, runtime, temp_storage).await;
      (provider_name, result)
    });
  }

  let mut has_error = false;
  let mut prepared = Vec::new();
  while let Some(result) = preparation_tasks.join_next().await {
    match result {
      Ok((_, Ok(Some(provider)))) => prepared.push(provider),
      Ok((_, Ok(None))) => {}
      Ok((provider_name, Err(error))) => {
        println!("[{provider_name}] Error preparing provider: {error}");
        has_error = true;
      }
      Err(error) => {
        println!("Provider preparation task failed to join: {error}");
        has_error = true;
      }
    }
  }

  let schedule = WaveScheduler::run_waves(prepared, runtime.max_rows_per_workflow, has_error, |prepared_provider, _wave, max_rows| {
    let client = d1_client.clone();
    let csv_sem = csv_semaphore.clone();
    let runtime = runtime.clone();
    async move {
      let outcome = processor::process_prepared_provider(&client, prepared_provider, csv_sem, runtime, max_rows).await?;
      Ok::<_, processor::ProcessorError>(WaveStepOutcome::from(outcome))
    }
  })
  .await?;

  println!(
    "Workflow logical-row budget: processed {} of {} rows; {} provider(s) remain incomplete; write budget exhausted: {}",
    schedule.total_rows_processed,
    runtime.max_rows_per_workflow,
    schedule.remaining_providers.len(),
    schedule.write_budget_exhausted
  );

  if schedule.should_release_daily_budget() {
    d1_client.release_daily_budget().await?;
  }
  let usage = d1_client.usage();
  println!("D1 query metadata for this workflow: {} rows read, {} rows written", usage.rows_read, usage.rows_written);

  if schedule.has_error {
    return Err("One or more providers failed to process".into());
  }

  Ok(())
}
