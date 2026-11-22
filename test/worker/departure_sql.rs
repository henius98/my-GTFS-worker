//! Exercise the Worker's cache SQL and migrations without compiling Worker code.

use gtfs_checks::{CheckResult, providers, root};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::fs;

type CacheColumn = (String, String, i64, Option<String>, i64);

fn sql(name: &str) -> CheckResult<String> {
  Ok(fs::read_to_string(root().join("worker/src/departures/sql").join(name))?)
}

fn database() -> CheckResult<Connection> {
  let db = Connection::open_in_memory()?;
  db.execute_batch(&fs::read_to_string(root().join("migrations/ktmb/0_gtfs_schema.sql"))?)?;
  db.execute_batch(&fs::read_to_string(root().join("migrations/ktmb/20261028_prefix_infrastructure_tables.sql"))?)?;
  Ok(db)
}

fn revision(db: &Connection) -> CheckResult<String> {
  Ok(db.query_row(&sql("feed_state.sql")?, [], |row| row.get(0))?)
}

fn write(db: &Connection, payload: &Value, requested_revision: Option<&str>, expires: i64, route: &str, direction: i64) -> CheckResult {
  let current = revision(db)?;
  let statement = sql("cache_write.sql")?.replace("__FEED_STATE__", &sql("feed_state.sql")?);
  db.execute(&statement, params!["S", route, direction, requested_revision.unwrap_or(&current), expires, payload.to_string()])?;
  Ok(())
}

fn read(db: &Connection, now: i64, route: &str, direction: i64) -> CheckResult<Option<Value>> {
  let payload: Option<String> = db.query_row(&sql("cache_read.sql")?, params!["S", route, direction, revision(db)?, now], |row| row.get(0)).optional()?;
  payload.map(|text| serde_json::from_str(&text).map_err(Into::into)).transpose()
}

#[test]
fn cache_expires_at_the_boundary_and_is_replaced() -> CheckResult {
  let db = database()?;
  write(&db, &json!({"departures":["2026-09-24T08:00:00+08:00"]}), None, 160, "", -1)?;
  assert!(read(&db, 159, "", -1)?.is_some());
  assert!(read(&db, 160, "", -1)?.is_none());
  write(&db, &json!({"departures":["2026-09-24T08:05:00+08:00"]}), None, 220, "", -1)?;
  assert_eq!(read(&db, 170, "", -1)?, Some(json!({"departures":["2026-09-24T08:05:00+08:00"]})));
  assert_eq!(db.query_row("SELECT COUNT(*) FROM departure_cache", [], |row| row.get::<_, i64>(0))?, 1);
  Ok(())
}

#[test]
fn route_and_direction_filters_have_distinct_entries() -> CheckResult {
  let db = database()?;
  let filters = [("", -1, "all"), ("R", -1, "route"), ("R", 0, "outbound"), ("R", 1, "inbound")];
  for &(route, direction, label) in &filters {
    write(&db, &json!({"filter": label}), None, 160, route, direction)?;
  }
  for &(route, direction, label) in &filters {
    assert_eq!(read(&db, 100, route, direction)?, Some(json!({"filter": label})));
  }
  Ok(())
}

#[test]
fn import_changes_invalidate_reads_and_reject_racing_writes() -> CheckResult {
  let db = database()?;
  db.execute("INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, Status) VALUES ('stop_times.txt', 'old', 10, 0)", [])?;
  let old_revision = revision(&db)?;
  write(&db, &json!({"generation":"old"}), None, 160, "", -1)?;
  db.execute("UPDATE \"0_import_progress\" SET CRC = 'new', Status = 1", [])?;
  assert_eq!(read(&db, 100, "", -1)?, None);
  db.execute("DELETE FROM departure_cache", [])?;
  write(&db, &json!({"generation":"racing"}), Some(&old_revision), 160, "", -1)?;
  assert_eq!(db.query_row("SELECT COUNT(*) FROM departure_cache", [], |row| row.get::<_, i64>(0))?, 0);
  write(&db, &json!({"generation":"in_progress"}), None, 160, "", -1)?;
  assert_eq!(read(&db, 100, "", -1)?, Some(json!({"generation":"in_progress"})));
  db.execute("UPDATE \"0_import_progress\" SET Status = 0, LastProcessedLine = 20", [])?;
  write(&db, &json!({"generation":"complete"}), None, 160, "", -1)?;
  assert_eq!(read(&db, 100, "", -1)?, Some(json!({"generation":"complete"})));
  Ok(())
}

