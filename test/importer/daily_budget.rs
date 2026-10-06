//! Exercise the production ledger SQL locally; no Cloudflare requests.

use gtfs_checks::{CheckResult, root};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params};
use std::{
  fs,
  path::Path,
  sync::{Arc, Mutex},
  thread,
};

fn source() -> CheckResult<String> {
  Ok(fs::read_to_string(root().join("importer/src/d1.rs"))?)
}

fn sql_after(source: &str, prefix: &str) -> CheckResult<String> {
  let marker = format!("sql: \"{prefix}");
  let rest = source.split_once(&marker).ok_or_else(|| format!("missing SQL prefix: {prefix}"))?.1;
  Ok(format!("{prefix}{}", rest.split('"').next().ok_or("unterminated SQL")?))
}

fn number(source: &str, name: &str) -> CheckResult<i64> {
  let line = source.lines().find(|line| line.contains(&format!("const {name}:")) || line.contains(&format!("pub const {name}:"))).ok_or_else(|| format!("missing {name}"))?;
  Ok(line.split_once('=').ok_or("missing assignment")?.1.trim().trim_end_matches(';').replace('_', "").parse()?)
}

struct Budget {
  _directory: tempfile::TempDir,
  db: Connection,
  path: std::path::PathBuf,
  day: Arc<Mutex<String>>,
  acquire_sql: String,
  release_sql: String,
  migration: String,
  limit: i64,
  overhead: i64,
}

impl Budget {
  fn new() -> CheckResult<Self> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("budget.db");
    let day = Arc::new(Mutex::new("2026-09-12".to_owned()));
    let db = Self::connect(&path, &day)?;
    let source = source()?;
    let migration = fs::read_to_string(root().join("migrations/ktmb/20260912_add_daily_import_budget.sql"))?;
    db.execute_batch(&migration)?;
    Ok(Self {
      _directory: directory,
      db,
      path,
      day,
      acquire_sql: sql_after(&source, "WITH allowance")?,
      release_sql: sql_after(&source, "UPDATE daily_import_budget")?,
      migration,
      limit: number(&source, "DAILY_WRITE_LIMIT")?,
      overhead: number(&source, "METADATA_WRITE_RESERVE")? + number(&source, "LEDGER_WRITE_RESERVE")?,
    })
  }

  fn connect(path: &Path, day: &Arc<Mutex<String>>) -> CheckResult<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let day = Arc::clone(day);
    db.create_scalar_function("date", 1, FunctionFlags::SQLITE_UTF8, move |_| day.lock().map(|value| value.clone()).map_err(|_| rusqlite::Error::InvalidQuery))?;
    Ok(db)
  }

  fn acquire_on(&self, db: &Connection, amount: i64, day: &str) -> CheckResult<Option<i64>> {
    Ok(db.query_row(&self.acquire_sql, params![amount, self.limit, day, self.overhead], |row| row.get(0)).optional()?)
  }

  fn acquire(&self, amount: i64) -> CheckResult<Option<i64>> {
    self.acquire_on(&self.db, amount, &self.today()?)
  }

  fn release(&self, amount: i64, day: &str) -> CheckResult {
    self.db.execute(&self.release_sql, params![amount, day, amount])?;
    Ok(())
  }

  fn today(&self) -> CheckResult<String> {
    Ok(self.day.lock().map_err(|_| "poisoned day lock")?.clone())
  }

  fn set_day(&self, day: &str) -> CheckResult {
    *self.day.lock().map_err(|_| "poisoned day lock")? = day.to_owned();
    Ok(())
  }

  fn reserved(&self) -> CheckResult<i64> {
    Ok(self.db.query_row("SELECT Reserved FROM daily_import_budget WHERE Id = 1", [], |row| row.get(0))?)
  }
}

