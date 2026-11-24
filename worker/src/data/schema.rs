include!(concat!(env!("OUT_DIR"), "/schemas.rs"));

pub(super) fn columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {
  table_columns(provider, table)
}

pub(crate) fn has_column(provider: &str, table: &str, column: &str) -> bool {
  columns(provider, table).is_some_and(|cols| cols.iter().any(|c| c.eq_ignore_ascii_case(column)))
}

pub(crate) fn optional_columns(provider: &str, table: &str, prefix: &str, cols: &[&str]) -> String {
  cols.iter().map(|&col| if has_column(provider, table, col) { format!("{prefix}{col}") } else { format!("NULL AS {col}") }).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
#[path = "../../../test/unit/worker/data/schema.rs"]
mod tests;
