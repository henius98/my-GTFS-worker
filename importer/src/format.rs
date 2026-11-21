use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColumnFormatter {
  None,
  GtfsTime,
}

impl ColumnFormatter {
  pub(crate) fn for_column(table_name: &str, column_name: &str) -> Self {
    if table_name.eq_ignore_ascii_case("stop_times") && (column_name.eq_ignore_ascii_case("arrival_time") || column_name.eq_ignore_ascii_case("departure_time")) {
      return Self::GtfsTime;
    }
    if table_name.eq_ignore_ascii_case("frequencies") && (column_name.eq_ignore_ascii_case("start_time") || column_name.eq_ignore_ascii_case("end_time")) {
      return Self::GtfsTime;
    }
    Self::None
  }

  pub(crate) fn format<'a>(self, value: &'a str) -> Cow<'a, str> {
    match self {
      Self::None => Cow::Borrowed(value),
      Self::GtfsTime => format_gtfs_time(value),
    }
  }
}

/// Normalizes GTFS time values (`H:MM:SS` or `HH:MM:SS`, including hours >= 24)
/// to zero-padded `HH:MM:SS` so SQLite lexicographical comparisons and sorting
/// match chronological order. Returns non-time or malformed values unchanged.
pub(crate) fn format_gtfs_time(value: &str) -> Cow<'_, str> {
  let trimmed = value.trim();
  let mut parts = trimmed.split(':');
  let (Some(hours), Some(minutes), Some(seconds), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
    return Cow::Borrowed(value);
  };
  if hours.is_empty()
    || minutes.is_empty()
    || seconds.is_empty()
    || minutes.len() > 2
    || seconds.len() > 2
    || !hours.bytes().all(|byte| byte.is_ascii_digit())
    || !minutes.bytes().all(|byte| byte.is_ascii_digit())
    || !seconds.bytes().all(|byte| byte.is_ascii_digit())
  {
    return Cow::Borrowed(value);
  }
  let Ok(minute_value) = minutes.parse::<u8>() else {
    return Cow::Borrowed(value);
  };
  let Ok(second_value) = seconds.parse::<u8>() else {
    return Cow::Borrowed(value);
  };
  if minute_value >= 60 || second_value >= 60 {
    return Cow::Borrowed(value);
  }
  if hours.len() >= 2 && minutes.len() == 2 && seconds.len() == 2 {
    return Cow::Borrowed(trimmed);
  }
  if hours.len() == 1 { Cow::Owned(format!("0{hours}:{minute_value:02}:{second_value:02}")) } else { Cow::Owned(format!("{hours}:{minute_value:02}:{second_value:02}")) }
}

#[cfg(test)]
#[path = "../../test/unit/importer/format.rs"]
mod tests;
