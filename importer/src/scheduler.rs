use crate::error::ProcessorError;
use std::future::Future;
use tokio::task::JoinSet;

pub(crate) trait NamedWorkItem: Send + 'static {
  fn name(&self) -> &str;
}

pub(crate) struct WaveStepOutcome<P> {
  pub(crate) remaining: Option<P>,
  pub(crate) rows_processed: u64,
  pub(crate) write_budget_exhausted: bool,
}

pub(crate) struct WaveScheduleSummary<P> {
  pub(crate) total_rows_processed: u64,
  pub(crate) remaining_providers: Vec<P>,
  pub(crate) write_budget_exhausted: bool,
  pub(crate) has_error: bool,
  pub(crate) waves_executed: u64,
}

pub(crate) async fn run_waves<P, E, F, Fut>(mut prepared: Vec<P>, max_rows_per_workflow: u64, mut run_provider_wave: F) -> Result<WaveScheduleSummary<P>, ProcessorError>
where
  P: NamedWorkItem,
  E: std::fmt::Display + Send + 'static,
  F: FnMut(P, u64) -> Fut,
  Fut: Future<Output = Result<WaveStepOutcome<P>, E>> + Send + 'static,
{
  prepared.sort_by(|left, right| left.name().cmp(right.name()));
  let prepared_count = u64::try_from(prepared.len()).unwrap_or(u64::MAX);
  if max_rows_per_workflow < prepared_count {
    return Err(ProcessorError::Invariant(format!("MAX_ROWS_PER_WORKFLOW={max_rows_per_workflow} cannot provide at least one row to each of the {} providers with pending work", prepared.len())));
  }

  let mut has_error = false;
  let mut pending = prepared;
  let mut remaining_budget = max_rows_per_workflow;
  let mut total_rows_processed = 0_u64;
  let mut wave = 0_u64;
  let mut write_budget_exhausted = false;

  while !pending.is_empty() && remaining_budget != 0 {
    wave += 1;
    let row_limits = fair_row_limits(remaining_budget, pending.len());
    let allocated_this_wave = row_limits.iter().sum::<u64>();
    let mut processing_tasks = JoinSet::new();
    let mut next_pending = Vec::new();
    for (prepared_provider, max_rows) in pending.into_iter().zip(row_limits) {
      if max_rows == 0 {
        next_pending.push(prepared_provider);
        continue;
      }
      let provider_name = prepared_provider.name().to_owned();
      println!("[{provider_name}] Processing wave {wave} with a fair allocation of {max_rows} rows");
      let future = run_provider_wave(prepared_provider, max_rows);
      processing_tasks.spawn(async move {
        let result = future.await;
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
    remaining_budget = max_rows_per_workflow.saturating_sub(total_rows_processed);
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
  }

  Ok(WaveScheduleSummary { total_rows_processed, remaining_providers: pending, write_budget_exhausted, has_error, waves_executed: wave })
}

pub(crate) fn fair_row_limits(total: u64, consumers: usize) -> Vec<u64> {
  if consumers == 0 {
    return Vec::new();
  }
  let consumer_count = u64::try_from(consumers).unwrap_or(u64::MAX);
  let base = total / consumer_count;
  let remainder = total % consumer_count;
  (0..consumers).map(|index| base + u64::from(u64::try_from(index).unwrap_or(u64::MAX) < remainder)).collect()
}

#[cfg(test)]
#[path = "../../test/unit/importer/scheduler.rs"]
mod tests;
