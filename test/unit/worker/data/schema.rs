use super::*;

#[test]
fn static_schemas_discover_known_tables() {
  assert!(columns("ktmb", "stops").is_some());
  assert!(columns("KTMB", "stops").is_some());
  assert!(columns("ktmb", "STOPS").is_some());
  assert!(columns("KTMB", "STOPS").is_some());
  assert!(columns("ktmb", "unknown_table").is_none());
  assert!(columns("unknown_provider", "stops").is_none());
}

#[test]
fn static_schemas_check_columns() {
  assert!(has_column("ktmb", "stops", "stop_id"));
  assert!(has_column("ktmb", "STOPS", "stop_id"));
  assert!(!has_column("ktmb", "stops", "nonexistent_col"));
  assert!(!has_column("ktmb", "trips", "trip_headsign"));
  assert!(has_column("rapid-bus-kl", "trips", "trip_headsign"));
  assert!(has_column("rapid-bus-kl", "frequencies", "trip_id"));
  assert!(!has_column("ktmb", "frequencies", "trip_id"));
}
