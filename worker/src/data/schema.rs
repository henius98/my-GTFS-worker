include!(concat!(env!("OUT_DIR"), "/schemas.rs"));

pub(super) fn columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {
  table_columns(&provider.to_ascii_lowercase(), &table.to_ascii_lowercase())
}

pub(crate) fn has_column(provider: &str, table: &str, column: &str) -> bool {
  columns(provider, table).is_some_and(|cols| cols.iter().any(|c| c.eq_ignore_ascii_case(column)))
}

#[cfg(test)]
#[path = "../../../test/unit/worker/data/schema.rs"]
mod tests;