#[test]
fn older_request_cannot_replace_a_newer_entry() -> CheckResult {
  let db = database()?;
  write(&db, &json!({"request":"new"}), None, 200, "", -1)?;
  write(&db, &json!({"request":"old"}), None, 160, "", -1)?;
  assert_eq!(read(&db, 100, "", -1)?, Some(json!({"request":"new"})));
  Ok(())
}

#[test]
fn new_revision_replaces_entry_even_if_old_entry_had_longer_expiry() -> CheckResult {
  let db = database()?;
  db.execute("INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, Status) VALUES ('stop_times.txt', 'v1', 10, 0)", [])?;
  write(&db, &json!({"ver":"v1"}), None, 500, "", -1)?;
  db.execute("UPDATE \"0_import_progress\" SET CRC = 'v2'", [])?;
  write(&db, &json!({"ver":"v2"}), None, 300, "", -1)?;
  assert_eq!(read(&db, 100, "", -1)?, Some(json!({"ver":"v2"})));
  Ok(())
}

#[test]
fn revision_changes_when_a_checkpoint_moves_without_a_new_crc() -> CheckResult {
  let db = database()?;
  db.execute("INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, Status) VALUES ('stop_times.txt', 'same', 10, 0)", [])?;
  write(&db, &json!({"generation":"old checkpoint"}), None, 160, "", -1)?;
  db.execute("UPDATE \"0_import_progress\" SET LastProcessedByte = 120", [])?;
  assert_eq!(read(&db, 100, "", -1)?, None);
  Ok(())
}

