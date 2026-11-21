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
