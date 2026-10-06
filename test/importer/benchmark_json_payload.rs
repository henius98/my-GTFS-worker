//! Benchmark the D1-facing JSON representations used by the importer.
//! SQLite JSON1 exercises the same JSON functions used by D1. Timings cover
//! local SQL execution only, not Cloudflare network latency.

use gtfs_checks::{CheckResult, benchmark_options};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};
use std::time::Instant;

const COLUMNS: [&str; 7] = ["route_id", "direction_id", "trip_id", "arrival_time", "departure_time", "stop_id", "stop_sequence"];
type InsertedRow = (String, i64, String, String, String, String, i64);

fn rows(count: usize) -> Vec<Vec<String>> {
  (0..count)
    .map(|index| {
      let timestamp = format!("{:02}:{:02}:{:02}", index / 3600 % 30, index / 60 % 60, index % 60);
      vec![format!("route-{}", index % 50), (index % 2).to_string(), format!("trip-{}", index / 40), timestamp.clone(), timestamp, format!("stop-{}", index % 2000), (index % 100).to_string()]
    })
    .collect()
}

fn median(mut samples: Vec<f64>) -> f64 {
  samples.sort_by(f64::total_cmp);
  let middle = samples.len() / 2;
  if samples.len().is_multiple_of(2) { (samples[middle - 1] + samples[middle]) / 2.0 } else { samples[middle] }
}

fn benchmark(db: &Connection, sql: &str, payload: &str, iterations: usize) -> CheckResult<f64> {
  for _ in 0..10 {
    db.execute("DELETE FROM stop_times", [])?;
    db.execute(sql, params![payload])?;
  }
  let mut samples = Vec::with_capacity(iterations);
  for _ in 0..iterations {
    db.execute("DELETE FROM stop_times", [])?;
    let start = Instant::now();
    db.execute(sql, params![payload])?;
    samples.push(start.elapsed().as_secs_f64() * 1_000.0);
  }
  Ok(median(samples))
}

fn inserted_rows(db: &Connection, quoted_columns: &str) -> CheckResult<Vec<InsertedRow>> {
  let mut stmt = db.prepare(&format!("SELECT {quoted_columns} FROM stop_times"))?;
  Ok(stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))?.collect::<Result<_, _>>()?)
}

fn main() -> CheckResult {
  let (row_count, iterations) = benchmark_options(500, 100)?;
  let rows = rows(row_count);
  let object_rows = rows
    .iter()
    .map(|row| {
      let map = COLUMNS.iter().zip(row).map(|(column, value)| (column.to_string(), Value::String(value.clone()))).collect::<Map<String, Value>>();
      Value::Object(map)
    })
    .collect::<Vec<_>>();
  let object_payload = serde_json::to_string(&object_rows)?;
  let positional_payload = serde_json::to_string(&rows)?;
  let quoted_columns = COLUMNS.iter().map(|column| format!("\"{column}\"")).collect::<Vec<_>>().join(", ");
  let object_selects = COLUMNS.iter().map(|column| format!("json_extract(value, '$.{column}')")).collect::<Vec<_>>().join(", ");
  let positional_selects = (0..COLUMNS.len()).map(|index| format!("json_extract(value, '$[{index}]')")).collect::<Vec<_>>().join(", ");
  let assignments = COLUMNS.iter().map(|column| format!("\"{column}\" = excluded.\"{column}\"")).collect::<Vec<_>>().join(", ");
  let changed = COLUMNS.iter().map(|column| format!("stop_times.\"{column}\" IS NOT excluded.\"{column}\"")).collect::<Vec<_>>().join(" OR ");
  let upsert = format!(" ON CONFLICT DO UPDATE SET {assignments} WHERE {changed}");
  let object_sql = format!("INSERT INTO stop_times ({quoted_columns}) SELECT {object_selects} FROM json_each(?) WHERE TRUE{upsert}");
  let positional_sql = format!("INSERT INTO stop_times ({quoted_columns}) SELECT {positional_selects} FROM json_each(?) WHERE TRUE{upsert}");

  let db = Connection::open_in_memory()?;
  db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; CREATE TABLE stop_times (route_id TEXT, direction_id INTEGER, trip_id TEXT, arrival_time TEXT, departure_time TEXT, stop_id TEXT, stop_sequence INTEGER, PRIMARY KEY (trip_id, stop_sequence));")?;
  db.execute(&object_sql, params![object_payload])?;
  let object_result = inserted_rows(&db, &quoted_columns)?;
  db.execute("DELETE FROM stop_times", [])?;
  db.execute(&positional_sql, params![positional_payload])?;
  if object_result != inserted_rows(&db, &quoted_columns)? {
    return Err("positional payload changed inserted values".into());
  }
  let object_ms = benchmark(&db, &object_sql, &object_payload, iterations)?;
  let positional_ms = benchmark(&db, &positional_sql, &positional_payload, iterations)?;
  let object_bytes = object_payload.len();
  let positional_bytes = positional_payload.len();
  println!("rows={row_count} columns={} iterations={iterations}", COLUMNS.len());
  println!("object_payload_bytes={object_bytes}\npositional_payload_bytes={positional_bytes}");
  println!("payload_reduction_pct={:.2}", (1.0 - positional_bytes as f64 / object_bytes as f64) * 100.0);
  println!("object_sql_median_ms={object_ms:.4}\npositional_sql_median_ms={positional_ms:.4}");
  println!("sql_median_change_pct={:.2}", (positional_ms / object_ms - 1.0) * 100.0);
  if positional_bytes >= object_bytes {
    return Err("positional payload was not smaller".into());
  }
  Ok(())
}