fn cache_columns(db: &Connection) -> CheckResult<Vec<CacheColumn>> {
  let mut statement = db.prepare("PRAGMA table_info(departure_cache)")?;
  Ok(statement.query_map([], |row| Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?.collect::<Result<_, _>>()?)
}

#[test]
fn every_provider_migration_matches_its_base_schema() -> CheckResult {
  for provider in providers()? {
    let folder = root().join("migrations").join(&provider.name);
    let schema = fs::read_to_string(folder.join("0_gtfs_schema.sql"))?;
    let db = Connection::open_in_memory()?;
    db.execute_batch(&schema)?;
    db.execute_batch(&schema)?;
    let columns = cache_columns(&db)?;
    let col_names = columns.iter().map(|c| c.0.as_str()).collect::<Vec<_>>();
    assert_eq!(col_names, ["stop_id", "route_id", "direction_id", "feed_revision", "expires_at", "payload"], "{}", provider.name);
    db.execute("INSERT OR IGNORE INTO stops (stop_id, stop_name) VALUES ('S', 'Existing stop')", [])?;
    assert_eq!(db.query_row("SELECT stop_name FROM stops WHERE stop_id = 'S'", [], |row| row.get::<_, String>(0))?, "Existing stop");
    let current = revision(&db)?;
    write(&db, &json!({"departures":[]}), Some(&current), 160, "", -1)?;
    assert_eq!(read(&db, 100, "", -1)?, Some(json!({"departures":[]})));
  }
  Ok(())
}

fn map_sql(name: &str) -> CheckResult<String> {
  Ok(fs::read_to_string(root().join("worker/src/map/sql").join(name))?)
}

fn explain_plan(db: &Connection, statement: &str, args: &[&dyn rusqlite::ToSql]) -> CheckResult<Vec<String>> {
  let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {statement}"))?;
  let rows = stmt.query_map(args, |row| row.get::<_, String>(3))?;
  Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[test]
fn ktmb_departure_query_uses_primary_keys_and_agency_fallback() -> CheckResult {
  let db = database()?;
  db.execute("INSERT INTO agency (agency_id, agency_timezone) VALUES ('KTMB', 'Asia/Kuala_Lumpur')", [])?;
  db.execute("INSERT INTO routes (route_id, agency_id, route_short_name) VALUES ('R1', 'KTMB', 'KTM1'), ('R2', '', 'KTM2')", [])?;
  db.execute("INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES ('S1', 'Sentral', 3.13, 101.68)", [])?;
  db.execute("INSERT INTO trips (trip_id, route_id, service_id, direction_id) VALUES ('T1', 'R1', 'SVC', 0), ('T2', 'R2', 'SVC', 1)", [])?;
  db.execute("INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES ('T1', 'S1', 100, '08:00:00', '08:01:00'), ('T2', 'S1', 100, '08:05:00', '08:06:00')", [])?;

  let agency_join = sql("agency_join.sql")?;
  let query = sql("query.sql")?
    .replace("__ROUTE_COLUMNS__", "r.route_short_name, r.route_long_name, r.route_color, r.route_text_color")
    .replace("__HEADSIGN__", "NULL")
    .replace("__AGENCY_JOIN__", agency_join.trim_end())
    .replace("__PICKUP_FILTER__", "");

  let plan = explain_plan(&db, &query, &[&"S1", &Option::<String>::None, &Option::<i64>::None])?;
  assert!(plan.iter().any(|step| step.contains("SEARCH st USING PRIMARY KEY (stop_id=?)")), "{plan:?}");
  assert!(plan.iter().any(|step| step.contains("SEARCH t USING PRIMARY KEY (trip_id=?)")), "{plan:?}");
  assert!(plan.iter().any(|step| step.contains("SEARCH r USING PRIMARY KEY (route_id=?)")), "{plan:?}");
  assert!(plan.iter().any(|step| step.contains("SEARCH a USING PRIMARY KEY (agency_id=?)")), "{plan:?}");
  assert!(!plan.iter().any(|step| step.starts_with("SCAN a ")), "{plan:?}");

  let mut stmt = db.prepare(&query)?;
  let rows = stmt.query_map(params!["S1", Option::<String>::None, Option::<i64>::None], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(18)?)))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(rows, vec![("T1".into(), Some("Asia/Kuala_Lumpur".into())), ("T2".into(), Some("Asia/Kuala_Lumpur".into()))]);
  Ok(())
}

