use super::*;

#[test]
fn column_formatter_targets_only_gtfs_time_columns() {
  for (table, column) in [("stop_times", "arrival_time"), ("stop_times", "departure_time"), ("frequencies", "start_time"), ("frequencies", "end_time"), ("STOP_TIMES", "ARRIVAL_TIME")] {
    assert_eq!(ColumnFormatter::for_column(table, column), ColumnFormatter::GtfsTime, "{table}.{column}");
  }

  for (table, column) in
    [("stop_times", "trip_id"), ("stop_times", "stop_sequence"), ("frequencies", "headway_secs"), ("agency", "agency_timezone"), ("trips", "start_time"), ("stops", "arrival_time")]
  {
    assert_eq!(ColumnFormatter::for_column(table, column), ColumnFormatter::None, "{table}.{column}");
  }
}

#[test]
fn format_gtfs_time_zero_pads_unpadded_hours_and_fixes_lexicographical_order() {
  assert_eq!(format_gtfs_time("0:00:00"), "00:00:00");
  assert_eq!(format_gtfs_time("6:00:00"), "06:00:00");
  assert_eq!(format_gtfs_time("9:00:00"), "09:00:00");
  assert_eq!(format_gtfs_time(" 6:09:42 "), "06:09:42");
  assert_eq!(format_gtfs_time("6:5:9"), "06:05:09");

  let nine_am = format_gtfs_time("9:00:00");
  let five_pm = format_gtfs_time("17:00:00");
  let six_am = format_gtfs_time("6:00:00");
  let eleven_pm = format_gtfs_time("23:00:00");
  assert!(nine_am.as_ref() < five_pm.as_ref());
  assert!(six_am.as_ref() < eleven_pm.as_ref());
}

#[test]
fn format_gtfs_time_borrows_valid_padded_values_without_allocation() {
  for valid in ["06:00:00", "17:00:00", "23:59:59", "24:30:00", "100:00:00"] {
    let formatted = format_gtfs_time(valid);
    assert!(matches!(formatted, Cow::Borrowed(borrowed) if borrowed == valid), "{valid}");
  }
  let trimmed = format_gtfs_time(" 06:00:00 ");
  assert!(matches!(trimmed, Cow::Borrowed("06:00:00")));
}

#[test]
fn format_gtfs_time_leaves_empty_and_malformed_values_unchanged() {
  for raw in ["", "   ", "06:00", "6:60:00", "6:00:60", "-6:00:00", "ab:cd:ef", "6:00:00:00", "not-a-time"] {
    let formatted = format_gtfs_time(raw);
    assert!(matches!(formatted, Cow::Borrowed(borrowed) if borrowed == raw), "{raw}");
  }
}
