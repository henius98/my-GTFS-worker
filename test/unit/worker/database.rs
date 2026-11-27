use serde_json::Value;
use worker::D1ResultMeta;

use super::{binding_name, format_d1_query_log};

#[test]
fn formats_provider_d1_binding_name() {
  assert_eq!(binding_name("ktmb"), "DB_KTMB");
  assert_eq!(binding_name("rapid-bus-kl"), "DB_RAPID_BUS_KL");
}

#[test]
fn formats_d1_query_log_with_quota_metadata() {
  let meta = D1ResultMeta { changed_db: Some(true), changes: Some(1), duration: Some(4.25), last_row_id: Some(9), rows_read: Some(42), rows_written: Some(3), size_after: Some(16_384) };

  let line = format_d1_query_log("ktmb", "DB_KTMB", "map", "route_trips", true, Some(&meta), None);
  let Some(line) = line else {
    panic!("expected serialized log line");
  };
  let parsed: Value = match serde_json::from_str(&line) {
    Ok(value) => value,
    Err(err) => panic!("invalid JSON log line: {err}"),
  };

  assert_eq!(parsed["event"], "d1_query");
  assert_eq!(parsed["provider"], "ktmb");
  assert_eq!(parsed["database"], "DB_KTMB");
  assert_eq!(parsed["route"], "map");
  assert_eq!(parsed["query"], "route_trips");
  assert_eq!(parsed["success"], true);
  assert_eq!(parsed["rows_read"], 42);
  assert_eq!(parsed["rows_written"], 3);
  assert_eq!(parsed["duration_ms"], 4.25);
  assert_eq!(parsed["changes"], 1);
  assert_eq!(parsed["changed_db"], true);
  assert_eq!(parsed["size_after"], 16_384);
  assert_eq!(parsed["last_row_id"], 9);
  assert!(parsed.get("error").is_none());
}

#[test]
fn formats_d1_query_log_for_errors_without_metadata() {
  let line = format_d1_query_log("ktmb", "DB_KTMB", "sql", "sql", false, None, Some("D1_ERROR: no such table"));
  let Some(line) = line else {
    panic!("expected serialized error log line");
  };
  let parsed: Value = match serde_json::from_str(&line) {
    Ok(value) => value,
    Err(err) => panic!("invalid JSON error log line: {err}"),
  };

  assert_eq!(parsed["event"], "d1_query");
  assert_eq!(parsed["provider"], "ktmb");
  assert_eq!(parsed["database"], "DB_KTMB");
  assert_eq!(parsed["route"], "sql");
  assert_eq!(parsed["query"], "sql");
  assert_eq!(parsed["success"], false);
  assert!(parsed["rows_read"].is_null());
  assert!(parsed["rows_written"].is_null());
  assert_eq!(parsed["error"], "D1_ERROR: no such table");
  assert!(parsed.get("size_after").is_none());
  assert!(parsed.get("last_row_id").is_none());
}
