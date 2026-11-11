#![allow(dead_code)]

use std::{collections::BTreeMap, fs, path::Path};

pub fn split_top_level_comma(s: &str) -> Vec<&str> {
  let mut parts = Vec::new();
  let mut start = 0;
  let mut depth: usize = 0;
  let mut in_string = false;
  let mut chars = s.char_indices().peekable();
  while let Some((idx, ch)) = chars.next() {
    match ch {
      '\'' if !in_string => in_string = true,
      '\'' if in_string => {
        if chars.peek().map(|&(_, c)| c) == Some('\'') {
          chars.next();
        } else {
          in_string = false;
        }
      }
      '(' if !in_string => depth += 1,
      ')' if !in_string => depth = depth.saturating_sub(1),
      ',' if !in_string && depth == 0 => {
        parts.push(&s[start..idx]);
        start = idx + 1;
      }
      _ => {}
    }
  }
  if start < s.len() {
    parts.push(&s[start..]);
  }
  parts
}

pub fn split_top_level_semicolon(s: &str) -> Vec<&str> {
  let mut parts = Vec::new();
  let mut start = 0;
  let mut in_string = false;
  let mut chars = s.char_indices().peekable();
  while let Some((idx, ch)) = chars.next() {
    match ch {
      '\'' if !in_string => in_string = true,
      '\'' if in_string => {
        if chars.peek().map(|&(_, c)| c) == Some('\'') {
          chars.next();
        } else {
          in_string = false;
        }
      }
      ';' if !in_string => {
        parts.push(&s[start..idx]);
        start = idx + 1;
      }
      _ => {}
    }
  }
  if start < s.len() {
    parts.push(&s[start..]);
  }
  parts
}

pub fn strip_sql_comments(sql: &str) -> String {
  let mut result = String::new();
  let mut in_string = false;
  let mut chars = sql.chars().peekable();

  while let Some(ch) = chars.next() {
    match ch {
      '\'' if !in_string => {
        in_string = true;
        result.push(ch);
      }
      '\'' if in_string => {
        if chars.peek() == Some(&'\'') {
          result.push('\'');
          result.push(chars.next().unwrap());
        } else {
          in_string = false;
          result.push(ch);
        }
      }
      '-' if !in_string && chars.peek() == Some(&'-') => {
        chars.next();
        for next_ch in chars.by_ref() {
          if next_ch == '\n' {
            result.push('\n');
            break;
          }
        }
      }
      '/' if !in_string && chars.peek() == Some(&'*') => {
        chars.next();
        while let Some(c) = chars.next() {
          if c == '*' && chars.peek() == Some(&'/') {
            chars.next();
            break;
          }
        }
        result.push(' ');
      }
      _ => result.push(ch),
    }
  }
  result
}

pub fn strip_sql_strings(s: &str) -> String {
  let mut result = String::new();
  let mut in_string = false;
  let mut chars = s.chars().peekable();
  while let Some(ch) = chars.next() {
    match ch {
      '\'' if !in_string => {
        in_string = true;
        result.push(' ');
      }
      '\'' if in_string => {
        if chars.peek() == Some(&'\'') {
          chars.next();
          result.push(' ');
          result.push(' ');
        } else {
          in_string = false;
          result.push(' ');
        }
      }
      _ if in_string => result.push(' '),
      _ => result.push(ch),
    }
  }
  result
}

pub fn extract_first_identifier(s: &str) -> Option<&str> {
  let s = s.trim_start();
  if s.is_empty() {
    return None;
  }
  if s.starts_with('"') {
    s[1..].find('"').map(|end| &s[..end + 2])
  } else if s.starts_with('`') {
    s[1..].find('`').map(|end| &s[..end + 2])
  } else if s.starts_with('\'') {
    s[1..].find('\'').map(|end| &s[..end + 2])
  } else if s.starts_with('[') {
    s[1..].find(']').map(|end| &s[..end + 2])
  } else {
    let end = s.find(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',' || c == ';').unwrap_or(s.len());
    if end == 0 { None } else { Some(&s[..end]) }
  }
}

