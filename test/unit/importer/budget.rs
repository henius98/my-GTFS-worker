use super::*;

impl WorkflowBudget {
  pub(crate) fn metadata_remaining(&self) -> u64 {
    self.metadata.remaining()
  }
}

use chrono::TimeZone;
use std::time::Duration;

fn utc(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Result<DateTime<Utc>, String> {
  Utc.with_ymd_and_hms(year, month, day, hour, min, sec).single().ok_or_else(|| "invalid UTC timestamp".to_owned())
}

#[tokio::test]
async fn write_budget_recycles_noop_capacity_and_hard_stops() -> Result<(), D1Error> {
  let pool = Arc::new(WritePool::new(10));
  let reservation = pool.reserve(8).await?;
  assert!(!reservation.settle(3));
  assert_eq!(pool.remaining(), 7);

  let reservation = pool.reserve(7).await?;
  assert!(!reservation.settle(7));
  assert!(matches!(pool.reserve(1).await, Err(D1Error::WriteBudgetExhausted { requested: 1, remaining: 0 })));
  Ok(())
}

#[tokio::test]
async fn write_budget_wakes_waiters_after_capacity_is_released() -> Result<(), D1Error> {
  let pool = Arc::new(WritePool::new(10));
  let first = pool.reserve(8).await?;
  let waiting_pool = pool.clone();
  let waiter = tokio::spawn(async move { waiting_pool.reserve(4).await });
  tokio::task::yield_now().await;
  assert!(!first.settle(2));

  let reservation = tokio::time::timeout(Duration::from_secs(1), waiter)
    .await
    .map_err(|_| D1Error::ApiError("write-budget waiter was not notified".to_owned()))?
    .map_err(|error| D1Error::ApiError(format!("write-budget waiter failed to join: {error}")))??;
  assert!(!reservation.settle(0));
  assert_eq!(pool.remaining(), 8);
  Ok(())
}

#[tokio::test]
async fn dropped_write_reservation_fails_closed() -> Result<(), D1Error> {
  let pool = Arc::new(WritePool::new(10));
  let reservation = pool.reserve(8).await?;
  drop(reservation);

  assert_eq!(pool.remaining(), 2);
  assert!(matches!(pool.reserve(3).await, Err(D1Error::WriteBudgetExhausted { requested: 3, remaining: 2 })));
  Ok(())
}

#[tokio::test]
async fn strict_import_budget_rejects_batches_exceeding_remaining_capacity() -> Result<(), D1Error> {
  let pool = Arc::new(WritePool::new(3));
  assert!(matches!(pool.reserve(8).await, Err(D1Error::WriteBudgetExhausted { requested: 8, remaining: 3 })));
  assert_eq!(pool.remaining(), 3);

  let fitting = pool.reserve(3).await?;
  assert!(!fitting.settle(3));
  assert_eq!(pool.remaining(), 0);
  assert!(matches!(pool.reserve(1).await, Err(D1Error::WriteBudgetExhausted { requested: 1, remaining: 0 })));
  Ok(())
}

#[tokio::test]
async fn import_budget_paces_concurrent_writes_near_limit() -> Result<(), D1Error> {
  let pool = Arc::new(WritePool::new(10));
  let first = pool.reserve(8).await?;
  let waiting_pool = pool.clone();
  let waiter = tokio::spawn(async move { waiting_pool.reserve(8).await });
  tokio::task::yield_now().await;
  let _ = first.settle(0);

  let reservation = tokio::time::timeout(Duration::from_secs(1), waiter)
    .await
    .map_err(|_| D1Error::ApiError("import write-budget waiter was not notified".to_owned()))?
    .map_err(|error| D1Error::ApiError(format!("import write-budget waiter failed to join: {error}")))??;
  let _ = reservation.settle(2);
  assert_eq!(pool.remaining(), 8);
  Ok(())
}

#[tokio::test]
async fn import_reservation_dropped_fails_closed() -> Result<(), D1Error> {
  let pool = Arc::new(WritePool::new(10));
  let reservation = pool.reserve(8).await?;
  drop(reservation);

  assert_eq!(pool.remaining(), 2);
  Ok(())
}

#[tokio::test]
async fn daily_lease_enforces_2358_data_cutoff_2359_global_cutoff_and_day_rollover() -> Result<(), Box<dyn std::error::Error>> {
  let clock = FixedClock::new(utc(2026, 9, 12, 23, 57, 59)?);
  let mut budget = WorkflowBudget::new(100, 10, clock.clone());

  // Without a daily lease, 23:58 does not block in-process data writes.
  clock.set(utc(2026, 9, 12, 23, 58, 30)?);
  budget.begin_import_write(1).await?.settle(0);

  // Install a lease for 2026-09-12 and verify exact 23:58 and 23:59 boundaries.
  clock.set(utc(2026, 9, 12, 23, 57, 59)?);
  budget.install_daily_lease("db-budget".to_owned(), "2026-09-12".to_owned(), 5_000);
  assert!(budget.check_daily_window().is_ok());
  budget.begin_import_write(1).await?.settle(0);

  // At 23:58:00, data writes stop while metadata/checkpoint window stays open.
  clock.set(utc(2026, 9, 12, 23, 58, 0)?);
  assert!(matches!(budget.begin_import_write(1).await, Err(D1Error::WriteBudgetExhausted { .. })));
  assert!(budget.check_daily_window().is_ok());

  // At 23:59:00, all D1 traffic closes for this lease.
  clock.set(utc(2026, 9, 12, 23, 59, 0)?);
  assert!(matches!(budget.check_daily_window(), Err(D1Error::WriteBudgetExhausted { .. })));

  // After UTC midnight rollover, the previous day's lease stays closed.
  clock.set(utc(2026, 9, 13, 0, 0, 5)?);
  assert!(matches!(budget.check_daily_window(), Err(D1Error::WriteBudgetExhausted { .. })));
  assert!(matches!(budget.begin_import_write(1).await, Err(D1Error::WriteBudgetExhausted { .. })));
  Ok(())
}
