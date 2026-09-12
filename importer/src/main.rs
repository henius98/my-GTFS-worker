mod config;
mod d1;
mod processor;

use crate::d1::D1Client;
use std::env;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

fn fair_row_limits(total: u64, consumers: usize) -> Vec<u64> {
  if consumers == 0 {
    return Vec::new();
  }
  let consumer_count = u64::try_from(consumers).unwrap_or(u64::MAX);
  let base = total / consumer_count;
  let remainder = total % consumer_count;
  (0..consumers)
    .map(|index| {
      let receives_remainder = u64::try_from(index).unwrap_or(u64::MAX) < remainder;
      base + if receives_remainder { 1 } else { 0 }
    })
    .collect()
}

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

  prepared.sort_by(|left, right| left.name().cmp(right.name()));
  let prepared_count = u64::try_from(prepared.len()).unwrap_or(u64::MAX);
  if runtime.max_rows_per_workflow < prepared_count {
    return Err(
      format!(
        "MAX_ROWS_PER_WORKFLOW={} cannot provide at least one row to each of the {} providers with pending work",
        runtime.max_rows_per_workflow,
        prepared.len()
      )
      .into(),
    );
  }

  let mut pending = prepared;
  let mut remaining_budget = runtime.max_rows_per_workflow;
  let mut total_rows_processed = 0_u64;
  let mut wave = 1_u64;
  let mut write_budget_exhausted = false;

  while !pending.is_empty() && remaining_budget != 0 {
    let row_limits = fair_row_limits(remaining_budget, pending.len());
    let allocated_this_wave = row_limits.iter().sum::<u64>();
    let mut processing_tasks = JoinSet::new();
    let mut next_pending = Vec::new();
    for (prepared_provider, max_rows) in pending.into_iter().zip(row_limits) {
      if max_rows == 0 {
        next_pending.push(prepared_provider);
        continue;
      }
      let client = d1_client.clone();
      let csv_sem = csv_semaphore.clone();
      let runtime = runtime.clone();
      processing_tasks.spawn(async move {
        let provider_name = prepared_provider.name().to_owned();
        println!("[{provider_name}] Processing wave {wave} with a fair allocation of {max_rows} rows");
        let result = processor::process_prepared_provider(&client, prepared_provider, csv_sem, runtime, max_rows).await;
        (provider_name, max_rows, result)
      });
    }

    let mut processed_this_wave = 0_u64;
    while let Some(result) = processing_tasks.join_next().await {
      match result {
        Ok((_, max_rows, Ok(outcome))) => {
          if outcome.rows_processed > max_rows {
            println!("Provider exceeded its {max_rows}-row allocation with {} rows", outcome.rows_processed);
            has_error = true;
          }
          processed_this_wave = processed_this_wave.saturating_add(outcome.rows_processed.min(max_rows));
          write_budget_exhausted |= outcome.write_budget_exhausted;
          if let Some(provider) = outcome.remaining {
            next_pending.push(provider);
          }
        }
        Ok((provider_name, _, Err(error))) => {
          println!("[{provider_name}] Error processing provider: {error}");
          has_error = true;
        }
        Err(error) => {
          println!("Provider processing task failed to join: {error}");
          has_error = true;
        }
      }
    }

    total_rows_processed = total_rows_processed.saturating_add(processed_this_wave);
    remaining_budget = runtime.max_rows_per_workflow.saturating_sub(total_rows_processed);
    next_pending.sort_by(|left, right| left.name().cmp(right.name()));
    pending = next_pending;

    if has_error {
      break;
    }
    if write_budget_exhausted {
      println!("D1 workflow write budget reached; preserving checkpoints for the next scheduled run");
      break;
    }
    if processed_this_wave >= allocated_this_wave {
      break;
    }
    if !pending.is_empty() && remaining_budget != 0 {
      println!("Redistributing {remaining_budget} unused workflow rows across {} provider(s) with pending work", pending.len());
    }
    wave += 1;
  }

  println!(
    "Workflow logical-row budget: processed {total_rows_processed} of {} rows; {} provider(s) remain incomplete; write budget exhausted: {write_budget_exhausted}",
    runtime.max_rows_per_workflow,
    pending.len()
  );

  if !has_error {
    d1_client.release_daily_budget().await?;
  }
  let usage = d1_client.usage();
  println!("D1 query metadata for this workflow: {} rows read, {} rows written", usage.rows_read, usage.rows_written);

  if has_error {
    return Err("One or more providers failed to process".into());
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fair_row_limits_are_exact_and_balanced() {
    let limits = fair_row_limits(10_000, 6);
    assert_eq!(limits.iter().sum::<u64>(), 10_000);
    assert_eq!(limits, vec![1_667, 1_667, 1_667, 1_667, 1_666, 1_666]);
    assert!(fair_row_limits(10_000, 0).is_empty());
  }
}
