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
async fn redistributes_unused_rows_across_waves_until_budget_or_work_completes() -> Result<(), ProcessorError> {
  let providers =
    vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 20, exhaust_write_budget: false }, FakeProvider { name: "beta".to_owned(), rows_remaining: 80, exhaust_write_budget: false }];
  let shutdown = ShutdownSignal::new();
  let summary = run_waves(
    providers,
    100,
    &shutdown,
    || None,
    |mut provider, max_rows| async move {
      let consumed = provider.rows_remaining.min(max_rows);
      provider.rows_remaining -= consumed;
      let remaining = (provider.rows_remaining != 0).then_some(provider);
      Ok::<_, String>(WaveStepOutcome { remaining, rows_processed: consumed, write_budget_exhausted: false, shutdown_requested: false })
    },
  )
  .await?;

  assert_eq!(summary.waves_executed, 2);
  assert_eq!(summary.total_rows_processed, 100);
  assert!(summary.remaining_providers.is_empty());
  assert!(!summary.shutdown_requested);
  assert!(!summary.has_error);
  Ok(())
}

#[tokio::test]
async fn stops_on_write_budget_exhaustion_without_flagging_error() -> Result<(), ProcessorError> {
  let providers =
    vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 100, exhaust_write_budget: true }, FakeProvider { name: "beta".to_owned(), rows_remaining: 100, exhaust_write_budget: false }];
  let shutdown = ShutdownSignal::new();
  let summary = run_waves(
    providers,
    100,
    &shutdown,
    || None,
    |mut provider, max_rows| async move {
      let consumed = (max_rows / 2).max(1);
      provider.rows_remaining = provider.rows_remaining.saturating_sub(consumed);
      let exhaust = provider.exhaust_write_budget;
      Ok::<_, String>(WaveStepOutcome { remaining: Some(provider), rows_processed: consumed, write_budget_exhausted: exhaust, shutdown_requested: false })
    },
  )
  .await?;

  assert_eq!(summary.waves_executed, 1);
  assert!(summary.write_budget_exhausted);
  assert!(!summary.shutdown_requested);
  assert_eq!(summary.remaining_providers.len(), 2);
  assert!(!summary.has_error);
  Ok(())
}

#[tokio::test]
async fn stops_between_waves_when_shutdown_signal_triggers() -> Result<(), ProcessorError> {
  let providers =
    vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 10, exhaust_write_budget: false }, FakeProvider { name: "beta".to_owned(), rows_remaining: 90, exhaust_write_budget: false }];
  let shutdown = ShutdownSignal::new();
  let wave_shutdown = shutdown.clone();
  let summary = run_waves(
    providers,
    100,
    &shutdown,
    || None,
    move |mut provider, max_rows| {
      let signal = wave_shutdown.clone();
      async move {
        let consumed = provider.rows_remaining.min(max_rows);
        provider.rows_remaining -= consumed;
        signal.trigger();
        let remaining = (provider.rows_remaining != 0).then_some(provider);
        Ok::<_, String>(WaveStepOutcome { remaining, rows_processed: consumed, write_budget_exhausted: false, shutdown_requested: true })
      }
    },
  )
  .await?;

  assert_eq!(summary.waves_executed, 1);
  assert!(summary.shutdown_requested);
  assert_eq!(summary.remaining_providers.len(), 1);
  assert!(!summary.has_error);
  Ok(())
}

#[tokio::test]
async fn over_allocation_flags_error() -> Result<(), ProcessorError> {
  let providers = vec![FakeProvider { name: "alpha".to_owned(), rows_remaining: 10, exhaust_write_budget: false }];
  let shutdown = ShutdownSignal::new();
  let summary = run_waves(
    providers,
    10,
    &shutdown,
    || None,
    |_provider, max_rows| async move { Ok::<_, String>(WaveStepOutcome { remaining: None, rows_processed: max_rows + 1, write_budget_exhausted: false, shutdown_requested: false }) },
  )
  .await?;

  assert!(summary.has_error);
  Ok(())
}

#[tokio::test]
async fn caps_wave_allocations_by_remaining_write_budget_and_prevents_provider_starvation() -> Result<(), ProcessorError> {
  use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
  };

  let providers = vec![
    FakeProvider { name: "alpha".to_owned(), rows_remaining: 500, exhaust_write_budget: false },
    FakeProvider { name: "beta".to_owned(), rows_remaining: 500, exhaust_write_budget: false },
    FakeProvider { name: "gamma".to_owned(), rows_remaining: 500, exhaust_write_budget: false },
    FakeProvider { name: "delta".to_owned(), rows_remaining: 500, exhaust_write_budget: false },
  ];
  let shutdown = ShutdownSignal::new();
  let remaining_writes = Arc::new(AtomicU64::new(100));
  let allocated = Arc::new(Mutex::new(Vec::new()));
  let writes_for_cap = remaining_writes.clone();
  let writes_for_wave = remaining_writes.clone();
  let allocated_for_wave = allocated.clone();

  let summary = run_waves(
    providers,
    5_000_000,
    &shutdown,
    move || Some(writes_for_cap.load(Ordering::Relaxed)),
    move |mut provider, max_rows| {
      let writes = writes_for_wave.clone();
      let allocated = allocated_for_wave.clone();
      async move {
        if let Ok(mut guard) = allocated.lock() {
          guard.push((provider.name.clone(), max_rows));
        }
        let consumed = provider.rows_remaining.min(max_rows);
        provider.rows_remaining -= consumed;
        writes.fetch_sub(consumed, Ordering::Relaxed);
        Ok::<_, String>(WaveStepOutcome { remaining: Some(provider), rows_processed: consumed, write_budget_exhausted: false, shutdown_requested: false })
      }
    },
  )
  .await?;

  assert_eq!(summary.waves_executed, 1);
  assert_eq!(summary.total_rows_processed, 100);
  assert!(summary.write_budget_exhausted);
  assert_eq!(summary.remaining_providers.len(), 4);
  assert!(!summary.has_error);
  let mut got = allocated.lock().map(|guard| guard.clone()).unwrap_or_default();
  got.sort();
  assert_eq!(got, vec![("alpha".to_owned(), 25), ("beta".to_owned(), 25), ("delta".to_owned(), 25), ("gamma".to_owned(), 25)]);
  Ok(())
}
