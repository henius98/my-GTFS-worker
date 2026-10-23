include!(concat!(env!("OUT_DIR"), "/schemas.rs"));

pub(super) fn columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {
  let normalized = provider.to_ascii_lowercase();
  table_columns(&normalized, table)
}

pub(crate) fn has_column(provider: &str, table: &str, column: &str) -> bool {
  let normalized = provider.to_ascii_lowercase();
  has_table_column(&normalized, table, column)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn static_schemas_discover_known_tables() {
    assert!(columns("ktmb", "stops").is_some());
    assert!(columns("KTMB", "stops").is_some());
    assert!(columns("ktmb", "unknown_table").is_none());
    assert!(columns("unknown_provider", "stops").is_none());
  }

  #[test]
  fn static_schemas_check_columns() {
    assert!(has_column("ktmb", "stops", "stop_id"));
    assert!(!has_column("ktmb", "stops", "nonexistent_col"));
    assert!(!has_column("ktmb", "trips", "trip_headsign"));
    assert!(has_column("rapid-bus-kl", "trips", "trip_headsign"));
    assert!(has_column("rapid-bus-kl", "frequencies", "trip_id"));
    assert!(!has_column("ktmb", "frequencies", "trip_id"));
  }
}
