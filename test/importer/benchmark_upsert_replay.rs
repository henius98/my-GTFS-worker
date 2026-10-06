//! Benchmark replay write amplification for the importer's UPSERT strategy.
//! SQLite JSON1 is a local CPU and changed-row proxy; it does not measure
//! Cloudflare latency or billed D1 usage.

use gtfs_checks::{CheckResult, benchmark_options};
use rusqlite::{Connection, params};
use std::time::Instant;

const REPLACE_SQL: &str =
  "INSERT OR REPLACE INTO stop_times (trip_id, stop_sequence, arrival_time) SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]'), json_extract(value, '$[2]') FROM json_each(?)";
const CONDITIONAL_SQL: &str = "INSERT INTO stop_times (trip_id, stop_sequence, arrival_time) SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]'), json_extract(value, '$[2]') FROM json_each(?) WHERE TRUE ON CONFLICT DO UPDATE SET trip_id = excluded.trip_id, stop_sequence = excluded.stop_sequence, arrival_time = excluded.arrival_time WHERE stop_times.trip_id IS NOT excluded.trip_id OR stop_times.stop_sequence IS NOT excluded.stop_sequence OR stop_times.arrival_time IS NOT excluded.arrival_time";

fn median(mut samples: Vec<f64>) -> f64 {
  samples.sort_by(f64::total_cmp);
  let middle = samples.len() / 2;
  if samples.len().is_multiple_of(2) { (samples[middle - 1] + samples[middle]) / 2.0 } else { samples[middle] }
}

fn benchmark(sql: &str, payload: &str, iterations: usize) -> CheckResult<(f64, Vec<u64>, i64)> {
  let db = Connection::open_in_memory()?;
  db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; CREATE TABLE stop_times (trip_id TEXT, stop_sequence INTEGER, arrival_time TEXT, PRIMARY KEY (trip_id, stop_sequence));")?;
  db.execute(REPLACE_SQL, params![payload])?;
  for _ in 0..3 {
    db.execute(sql, params![payload])?;
  }
  let mut samples = Vec::with_capacity(iterations);
  let mut changes = Vec::with_capacity(iterations);
  for _ in 0..iterations {
    let before = db.total_changes();
    let start = Instant::now();
    db.execute(sql, params![payload])?;
    samples.push(start.elapsed().as_secs_f64() * 1_000.0);
    changes.push(db.total_changes() - before);
  }
  let count = db.query_row("SELECT COUNT(*) FROM stop_times", [], |row| row.get(0))?;
  Ok((median(samples), changes, count))
}

fn main() -> CheckResult {
  let (rows, iterations) = benchmark_options(50_000, 9)?;
  let payload = serde_json::to_string(&(0..rows).map(|index| [format!("trip-{}", index / 40), (index % 40).to_string(), format!("{:02}:00:00", index % 24)]).collect::<Vec<_>>())?;
  let (replace_ms, replace_changes, replace_rows) = benchmark(REPLACE_SQL, &payload, iterations)?;
  let (conditional_ms, conditional_changes, conditional_rows) = benchmark(CONDITIONAL_SQL, &payload, iterations)?;
  println!("rows={rows} iterations={iterations}");
  println!("replace_replay_median_ms={replace_ms:.4}\nconditional_replay_median_ms={conditional_ms:.4}");
  println!("replay_median_reduction_pct={:.2}", (1.0 - conditional_ms / replace_ms) * 100.0);
  println!("replace_changed_rows_per_replay={replace_changes:?}\nconditional_changed_rows_per_replay={conditional_changes:?}");
  if replace_rows != rows as i64 || conditional_rows != rows as i64 || replace_changes.iter().any(|&count| count != rows as u64) || conditional_changes.iter().any(|&count| count != 0) {
    return Err("UPSERT replay changed rows unexpectedly".into());
  }
  Ok(())
}