#[test]
fn frequency_departure_query_avoids_correlated_stop_times_scan() -> CheckResult {
  let db = Connection::open_in_memory()?;
  db.execute_batch(&fs::read_to_string(root().join("migrations/rapid-bus-kl/0_gtfs_schema.sql"))?)?;
  db.execute("INSERT INTO agency (agency_id, agency_timezone) VALUES ('RKL', 'Asia/Kuala_Lumpur')", [])?;
  db.execute("INSERT INTO routes (route_id, agency_id, route_short_name) VALUES ('U1', 'RKL', 'U1')", [])?;
  db.execute("INSERT INTO trips (trip_id, route_id, service_id, trip_headsign, direction_id) VALUES ('TF', 'U1', 'SVC', 'Freq', 0), ('TS', 'U1', 'SVC', 'Sched', 1)", [])?;
  db.execute(
    "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES ('TF', 'S1', 1, '06:00:00', '06:00:00'), ('TF', 'S2', 2, '06:05:00', '06:05:00'), ('TS', 'S2', 1, '07:00:00', '07:00:00')",
    [],
  )?;
  db.execute("INSERT INTO frequencies (trip_id, start_time, end_time, headway_secs, exact_times) VALUES ('TF', '08:00:00', '09:00:00', 600, 0)", [])?;

  let agency_join = sql("agency_join.sql")?;
  let query = sql("query_frequencies.sql")?
    .replace("__ROUTE_COLUMNS__", "r.route_short_name, r.route_long_name, r.route_color, r.route_text_color")
    .replace("__HEADSIGN__", "COALESCE(NULLIF(st.stop_headsign, ''), t.trip_headsign)")
    .replace("__EXACT_TIMES__", "f.exact_times")
    .replace("__AGENCY_JOIN__", agency_join.trim_end())
    .replace("__PICKUP_FILTER__", "");

  let plan = explain_plan(&db, &query, &[&"S2", &Option::<String>::None, &Option::<i64>::None])?;
  assert!(plan.iter().any(|step| step.contains("MATERIALIZE matched")), "{plan:?}");
  assert!(plan.iter().any(|step| step.contains("SEARCH a USING PRIMARY KEY (agency_id=?)")), "{plan:?}");
  assert!(!plan.iter().any(|step| step.contains("CORRELATED SCALAR SUBQUERY")), "{plan:?}");

  let mut stmt = db.prepare(&query)?;
  let mut rows = stmt.query_map(params!["S2", Option::<String>::None, Option::<i64>::None], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(12)?)))?.collect::<Result<Vec<_>, _>>()?;
  rows.sort();
  assert_eq!(rows, vec![("TF".into(), Some("06:00:00".into())), ("TS".into(), None)]);
  Ok(())
}

#[test]
fn ktmb_map_queries_deduplicate_and_use_primary_keys() -> CheckResult {
  let db = database()?;
  db.execute("INSERT INTO routes (route_id) VALUES ('R1'), ('R2')", [])?;
  db.execute("INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES ('S1', 'A', 3.1, 101.6), ('S2', 'B', 3.2, 101.7), ('S3', 'C', 3.3, 101.8)", [])?;
  db.execute("INSERT INTO trips (trip_id, route_id, service_id, direction_id) VALUES ('T1', 'R1', 'SVC', 0), ('T2', 'R1', 'SVC', 0), ('T3', 'R2', 'SVC', 0)", [])?;
  db.execute(
    "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES ('T1', 'S1', 1, '08:00:00', '08:00:00'), ('T1', 'S2', 2, '08:10:00', '08:10:00'), ('T2', 'S1', 1, '09:00:00', '09:00:00'), ('T2', 'S2', 2, '09:10:00', '09:10:00'), ('T3', 'S2', 1, '10:00:00', '10:00:00'), ('T3', 'S3', 2, '10:10:00', '10:10:00')",
    [],
  )?;

  let segments_sql = map_sql("segments.sql")?.replace("__TRIP_FILTER__", " WHERE trips.route_id = ?1");
  let segments = db.prepare(&segments_sql)?.query_map(params!["R1"], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(segments, vec![("R1".into(), "S1".into(), "S2".into())]);

  let stops_by_route_sql = map_sql("stops_by_route.sql")?;
  let plan = explain_plan(&db, &stops_by_route_sql, &[&"R1"])?;
  assert!(plan.iter().any(|step| step.contains("SEARCH stop_times USING PRIMARY KEY (stop_id=?)")), "{plan:?}");
  assert!(!plan.iter().any(|step| step.contains("TEMP B-TREE")), "{plan:?}");

  let filtered_stops = db.prepare(&stops_by_route_sql)?.query_map(params!["R1"], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(filtered_stops, vec![("R1".into(), "S1".into()), ("R1".into(), "S2".into())]);

  let all_stops_sql = map_sql("stops.sql")?;
  let all_stops = db.prepare(&all_stops_sql)?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(all_stops, vec![("R1".into(), "S1".into()), ("R1".into(), "S2".into()), ("R2".into(), "S2".into()), ("R2".into(), "S3".into())]);
  Ok(())
}
