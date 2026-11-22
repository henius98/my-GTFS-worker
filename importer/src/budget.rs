use crate::d1::D1Error;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::Notify;

pub const METADATA_WRITE_RESERVE: u64 = 2_000;
// One ledger update to acquire a lease and one to return unused capacity.
pub const LEDGER_WRITE_RESERVE: u64 = 2;

// Stop data writes early enough to drain requests and save checkpoints before
// the final-minute guard closes all D1 traffic for this lease.
const DATA_WINDOW_CUTOFF_SECS: i64 = 86_280;
const DAILY_WINDOW_CUTOFF_SECS: i64 = 86_340;

pub trait Clock: Send + Sync {
  fn now_utc(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
  fn now_utc(&self) -> DateTime<Utc> {
    Utc::now()
  }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub struct FixedClock {
  now: Mutex<DateTime<Utc>>,
}

impl FixedClock {
  pub fn new(now: DateTime<Utc>) -> Arc<Self> {
    Arc::new(Self { now: Mutex::new(now) })
  }

  pub fn set(&self, now: DateTime<Utc>) {
    *lock_unpoisoned(&self.now) = now;
  }
}

impl Clock for FixedClock {
  fn now_utc(&self) -> DateTime<Utc> {
    *lock_unpoisoned(&self.now)
  }
}

#[derive(Debug)]
struct WritePoolState {
  remaining: u64,
  in_flight: u64,
  in_flight_claimed: u64,
}

#[derive(Debug)]
pub(crate) struct WritePool {
  state: Mutex<WritePoolState>,
  notify: Notify,
}

impl WritePool {
  pub(crate) fn new(maximum_writes: u64) -> Self {
    Self { state: Mutex::new(WritePoolState { remaining: maximum_writes, in_flight: 0, in_flight_claimed: 0 }), notify: Notify::new() }
  }

  fn state(&self) -> MutexGuard<'_, WritePoolState> {
    lock_unpoisoned(&self.state)
  }

  pub(crate) fn remaining(&self) -> u64 {
    let state = self.state();
    state.remaining.saturating_sub(state.in_flight_claimed)
  }

  pub(crate) async fn reserve(self: &Arc<Self>, requested: u64) -> Result<WriteReservation, D1Error> {
    loop {
      let notified = self.notify.notified();
      tokio::pin!(notified);
      let _ = notified.as_mut().enable();
      {
        let mut state = self.state();
        let available = state.remaining.saturating_sub(state.in_flight_claimed);
        if requested <= available {
          state.in_flight = state.in_flight.saturating_add(1);
          state.in_flight_claimed = state.in_flight_claimed.saturating_add(requested);
          return Ok(WriteReservation { pool: self.clone(), claimed: requested, settled: false });
        }
        if state.in_flight == 0 {
          return Err(D1Error::WriteBudgetExhausted { requested, remaining: available });
        }
      }
      notified.await;
    }
  }

  fn settle(&self, claimed: u64, actual: u64) -> bool {
    let mut state = self.state();
    state.in_flight = state.in_flight.saturating_sub(1);
    state.in_flight_claimed = state.in_flight_claimed.saturating_sub(claimed);
    state.remaining = state.remaining.saturating_sub(actual);
    let exceeded_reservation = actual > claimed;
    drop(state);
    self.notify.notify_waiters();
    exceeded_reservation
  }
}

pub struct WriteReservation {
  pool: Arc<WritePool>,
  claimed: u64,
  settled: bool,
}

impl WriteReservation {
  pub(crate) fn claimed(&self) -> u64 {
    self.claimed
  }

  pub(crate) fn settle(mut self, actual: u64) -> bool {
    let exceeded_reservation = self.pool.settle(self.claimed, actual);
    self.settled = true;
    exceeded_reservation
  }
}

impl Drop for WriteReservation {
  fn drop(&mut self) {
    if !self.settled {
      // An unexpected drop can follow task cancellation or a panic after
      // the request was dispatched. Treat that outcome as ambiguous and
      // consume the reservation instead of making it spendable again.
      self.pool.settle(self.claimed, self.claimed);
      self.settled = true;
    }
  }
}

#[derive(Clone, Debug)]
pub(crate) struct DailyWriteLease {
  pub(crate) database_id: String,
  pub(crate) day: String,
}

#[derive(Clone)]
pub(crate) struct WorkflowBudget {
  data: Arc<WritePool>,
  metadata: Arc<WritePool>,
  daily_lease: Option<Arc<DailyWriteLease>>,
  clock: Arc<dyn Clock>,
}

impl WorkflowBudget {
  pub(crate) fn new(max_data_writes: u64, max_metadata_writes: u64, clock: Arc<dyn Clock>) -> Self {
    Self { data: Arc::new(WritePool::new(max_data_writes)), metadata: Arc::new(WritePool::new(max_metadata_writes)), daily_lease: None, clock }
  }

  pub(crate) fn today_utc(&self) -> String {
    self.clock.now_utc().format("%Y-%m-%d").to_string()
  }

  pub(crate) fn install_daily_lease(&mut self, database_id: String, day: String, granted: u64) {
    self.data = Arc::new(WritePool::new(granted.saturating_sub(METADATA_WRITE_RESERVE + LEDGER_WRITE_RESERVE)));
    self.metadata = Arc::new(WritePool::new(METADATA_WRITE_RESERVE));
    self.daily_lease = Some(Arc::new(DailyWriteLease { database_id, day }));
  }

  pub(crate) fn daily_lease(&self) -> Option<&DailyWriteLease> {
    self.daily_lease.as_deref()
  }

  pub(crate) fn is_budget_database(&self, database_id: &str) -> bool {
    self.daily_lease.as_ref().is_some_and(|lease| lease.database_id == database_id)
  }

  pub(crate) fn data_remaining(&self) -> u64 {
    self.data.remaining()
  }

  pub(crate) fn unused_writes(&self) -> u64 {
    self.data.remaining() + self.metadata.remaining()
  }

  pub(crate) fn check_daily_window(&self) -> Result<(), D1Error> {
    self.check_lease_cutoff(DAILY_WINDOW_CUTOFF_SECS)
  }

  fn check_lease_cutoff(&self, cutoff_secs: i64) -> Result<(), D1Error> {
    if let Some(lease) = &self.daily_lease {
      let now = self.clock.now_utc();
      if now.format("%Y-%m-%d").to_string() != lease.day || now.timestamp().rem_euclid(86_400) >= cutoff_secs {
        return Err(D1Error::WriteBudgetExhausted { requested: 1, remaining: 0 });
      }
    }
    Ok(())
  }

  pub(crate) async fn begin_import_write(&self, estimated_writes: u64) -> Result<WriteReservation, D1Error> {
    self.check_lease_cutoff(DATA_WINDOW_CUTOFF_SECS)?;
    let reservation = self.data.reserve(estimated_writes).await?;
    if let Err(error) = self.check_lease_cutoff(DATA_WINDOW_CUTOFF_SECS) {
      let _ = reservation.settle(0);
      return Err(error);
    }
    Ok(reservation)
  }

  pub(crate) async fn reserve_metadata(&self, maximum_writes: u64) -> Result<WriteReservation, D1Error> {
    self.metadata.reserve(maximum_writes).await
  }
}

#[cfg(test)]
#[path = "../../test/unit/importer/budget.rs"]
mod tests;