pub fn parse_table_definitions(sql: &str) -> Vec<(String, String)> {
  let clean_sql = strip_sql_comments(sql);
  let mut tables = Vec::new();

  for statement in split_top_level_semicolon(&clean_sql) {
    let statement = statement.trim();
    if statement.is_empty() {
      continue;
    }
    let mut tokens = statement.split_whitespace();
    let is_create_table = tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("CREATE")) && tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("TABLE"));
    if !is_create_table {
      continue;
    }
    let mut rest = statement.trim_start();
    // Skip "CREATE" and "TABLE"
    for _ in 0..2 {
      if let Some(end) = rest.find(|c: char| c.is_whitespace()) {
        rest = rest[end..].trim_start();
      }
    }
    // Check optional IF NOT EXISTS
    let mut after_table_tokens = rest.split_whitespace();
    if after_table_tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("IF"))
      && after_table_tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("NOT"))
      && after_table_tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("EXISTS"))
    {
      for _ in 0..3 {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        rest = rest[end..].trim_start();
      }
    }
    let Some(raw_table) = extract_first_identifier(rest) else {
      continue;
    };
    let table_name = raw_table.trim_matches(['"', '`', '\'', '[', ']']).to_string();
    if table_name.is_empty() {
      continue;
    }

    let rest_after_table = &rest[raw_table.len()..];
    let Some(open_paren_idx) = rest_after_table.find('(') else {
      continue;
    };
    let body = &rest_after_table[open_paren_idx + 1..];

    // Find the matching closing parenthesis for the table body
    let mut depth = 1;
    let mut in_str = false;
    let mut body_end = body.len();
    let mut chars = body.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
      match ch {
        '\'' if !in_str => in_str = true,
        '\'' if in_str => {
          if chars.peek().map(|&(_, c)| c) == Some('\'') {
            chars.next();
          } else {
            in_str = false;
          }
        }
        '(' if !in_str => depth += 1,
        ')' if !in_str => {
          depth -= 1;
          if depth == 0 {
            body_end = idx;
            break;
          }
        }
        _ => {}
      }
    }
    tables.push((table_name, body[..body_end].to_string()));
  }

  tables
}

fn is_direct_pk_constraint(first_word: &str, second_word: &str) -> bool {
  first_word.eq_ignore_ascii_case("PRIMARY") && second_word.to_ascii_uppercase().starts_with("KEY")
}

fn is_other_table_constraint(first_word: &str, second_word: &str) -> bool {
  (first_word.eq_ignore_ascii_case("FOREIGN") && second_word.to_ascii_uppercase().starts_with("KEY"))
    || first_word.eq_ignore_ascii_case("UNIQUE")
    || first_word.to_ascii_uppercase().starts_with("UNIQUE(")
    || first_word.eq_ignore_ascii_case("CHECK")
    || first_word.to_ascii_uppercase().starts_with("CHECK(")
    || first_word.eq_ignore_ascii_case("CONSTRAINT")
}

fn has_primary_key_tokens(s: &str) -> bool {
  let clean = strip_sql_strings(s);
  let mut prev_was_primary = false;
  for word in clean.split_whitespace() {
    if prev_was_primary {
      let upper = word.to_ascii_uppercase();
      if upper == "KEY" || upper.starts_with("KEY(") || upper.starts_with("KEY,") {
        return true;
      }
    }
    prev_was_primary = word.eq_ignore_ascii_case("PRIMARY");
  }
  false
}

pub fn parse_schema_file(path: &Path) -> BTreeMap<String, Vec<String>> {
  let sql = fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
  parse_schema_str(&sql)
}

pub fn parse_schema_str(sql: &str) -> BTreeMap<String, Vec<String>> {
  let mut schemas = BTreeMap::new();

  for (table_name, body_inner) in parse_table_definitions(sql) {
    let mut current_cols = Vec::new();
    for col_def in split_top_level_comma(&body_inner) {
      let col_def = col_def.trim();
      if col_def.is_empty() {
        continue;
      }
      let mut words = col_def.split_whitespace();
      let first_word = words.next().unwrap_or("");
      let second_word = words.next().unwrap_or("");
      if is_direct_pk_constraint(first_word, second_word) || is_other_table_constraint(first_word, second_word) {
        continue;
      }
      if let Some(col_name) = extract_first_identifier(col_def) {
        let col_name = col_name.trim_matches(['"', '`', '\'', '[', ']']);
        if !col_name.is_empty() {
          current_cols.push(col_name.to_string());
        }
      }
    }
    schemas.insert(table_name, current_cols);
  }

  schemas
}

