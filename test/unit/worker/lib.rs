use super::{MAX_TTL_SECONDS, is_d1_row_read_limit, parse_non_empty, parse_non_negative_i64, parse_positive_i64, parse_positive_u32, parse_positive_usize, parse_timezone};

#[test]
fn detects_d1_daily_row_read_limit_errors() {
  assert!(is_d1_row_read_limit("Error: D1_ERROR: Your account has exceeded D1's free tier daily row read limit."));
  assert!(!is_d1_row_read_limit("Error: D1_ERROR: no such table: routes"));
}

#[test]
fn parses_worker_env_values_and_rejects_invalid_inputs() {
  assert_eq!(parse_non_empty(" * "), Some("*"));
  assert_eq!(parse_non_empty("   "), None);

  assert_eq!(parse_positive_u32(" 1000 "), Some(1000));
  assert_eq!(parse_positive_u32("0"), None);
  assert_eq!(parse_positive_u32("-1"), None);

  assert_eq!(parse_positive_usize("100"), Some(100));
  assert_eq!(parse_positive_usize("0"), None);

  assert_eq!(parse_positive_i64("7", 3_650), Some(7));
  assert_eq!(parse_positive_i64("0", 3_650), None);
  assert_eq!(parse_positive_i64("3651", 3_650), None);

  assert_eq!(parse_non_negative_i64("0", MAX_TTL_SECONDS), Some(0));
  assert_eq!(parse_non_negative_i64("86400", MAX_TTL_SECONDS), Some(86_400));
  assert_eq!(parse_non_negative_i64("-1", MAX_TTL_SECONDS), None);

  assert_eq!(parse_timezone(" Asia/Kuala_Lumpur "), Some("Asia/Kuala_Lumpur"));
  assert_eq!(parse_timezone("UTC"), Some("UTC"));
  assert_eq!(parse_timezone(""), None);
  assert_eq!(parse_timezone("Invalid/Timezone"), None);
}
