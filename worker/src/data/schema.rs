include!(concat!(env!("OUT_DIR"), "/schemas.rs"));

pub(super) fn columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {
  table_columns(&provider.to_ascii_lowercase(), &table.to_ascii_lowercase())
}

pub(crate) fn has_column(provider: &str, table: &str, column: &str) -> bool {
  columns(provider, table).is_some_and(|cols| cols.iter().any(|c| c.eq_ignore_ascii_case(column)))
}

#[cfg(test)]
mod tests {
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
}
