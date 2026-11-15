use crate::processor::{PreparedProvider, ProviderRunOutcome};
use std::future::Future;
use tokio::task::JoinSet;

pub trait NamedWorkItem: Send + 'static {
  fn name(&self) -> &str;
}

impl NamedWorkItem for PreparedProvider {
  fn name(&self) -> &str {
    PreparedProvider::name(self)
  }
}

pub struct WaveStepOutcome<P> {
  pub remaining: Option<P>,
  pub rows_processed: u64,
  pub write_budget_exhausted: bool,
}

impl From<ProviderRunOutcome> for WaveStepOutcome<PreparedProvider> {
  fn from(outcome: ProviderRunOutcome) -> Self {
    Self { remaining: outcome.remaining, rows_processed: outcome.rows_processed, write_budget_exhausted: outcome.write_budget_exhausted }
  }
}

pub struct WaveScheduleSummary<P> {
  pub total_rows_processed: u64,
  pub remaining_providers: Vec<P>,
  pub write_budget_exhausted: bool,
  pub has_error: bool,
  pub waves_executed: u64,
}

impl<P> WaveScheduleSummary<P> {
  pub fn should_release_daily_budget(&self) -> bool {
    !self.has_error
  }
}

pub struct WaveScheduler;

impl WaveScheduler {
  pub async fn run_waves<P, E, F, Fut>(mut prepared: Vec<P>, max_rows_per_workflow: u64, initial_has_error: bool, mut run_provider_wave: F) -> Result<WaveScheduleSummary<P>, String>
  where
    P: NamedWorkItem,
    E: std::fmt::Display + Send + 'static,
    F: FnMut(P, u64, u64) -> Fut,
    Fut: Future<Output = Result<WaveStepOutcome<P>, E>> + Send + 'static,
  {
    prepared.sort_by(|left, right| left.name().cmp(right.name()));
    let prepared_count = u64::try_from(prepared.len()).unwrap_or(u64::MAX);
    if max_rows_per_workflow < prepared_count {
      return Err(format!("MAX_ROWS_PER_WORKFLOW={max_rows_per_workflow} cannot provide at least one row to each of the {} providers with pending work", prepared.len()));
    }

    let mut has_error = initial_has_error;
    let mut pending = prepared;
    let mut remaining_budget = max_rows_per_workflow;
    let mut total_rows_processed = 0_u64;
    let mut wave = 0_u64;
    let mut write_budget_exhausted = false;

    while !pending.is_empty() && remaining_budget != 0 {
      wave += 1;
      let current_wave = wave;
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
        println!("[{provider_name}] Processing wave {current_wave} with a fair allocation of {max_rows} rows");
        let future = run_provider_wave(prepared_provider, current_wave, max_rows);
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
}

pub fn fair_row_limits(total: u64, consumers: usize) -> Vec<u64> {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Debug)]
  struct FakeProvider {
    name: String,
    rows_remaining: u64,
    exhaust_write_budget: bool,
  }

  impl NamedWorkItem for FakeProvider {
    fn name(&self) -> &str {
      &self.name
    }
  }

  #[test]
  fn fair_row_limits_are_exact_and_balanced() {
    let limits = fair_row_limits(10_000, 6);
    assert_eq!(limits.iter().sum::<u64>(), 10_000);
    assert_eq!(limits, vec![1_667, 1_667, 1_667, 1_667, 1_666, 1_666]);
    assert!(fair_row_limits(10_000, 0).is_empty());
  }

  #[tokio::test]
  async fn redistributes_unused_rows_across_waves_until_budget_or_work_completes() -> Result<(), String> {
    let providers =
      vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 20, exhaust_write_budget: false }, FakeProvider { name: "beta".to_owned(), rows_remaining: 80, exhaust_write_budget: false }];
    let summary = WaveScheduler::run_waves(providers, 100, false, |mut provider, _wave, max_rows| async move {
      let consumed = provider.rows_remaining.min(max_rows);
      provider.rows_remaining -= consumed;
      let remaining = (provider.rows_remaining != 0).then_some(provider);
      Ok::<_, String>(WaveStepOutcome { remaining, rows_processed: consumed, write_budget_exhausted: false })
    })
    .await?;

    assert_eq!(summary.waves_executed, 2);
    assert_eq!(summary.total_rows_processed, 100);
    assert!(summary.remaining_providers.is_empty());
    assert!(summary.should_release_daily_budget());
    Ok(())
  }

  #[tokio::test]
  async fn stops_on_write_budget_exhaustion_and_still_releases_daily_lease() -> Result<(), String> {
    let providers =
      vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 100, exhaust_write_budget: true }, FakeProvider { name: "beta".to_owned(), rows_remaining: 100, exhaust_write_budget: false }];
    let summary = WaveScheduler::run_waves(providers, 100, false, |mut provider, _wave, max_rows| async move {
      let consumed = (max_rows / 2).max(1);
      provider.rows_remaining = provider.rows_remaining.saturating_sub(consumed);
      let exhaust = provider.exhaust_write_budget;
      Ok::<_, String>(WaveStepOutcome { remaining: Some(provider), rows_processed: consumed, write_budget_exhausted: exhaust })
    })
    .await?;

    assert_eq!(summary.waves_executed, 1);
    assert!(summary.write_budget_exhausted);
    assert_eq!(summary.remaining_providers.len(), 2);
    assert!(summary.should_release_daily_budget());
    Ok(())
  }

  #[tokio::test]
  async fn over_allocation_flags_error_and_withholds_daily_lease_release() -> Result<(), String> {
    let providers = vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 10, exhaust_write_budget: false }];
    let summary = WaveScheduler::run_waves(providers, 10, false, |_provider, _wave, max_rows| async move {
      Ok::<_, String>(WaveStepOutcome { remaining: None, rows_processed: max_rows + 1, write_budget_exhausted: false })
    })
    .await?;

    assert!(summary.has_error);
    assert!(!summary.should_release_daily_budget());
    Ok(())
  }
}
