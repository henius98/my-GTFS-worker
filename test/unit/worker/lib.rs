use chrono::NaiveTime;

use super::{MAX_TTL_SECONDS, is_d1_row_read_limit, is_d1_row_read_limit_error, parse_hh_mm, parse_int_in_range, parse_non_empty, parse_timezone};

#[test]
fn detects_d1_daily_row_read_limit_errors() {
  assert!(is_d1_row_read_limit("Error: D1_ERROR: Your account has exceeded D1's free tier daily row read limit."));
  assert!(!is_d1_row_read_limit("Error: D1_ERROR: no such table: routes"));
  assert!(is_d1_row_read_limit_error(&worker::Error::RustError("exceeded daily row read limit".into())));
  assert!(!is_d1_row_read_limit_error(&worker::Error::RustError("no such table: routes".into())));
}

#[test]
fn parses_worker_env_values_and_rejects_invalid_inputs() {
  assert_eq!(parse_non_empty(" * "), Some("*"));
  assert_eq!(parse_non_empty("   "), None);

  assert_eq!(parse_int_in_range(" 1000 ", 1u32, u32::MAX), Some(1000));
  assert_eq!(parse_int_in_range("0", 1u32, u32::MAX), None);
  assert_eq!(parse_int_in_range("-1", 1u32, u32::MAX), None);

  assert_eq!(parse_int_in_range("100", 1usize, usize::MAX), Some(100));
  assert_eq!(parse_int_in_range("0", 1usize, usize::MAX), None);

  assert_eq!(parse_int_in_range("7", 1i64, 3_650), Some(7));
  assert_eq!(parse_int_in_range("0", 1i64, 3_650), None);
  assert_eq!(parse_int_in_range("3651", 1i64, 3_650), None);

  assert_eq!(parse_int_in_range("0", 0i64, MAX_TTL_SECONDS), Some(0));
  assert_eq!(parse_int_in_range("86400", 0i64, MAX_TTL_SECONDS), Some(86_400));
  assert_eq!(parse_int_in_range("-1", 0i64, MAX_TTL_SECONDS), None);

  assert_eq!(parse_timezone(" Asia/Kuala_Lumpur "), Some(chrono_tz::Asia::Kuala_Lumpur));
  assert_eq!(parse_timezone("UTC"), Some(chrono_tz::UTC));
  assert_eq!(parse_timezone(""), None);
  assert_eq!(parse_timezone("Invalid/Timezone"), None);

  assert_eq!(parse_hh_mm(" 05:00 "), NaiveTime::from_hms_opt(5, 0, 0));
  assert_eq!(parse_hh_mm("00:00"), NaiveTime::from_hms_opt(0, 0, 0));
  assert_eq!(parse_hh_mm("23:59"), NaiveTime::from_hms_opt(23, 59, 0));
  for invalid in ["", "5:00", "05:0", "24:00", "05:60", "05:00:00", "-1:00", "ab:cd"] {
    assert_eq!(parse_hh_mm(invalid), None, "{invalid}");
  }
}