pub fn parse_primary_keys_str(sql: &str) -> BTreeMap<String, Vec<String>> {
  let mut result = BTreeMap::new();

  for (table_name, body_inner) in parse_table_definitions(sql) {
    let mut keys = Vec::new();
    for part in split_top_level_comma(&body_inner) {
      let part = part.trim();
      if part.is_empty() {
        continue;
      }
      let mut words = part.split_whitespace();
      let first_word = words.next().unwrap_or("");
      let second_word = words.next().unwrap_or("");
      let is_named_pk_constraint = first_word.eq_ignore_ascii_case("CONSTRAINT") && has_primary_key_tokens(part);

      if is_direct_pk_constraint(first_word, second_word) || is_named_pk_constraint {
        if let Some(paren_idx) = part.find('(') {
          let after_paren = &part[paren_idx + 1..];
          if let Some((columns, _)) = after_paren.split_once(')') {
            for col in columns.split(',') {
              if let Some(col) = extract_first_identifier(col) {
                let col = col.trim_matches(['"', '`', '\'', '[', ']']);
                if !col.is_empty() {
                  keys.push(col.to_string());
                }
              }
            }
          }
        }
      } else if !is_other_table_constraint(first_word, second_word) && has_primary_key_tokens(part) {
        if let Some(col_name) = extract_first_identifier(part) {
          let col_name = col_name.trim_matches(['"', '`', '\'', '[', ']']);
          if !col_name.is_empty() {
            keys.push(col_name.to_string());
          }
        }
      }
    }
    result.insert(table_name, keys);
  }

  result
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_split_top_level_comma_nested_parentheses_and_composite_keys() {
    let input = "Id INTEGER PRIMARY KEY, Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5)), Message TEXT NOT NULL, PRIMARY KEY (Id, Level)";
    let parts = split_top_level_comma(input);
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0].trim(), "Id INTEGER PRIMARY KEY");
    assert_eq!(parts[1].trim(), "Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5))");
    assert_eq!(parts[2].trim(), "Message TEXT NOT NULL");
    assert_eq!(parts[3].trim(), "PRIMARY KEY (Id, Level)");
  }

  #[test]
  fn test_strip_sql_comments_block_comment_spacing() {
    let sql = "CREATE/*comment*/TABLE foo (id INT);";
    assert_eq!(strip_sql_comments(sql), "CREATE TABLE foo (id INT);");
  }

  #[test]
  fn test_parse_schema_and_primary_keys_composite_and_inline() {
    let sql = r#"
      -- PRIMARY KEY in comment
      CREATE TABLE IF NOT EXISTS test_table (
        id TEXT,
        trip_id TEXT,
        note TEXT DEFAULT 'PRIMARY KEY', -- comment: not a PRIMARY KEY
        status INTEGER,
        FOREIGN KEY (trip_id)
          REFERENCES trips (trip_id),
        PRIMARY KEY (
          id,
          trip_id
        )
      );
      CREATE TABLE IF NOT EXISTS single_pk (
        code TEXT PRIMARY KEY,
        description TEXT
      );
      CREATE TABLE IF NOT EXISTS no_pk (
        val1 TEXT,
        val2 TEXT
      );
    "#;
    let schemas = parse_schema_str(sql);
    assert_eq!(schemas.get("test_table"), Some(&vec!["id".to_string(), "trip_id".to_string(), "note".to_string(), "status".to_string()]));
    assert_eq!(schemas.get("single_pk"), Some(&vec!["code".to_string(), "description".to_string()]));

    let pks = parse_primary_keys_str(sql);
    assert_eq!(pks.get("test_table"), Some(&vec!["id".to_string(), "trip_id".to_string()]));
    assert_eq!(pks.get("single_pk"), Some(&vec!["code".to_string()]));
    assert_eq!(pks.get("no_pk"), Some(&vec![]));
  }

  #[test]
  fn test_parse_table_multiline_whitespace_and_quoted_names() {
    let sql = r#"
      CREATE
      TABLE  "bus routes" (
        "route id" TEXT PRIMARY KEY,
        route_name TEXT
      );
      create   table   [train schedules] (
        [schedule_id] TEXT PRIMARY KEY,
        dept_time TEXT
      );
    "#;
    let schemas = parse_schema_str(sql);
    assert_eq!(schemas.get("bus routes"), Some(&vec!["route id".to_string(), "route_name".to_string()]));
    assert_eq!(schemas.get("train schedules"), Some(&vec!["schedule_id".to_string(), "dept_time".to_string()]));

    let pks = parse_primary_keys_str(sql);
    assert_eq!(pks.get("bus routes"), Some(&vec!["route id".to_string()]));
    assert_eq!(pks.get("train schedules"), Some(&vec!["schedule_id".to_string()]));
  }

  #[test]
  fn test_parse_schema_check_and_constraint_prefixed_columns() {
    let sql = r#"
      CREATE TABLE IF NOT EXISTS test_check (
        check_id INTEGER PRIMARY KEY,
        checkpoint TEXT,
        unique_code TEXT,
        constraint_name TEXT,
        [bracket_col] INTEGER,
        status TINYINT CHECK (status IN (0, 1)),
        CHECK (check_id > 0)
      );
    "#;
    let schemas = parse_schema_str(sql);
    assert_eq!(
      schemas.get("test_check"),
      Some(&vec!["check_id".to_string(), "checkpoint".to_string(), "unique_code".to_string(), "constraint_name".to_string(), "bracket_col".to_string(), "status".to_string()])
    );
    let pks = parse_primary_keys_str(sql);
    assert_eq!(pks.get("test_check"), Some(&vec!["check_id".to_string()]));
  }

  #[test]
  fn test_parse_composite_primary_key_modifiers_and_utf8() {
    let sql = r#"
      CREATE TABLE IF NOT EXISTS trips_utf8 (
        "路线_id" TEXT,
        service_id TEXT,
        start_date INTEGER,
        end_date INTEGER,
        CONSTRAINT "pk_路线" PRIMARY KEY (service_id ASC, [start_date] DESC, "路线_id" COLLATE NOCASE)
      );
    "#;
    let schemas = parse_schema_str(sql);
    assert_eq!(schemas.get("trips_utf8"), Some(&vec!["路线_id".to_string(), "service_id".to_string(), "start_date".to_string(), "end_date".to_string()]));
    let pks = parse_primary_keys_str(sql);
    assert_eq!(pks.get("trips_utf8"), Some(&vec!["service_id".to_string(), "start_date".to_string(), "路线_id".to_string()]));
  }

  #[test]
  fn test_parse_schema_and_primary_keys_multiline_and_spacing() {
    let sql = r#"
      CREATE TABLE IF NOT EXISTS multiline_pk (
        service_id TEXT,
        date INTEGER,
        PRIMARY
          KEY (
            service_id,
            date
          )
      );
      CREATE TABLE IF NOT EXISTS inline_multiline_pk (
        trip_id TEXT PRIMARY
          KEY,
        direction_id INTEGER,
        FOREIGN
          KEY (direction_id) REFERENCES directions (direction_id)
      );
      CREATE TABLE IF NOT EXISTS no_space_table(
        id INTEGER PRIMARY KEY
      );
      CREATE TABLE IF NOT EXISTS "quoted (paren) table" (
        col1 TEXT PRIMARY KEY
      );
    "#;
    let schemas = parse_schema_str(sql);
    assert_eq!(schemas.get("multiline_pk"), Some(&vec!["service_id".to_string(), "date".to_string()]));
    assert_eq!(schemas.get("inline_multiline_pk"), Some(&vec!["trip_id".to_string(), "direction_id".to_string()]));
    assert_eq!(schemas.get("no_space_table"), Some(&vec!["id".to_string()]));
    assert_eq!(schemas.get("quoted (paren) table"), Some(&vec!["col1".to_string()]));

    let pks = parse_primary_keys_str(sql);
    assert_eq!(pks.get("multiline_pk"), Some(&vec!["service_id".to_string(), "date".to_string()]));
    assert_eq!(pks.get("inline_multiline_pk"), Some(&vec!["trip_id".to_string()]));
    assert_eq!(pks.get("no_space_table"), Some(&vec!["id".to_string()]));
    assert_eq!(pks.get("quoted (paren) table"), Some(&vec!["col1".to_string()]));
  }
}