#[test]
fn crashed_runs_keep_capacity_and_third_run_is_denied() -> CheckResult {
  let budget = Budget::new()?;
  assert!(budget.acquire(budget.limit / 2)?.is_some());
  assert!(budget.acquire(budget.limit / 2)?.is_some());
  assert_eq!(budget.acquire(budget.limit / 2)?, None);
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn returning_unused_capacity_preserves_consumed_writes() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit / 2)?;
  let unused = budget.limit / 2 - 10_000;
  budget.release(unused, &budget.today()?)?;
  assert_eq!(budget.reserved()?, 10_000);
  assert!(budget.acquire(budget.limit / 2)?.is_some());
  assert_eq!(budget.acquire(budget.limit / 2)?, Some(budget.limit / 2 - 10_000));
  assert_eq!(budget.acquire(budget.limit / 2)?, None);
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn full_allowance_and_later_run_use_remaining_capacity() -> CheckResult {
  let budget = Budget::new()?;
  assert_eq!(budget.limit, 100_000);
  assert_eq!(budget.acquire(budget.limit)?, Some(budget.limit));
  assert_eq!(budget.acquire(budget.limit)?, None);
  let unused = budget.limit - 10_000;
  budget.release(unused, &budget.today()?)?;
  assert_eq!(budget.acquire(budget.limit)?, Some(unused));
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn remaining_capacity_must_cover_metadata_and_ledger() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit)?;
  budget.release(budget.overhead, &budget.today()?)?;
  assert_eq!(budget.acquire(budget.limit)?, None);
  assert_eq!(budget.reserved()?, budget.limit - budget.overhead);
  Ok(())
}

#[test]
fn stale_day_cannot_acquire_a_new_lease() -> CheckResult {
  let budget = Budget::new()?;
  let old_day = budget.today()?;
  budget.set_day("2026-09-13")?;
  assert_eq!(budget.acquire_on(&budget.db, budget.limit, &old_day)?, None);
  assert_eq!(budget.reserved()?, 0);
  Ok(())
}

#[test]
fn next_day_resets_and_old_release_cannot_refund_new_day() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit / 2)?;
  let old_day = budget.today()?;
  budget.set_day("2026-09-13")?;
  assert_eq!(budget.acquire(budget.limit)?, Some(budget.limit));
  budget.release(30_000, &old_day)?;
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn concurrent_processes_share_one_allowance() -> CheckResult {
  let budget = Budget::new()?;
  let grants = thread::scope(|scope| {
    let workers = (0..4)
      .map(|_| {
        let path = budget.path.clone();
        let day = Arc::clone(&budget.day);
        let sql = budget.acquire_sql.clone();
        let limit = budget.limit;
        let overhead = budget.overhead;
        scope.spawn(move || -> Result<Option<i64>, String> {
          let result = (|| -> CheckResult<Option<i64>> {
            let db = Budget::connect(&path, &day)?;
            let today = day.lock().map_err(|_| "poisoned day lock")?.clone();
            Ok(db.query_row(&sql, params![40_000, limit, today, overhead], |row| row.get(0)).optional()?)
          })();
          result.map_err(|error| error.to_string())
        })
      })
      .collect::<Vec<_>>();
    let mut grants = Vec::with_capacity(workers.len());
    for worker in workers {
      let result = worker.join().map_err(|_| "worker panicked")?;
      grants.push(result?);
    }
    Ok::<_, Box<dyn std::error::Error>>(grants)
  })?;
  let mut grants = grants.into_iter().flatten().collect::<Vec<_>>();
  grants.sort_unstable();
  assert_eq!(grants, [20_000, 40_000, 40_000]);
  assert_eq!(budget.reserved()?, budget.limit);
  Ok(())
}

#[test]
fn reapplying_migration_preserves_ledger() -> CheckResult {
  let budget = Budget::new()?;
  budget.acquire(budget.limit / 2)?;
  budget.db.execute_batch(&budget.migration)?;
  assert_eq!(budget.reserved()?, budget.limit / 2);
  Ok(())
}
