use super::*;

#[test]
fn test_parse_filter_items_single_and_escaped() {
  assert_eq!(parse_filter_items(""), vec![""]);
  assert_eq!(parse_filter_items("   "), vec!["   "]);
  assert_eq!(parse_filter_items("val@col"), vec!["val@col"]);
  assert_eq!(parse_filter_items(r"val1\,val2@col"), vec!["val1,val2@col"]);
  assert_eq!(parse_filter_items("val1@col1,val2@col2"), vec!["val1@col1", "val2@col2"]);
}

#[test]
fn test_parse_range_bounds_valid_cases() {
  for (input, begin, end) in [
    ("1:10", Some("1"), Some("10")),
    ("1..10", Some("1"), Some("10")),
    (":10", None, Some("10")),
    ("5:", Some("5"), None),
    ("08:00:10:00", Some("08:00"), Some("10:00")),
    ("08:00:00:10:00:00", Some("08:00:00"), Some("10:00:00")),
    ("08:00:00..10:00:00", Some("08:00:00"), Some("10:00:00")),
    ("08:00:00:10:00", Some("08:00:00"), Some("10:00")),
    ("08:00:10:00:00", Some("08:00"), Some("10:00:00")),
    (":10:00:00", None, Some("10:00:00")),
    ("08:00:00:", Some("08:00:00"), None),
  ] {
    assert_eq!(parse_range_bounds(input), Ok(RangeBounds { begin, end }), "{input}");
  }
}

#[test]
fn test_parse_range_bounds_invalid_cases() {
  for input in [":", "::", "0:1:2", "08:00:00", ":5:", "..5..", "1..5..10", "5....", ":5..10", "5..10:", "1...10", "...", "1...", "...10"] {
    assert!(parse_range_bounds(input).is_err(), "{input}");
  }
}

#[test]
fn test_parse_range_part_validation() {
  assert_eq!(parse_range_part("Status[1:10]"), Ok(RangeFilter { column: "Status", bounds: RangeBounds { begin: Some("1"), end: Some("10") } }));
  for invalid in ["", "Status", "Status[1:10", "[1:10]"] {
    assert!(parse_range_part(invalid).is_err(), "{invalid}");
  }
  assert_eq!(parse_range_part("Status[0:1:2]"), Err("Invalid range format for Status. Expected col[begin:end]".into()));
  assert_eq!(parse_range_part("Status[::]"), Err("Invalid range format for Status. Expected col[begin:end]".into()));
}
