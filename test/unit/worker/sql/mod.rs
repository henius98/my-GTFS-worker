use super::*;

#[test]
fn test_strip_sql_strings_replaces_literals_with_whitespace() {
  let sql = "SELECT * FROM routes WHERE desc = 'Express -- Non-stop; /* test */'";
  let stripped = strip_sql_strings(sql);
  assert!(!stripped.contains("--"));
  assert!(!stripped.contains(';'));
  assert!(!stripped.contains("/*"));
  assert!(stripped.starts_with("SELECT * FROM routes WHERE desc = "));
}

#[test]
fn test_strip_sql_strings_escaped_quotes() {
  let sql = "SELECT * FROM routes WHERE desc = 'it''s a -- test'";
  let stripped = strip_sql_strings(sql);
  assert!(!stripped.contains("--"));
}

#[test]
fn test_delimiters_outside_string_literals_detected() {
  let sql = "SELECT * FROM routes WHERE desc = 'safe'; DROP TABLE routes; --";
  let stripped = strip_sql_strings(sql);
  assert!(stripped.contains(';'));
  assert!(stripped.contains("--"));
}

#[test]
fn test_build_wrapped_query_preserves_sql_and_applies_limit() {
  assert_eq!(build_wrapped_query("SELECT '__LIMIT__'", 500), "SELECT * FROM (\nSELECT '__LIMIT__'\n) LIMIT 500");
}

#[test]
fn test_extract_bearer_token_handles_utf8_and_case() {
  assert_eq!(extract_bearer_token("Bearer secret-token"), Some("secret-token"));
  assert_eq!(extract_bearer_token("  bearer   spaced  "), Some("spaced"));
  assert_eq!(extract_bearer_token("Basic abc"), None);
  assert_eq!(extract_bearer_token("🔥123456"), None);
  assert_eq!(extract_bearer_token("éééé"), None);
}

#[test]
fn test_constant_time_eq() {
  assert!(constant_time_eq("secret", "secret"));
  assert!(!constant_time_eq("secret", "Secret"));
  assert!(!constant_time_eq("secret", "short"));
  assert!(!constant_time_eq("", "a"));
}
