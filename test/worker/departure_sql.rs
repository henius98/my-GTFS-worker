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
  db.execute("INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status) VALUES ('ktmb', 'stop_times.txt', 'old', 10, 0)", [])?;
  let old_revision = revision(&db)?;
  write(&db, &json!({"generation":"old"}), None, 160, "", -1)?;
  db.execute("UPDATE import_progress SET CRC = 'new', Status = 1", [])?;
  assert_eq!(read(&db, 100, "", -1)?, None);
  let importing: i64 = db.query_row(&sql("feed_state.sql")?, [], |row| row.get(1))?;
  assert_eq!(importing, 1);
  db.execute("DELETE FROM departure_cache", [])?;
  write(&db, &json!({"generation":"racing"}), Some(&old_revision), 160, "", -1)?;
  write(&db, &json!({"generation":"partial"}), None, 160, "", -1)?;
  assert_eq!(db.query_row("SELECT COUNT(*) FROM departure_cache", [], |row| row.get::<_, i64>(0))?, 0);
  db.execute("UPDATE import_progress SET Status = 0, LastProcessedLine = 20", [])?;
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
fn revision_changes_when_a_checkpoint_moves_without_a_new_crc() -> CheckResult {
  let db = database()?;
  db.execute("INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status) VALUES ('ktmb', 'stop_times.txt', 'same', 10, 0)", [])?;
  write(&db, &json!({"generation":"old checkpoint"}), None, 160, "", -1)?;
  db.execute("UPDATE import_progress SET LastProcessedByte = 120", [])?;
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
    let migrations = fs::read_dir(&folder)?
      .map(|entry| entry.map(|entry| entry.path()))
      .collect::<Result<Vec<_>, _>>()?
      .into_iter()
      .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().ends_with("_add_departure_cache.sql")))
      .collect::<Vec<_>>();
    assert_eq!(migrations.len(), 1, "{}", provider.name);
    let fresh = Connection::open_in_memory()?;
    let upgraded = Connection::open_in_memory()?;
    fresh.execute_batch(&schema)?;
    upgraded.execute_batch(schema.split("-- Calculated departures;").next().ok_or("missing schema prefix")?)?;
    upgraded.execute("INSERT INTO stops (stop_id, stop_name) VALUES ('S', 'Existing stop')", [])?;
    let migration = fs::read_to_string(&migrations[0])?;
    upgraded.execute_batch(&migration)?;
    upgraded.execute_batch(&migration)?;
    assert_eq!(cache_columns(&fresh)?, cache_columns(&upgraded)?, "{}", provider.name);
    assert_eq!(upgraded.query_row("SELECT stop_name FROM stops WHERE stop_id = 'S'", [], |row| row.get::<_, String>(0))?, "Existing stop");
    let current = revision(&upgraded)?;
    write(&upgraded, &json!({"departures":[]}), Some(&current), 160, "", -1)?;
    assert_eq!(read(&upgraded, 100, "", -1)?, Some(json!({"departures":[]})));
  }
  Ok(())
}
