//! Exercise the Worker's cache SQL and migrations without compiling Worker code.

use gtfs_checks::{CheckResult, providers, root};
use rusqlite::{Connection, params};
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
  let statement = sql("cache_read.sql")?.replace("__FEED_STATE__", &sql("feed_state.sql")?);
  let payload: Option<String> = db.query_row(&statement, params!["S", route, direction, now], |row| row.get(1))?;
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
  assert!(plan.iter().any(|step| step.contains("SEARCH st USING PRIMARY KEY (trip_id=? AND stop_id=?)")), "{plan:?}");
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
  db.execute("INSERT INTO calendar (service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date) VALUES ('SVC', 1, 1, 1, 1, 1, 1, 1, 20260101, 20261231)", [])?;
  db.execute("INSERT INTO trips (trip_id, route_id, service_id, direction_id) VALUES ('T1', 'R1', 'SVC', 0), ('T2', 'R1', 'SVC', 1), ('T3', 'R2', 'SVC', 0)", [])?;
  db.execute(
    "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES ('T1', 'S1', 1, '08:00:00', '08:00:00'), ('T1', 'S2', 2, '08:10:00', '08:10:00'), ('T2', 'S2', 1, '09:00:00', '09:00:00'), ('T2', 'S1', 2, '09:10:00', '09:10:00'), ('T3', 'S2', 1, '10:00:00', '10:00:00'), ('T3', 'S3', 2, '10:10:00', '10:10:00')",
    [],
  )?;

  let active_services = map_sql("active_services_calendar.sql")?.trim_end().replace("__WEEKDAY__", "monday").replace("__DATE__", "20260921");

  let route_trips_sql = map_sql("route_trips.sql")?
    .replace("__ACTIVE_SERVICES__", &active_services)
    .replace("__ROUTE_COLUMNS__", "routes.route_short_name, routes.route_long_name, routes.route_color")
    .replace("__SHAPE_IDS_EXPR__", "'[]' AS shape_ids_json");
  let route_trips_plan = explain_plan(&db, &route_trips_sql, &[&r#"["R1","R2"]"#])?;
  assert!(route_trips_plan.first().is_some_and(|step| step.contains("SEARCH routes USING PRIMARY KEY (route_id=?)")), "{route_trips_plan:?}");
  assert!(route_trips_plan.iter().any(|step| step.contains("SEARCH trips USING PRIMARY KEY (route_id=?")), "{route_trips_plan:?}");
  let multi_route_rows = db
    .prepare(&route_trips_sql)?
    .query_map(params![r#"["R1","R2","MISSING"]"#], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?)))?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(multi_route_rows.len(), 2);
  assert_eq!(multi_route_rows[0].0, "R1");
  assert_eq!(serde_json::from_str::<Vec<String>>(&multi_route_rows[0].1)?, vec!["T1", "T2"]);
  assert_eq!(multi_route_rows[0].2, "[]");
  assert_eq!(multi_route_rows[1].0, "R2");
  assert_eq!(serde_json::from_str::<Vec<String>>(&multi_route_rows[1].1)?, vec!["T3"]);
  assert_eq!(multi_route_rows[1].2, "[]");
  let missing_trips = db.prepare(&route_trips_sql)?.query_map(params![r#"["MISSING"]"#], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
  assert!(missing_trips.is_empty());

  let route_trip_pairs_json = r#"[["R1","T1"],["R1","T2"],["R2","T3"]]"#;

  // Reverse trips (S1 -> S2 and S2 -> S1) collapse into one undirected coordinate pair per route in a single materialized scan.
  let merged_by_route_sql = map_sql("segments_and_stops_by_route.sql")?;
  let merged_plan = explain_plan(&db, &merged_by_route_sql, &[&route_trip_pairs_json])?;
  assert!(merged_plan.iter().any(|step| step.contains("MATERIALIZE trip_stop_times")), "{merged_plan:?}");
  assert!(merged_plan.iter().any(|step| step.contains("SEARCH stop_times USING PRIMARY KEY (trip_id=?)")), "{merged_plan:?}");
  assert!(merged_plan.iter().any(|step| step.contains("SEARCH stops USING PRIMARY KEY (stop_id=?)")), "{merged_plan:?}");
  let merged_rows = db
    .prepare(&merged_by_route_sql)?
    .query_map(params![route_trip_pairs_json], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, f64>(4)?, row.get::<_, Option<f64>>(6)?)))?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(
    merged_rows,
    vec![
      ("segment".into(), "R1".into(), None, 3.1, Some(3.2)),
      ("segment".into(), "R2".into(), None, 3.2, Some(3.3)),
      ("stop".into(), "R1".into(), Some("S1".into()), 3.1, None),
      ("stop".into(), "R1".into(), Some("S2".into()), 3.2, None),
      ("stop".into(), "R2".into(), Some("S2".into()), 3.2, None),
      ("stop".into(), "R2".into(), Some("S3".into()), 3.3, None),
    ]
  );

  let stops_by_route_sql = map_sql("stops_by_route.sql")?;
  let plan = explain_plan(&db, &stops_by_route_sql, &[&route_trip_pairs_json])?;
  assert!(plan.iter().any(|step| step.contains("SEARCH stop_times USING PRIMARY KEY (trip_id=?)")), "{plan:?}");
  assert!(plan.iter().any(|step| step.contains("SEARCH stops USING PRIMARY KEY (stop_id=?)")), "{plan:?}");
  assert!(!plan.iter().any(|step| step.contains("trips")), "{plan:?}");

  let filtered_stops = db
    .prepare(&stops_by_route_sql)?
    .query_map(params![route_trip_pairs_json], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(5)?)))?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(
    filtered_stops,
    vec![
      ("R1".into(), "S1".into(), Some("08:00:00, 09:10:00".into())),
      ("R1".into(), "S2".into(), Some("08:10:00, 09:00:00".into())),
      ("R2".into(), "S2".into(), Some("10:00:00".into())),
      ("R2".into(), "S3".into(), Some("10:10:00".into())),
    ]
  );

  let all_merged_sql = map_sql("segments_and_stops.sql")?.replace("__ACTIVE_SERVICES__", &active_services);
  let all_merged = db
    .prepare(&all_merged_sql)?
    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, f64>(4)?, row.get::<_, Option<f64>>(6)?)))?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(
    all_merged,
    vec![
      ("segment".into(), "R1".into(), None, 3.1, Some(3.2)),
      ("segment".into(), "R2".into(), None, 3.2, Some(3.3)),
      ("stop".into(), "R1".into(), Some("S1".into()), 3.1, None),
      ("stop".into(), "R1".into(), Some("S2".into()), 3.2, None),
      ("stop".into(), "R2".into(), Some("S2".into()), 3.2, None),
      ("stop".into(), "R2".into(), Some("S3".into()), 3.3, None),
    ]
  );

  let all_stops_sql = map_sql("stops.sql")?.replace("__ACTIVE_SERVICES__", &active_services);
  let all_stops = db
    .prepare(&all_stops_sql)?
    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?)))?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(
    all_stops,
    vec![
      ("R1".into(), "S1".into(), Some(r#"["T1","T2"]"#.into()), Some("08:00:00, 09:10:00".into())),
      ("R1".into(), "S2".into(), Some(r#"["T1","T2"]"#.into()), Some("08:10:00, 09:00:00".into())),
      ("R2".into(), "S2".into(), Some(r#"["T3"]"#.into()), Some("10:00:00".into())),
      ("R2".into(), "S3".into(), Some(r#"["T3"]"#.into()), Some("10:10:00".into())),
    ]
  );
  Ok(())
}

#[test]
fn map_queries_filter_by_date_with_calendar_and_calendar_dates_exceptions() -> CheckResult {
  let db = Connection::open_in_memory()?;
  db.execute_batch(&fs::read_to_string(root().join("migrations/rapid-bus-mrtfeeder/0_gtfs_schema.sql"))?)?;
  db.execute("INSERT INTO routes (route_id) VALUES ('U1')", [])?;
  db.execute("INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES ('S1', 'A', 3.1, 101.6), ('S2', 'B', 3.2, 101.7), ('S3', 'C', 3.3, 101.8), ('S4', 'D', 3.4, 101.9)", [])?;
  db.execute(
    "INSERT INTO shapes (shape_id, shape_pt_lat, shape_pt_lon, shape_pt_sequence) VALUES ('SH_WD', 3.1, 101.6, 1), ('SH_WD', 3.2, 101.7, 2), ('SH_WE', 3.1, 101.6, 1), ('SH_WE', 3.3, 101.8, 2), ('SH_HOL', 3.1, 101.6, 1), ('SH_HOL', 3.4, 101.9, 2)",
    [],
  )?;
  db.execute(
    "INSERT INTO calendar (service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date) VALUES ('WD', 1, 1, 1, 1, 1, 0, 0, 20260901, 20260930), ('WE', 0, 0, 0, 0, 0, 1, 1, 20260901, 20260930)",
    [],
  )?;
  // 2026-09-21 (Monday): remove WD (exception_type = 2), add HOL (calendar_dates-only, exception_type = 1)
  // 2026-09-28 (Monday): normal WD schedule
  db.execute("INSERT INTO calendar_dates (service_id, date, exception_type) VALUES ('WD', 20260921, 2), ('HOL', 20260921, 1)", [])?;
  db.execute("INSERT INTO trips (trip_id, route_id, service_id, shape_id) VALUES ('T_WD', 'U1', 'WD', 'SH_WD'), ('T_WE', 'U1', 'WE', 'SH_WE'), ('T_HOL', 'U1', 'HOL', 'SH_HOL')", [])?;
  db.execute(
    "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES ('T_WD', 'S1', 1, '08:00:00', '08:00:00'), ('T_WD', 'S2', 2, '08:10:00', '08:10:00'), ('T_WE', 'S1', 1, '09:00:00', '09:00:00'), ('T_WE', 'S3', 2, '09:10:00', '09:10:00'), ('T_HOL', 'S1', 1, '10:00:00', '10:00:00'), ('T_HOL', 'S4', 2, '10:10:00', '10:10:00')",
    [],
  )?;

  let exc_cte_template = map_sql("active_services_with_exceptions.sql")?;
  let exc_cte = |yyyymmdd: i32, weekday: &str| exc_cte_template.trim_end().replace("__WEEKDAY__", weekday).replace("__DATE__", &yyyymmdd.to_string());
  let cal_only_cte = map_sql("active_services_calendar.sql")?.trim_end().replace("__WEEKDAY__", "monday").replace("__DATE__", "20260928");
  let route_trips_template = map_sql("route_trips.sql")?
    .replace("__ROUTE_COLUMNS__", "routes.route_short_name, routes.route_long_name, NULL AS route_color")
    .replace("__SHAPE_IDS_EXPR__", "json_group_array(DISTINCT trips.shape_id) FILTER (WHERE trips.shape_id IS NOT NULL AND trips.shape_id <> '') AS shape_ids_json");

  // Filtered-route query starts from routes primary key and returns JSON trip/shape arrays.
  let cal_trips_sql = route_trips_template.replace("__ACTIVE_SERVICES__", &cal_only_cte);
  let plan = explain_plan(&db, &cal_trips_sql, &[&r#"["U1"]"#])?;
  assert!(plan.first().is_some_and(|step| step.contains("SEARCH routes USING PRIMARY KEY (route_id=?)")), "{plan:?}");
  let (cal_trip_ids_json, cal_shape_ids_json): (String, String) = db.query_row(&cal_trips_sql, params![r#"["U1"]"#], |row| Ok((row.get(4)?, row.get(5)?)))?;
  assert_eq!(cal_trip_ids_json, r#"["T_WD"]"#);
  assert_eq!(cal_shape_ids_json, r#"["SH_WD"]"#);

  let shapes_by_route_sql = map_sql("shapes_by_route.sql")?;
  let shape_plan = explain_plan(&db, &shapes_by_route_sql, &[&cal_shape_ids_json])?;
  assert!(shape_plan.iter().any(|step| step.contains("SEARCH shapes USING PRIMARY KEY (shape_id=?)")), "{shape_plan:?}");
  let cal_shapes = db.prepare(&shapes_by_route_sql)?.query_map(params![&cal_shape_ids_json], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(cal_shapes, vec!["SH_WD", "SH_WD"]);

  // Holiday Monday (2026-09-21): WD removed, HOL added.
  let hol_cte = exc_cte(20260921, "monday");
  let hol_trips_sql = route_trips_template.replace("__ACTIVE_SERVICES__", &hol_cte);
  let (hol_trip_ids_json, hol_shape_ids_json): (String, String) = db.query_row(&hol_trips_sql, params![r#"["U1"]"#], |row| Ok((row.get(4)?, row.get(5)?)))?;
  assert_eq!(hol_trip_ids_json, r#"["T_HOL"]"#);
  assert_eq!(hol_shape_ids_json, r#"["SH_HOL"]"#);
  let hol_trip_pairs_json = r#"[["U1","T_HOL"]]"#;

  // Out-of-range Monday (2026-10-05): no active trips returns 0 rows.
  let oob_cte = exc_cte(20261005, "monday");
  let oob_trips_sql = route_trips_template.replace("__ACTIVE_SERVICES__", &oob_cte);
  let oob_trips = db.prepare(&oob_trips_sql)?.query_map(params![r#"["U1"]"#], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
  assert!(oob_trips.is_empty());

  let hol_shapes = db.prepare(&shapes_by_route_sql)?.query_map(params![&hol_shape_ids_json], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(hol_shapes, vec!["SH_HOL", "SH_HOL"]);

  let hol_merged_sql = map_sql("segments_and_stops_by_route.sql")?;
  let hol_segments = db
    .prepare(&hol_merged_sql)?
    .query_map(params![hol_trip_pairs_json], |row| {
      Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(4)?, row.get::<_, f64>(5)?, row.get::<_, Option<f64>>(6)?, row.get::<_, Option<f64>>(7)?))
    })?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(hol_segments.into_iter().filter(|row| row.0 == "segment").collect::<Vec<_>>(), vec![("segment".into(), "U1".into(), 3.1, 101.6, Some(3.4), Some(101.9))]);

  let hol_stops_sql = map_sql("stops_by_route.sql")?;
  let hol_stops = db
    .prepare(&hol_stops_sql)?
    .query_map(params![hol_trip_pairs_json], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(5)?)))?
    .collect::<Result<Vec<_>, _>>()?;
  assert_eq!(hol_stops, vec![("U1".into(), "S1".into(), Some("10:00:00".into())), ("U1".into(), "S4".into(), Some("10:10:00".into()))]);

  let hol_all_stops_sql = map_sql("stops.sql")?.replace("__ACTIVE_SERVICES__", &hol_cte);
  let hol_all_stops =
    db.prepare(&hol_all_stops_sql)?.query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?)))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(hol_all_stops, vec![("S1".into(), Some(r#"["T_HOL"]"#.into()), Some("10:00:00".into())), ("S4".into(), Some(r#"["T_HOL"]"#.into()), Some("10:10:00".into()))]);

  // Normal Monday (2026-09-28): WD active, HOL inactive.
  let norm_cte = exc_cte(20260928, "monday");
  let norm_shapes_sql = map_sql("shapes.sql")?.replace("__ACTIVE_SERVICES__", &norm_cte);
  let norm_shapes = db.prepare(&norm_shapes_sql)?.query_map([], |row| row.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(norm_shapes, vec!["SH_WD", "SH_WD"]);

  // Out-of-range Monday (2026-10-05): no active service.
  let oob_shapes_sql = map_sql("shapes.sql")?.replace("__ACTIVE_SERVICES__", &oob_cte);
  let oob_shapes = db.prepare(&oob_shapes_sql)?.query_map([], |row| row.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
  assert!(oob_shapes.is_empty());
  Ok(())
}
