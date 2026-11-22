#![allow(dead_code)]

use std::{collections::BTreeMap, fs, path::Path};

pub const STAGING_TABLE_SUFFIX: &str = "__staging";

fn quote_sql_identifier(identifier: &str) -> String {
  format!("\"{}\"", identifier.replace('"', "\"\""))
}

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
          if let Some(escaped) = chars.next() {
            result.push(escaped);
          }
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
  for (open, close) in [('"', '"'), ('`', '`'), ('\'', '\''), ('[', ']')] {
    if let Some(rest) = s.strip_prefix(open) {
      return rest.find(close).map(|end| &s[..end + open.len_utf8() + close.len_utf8()]);
    }
  }
  let end = s.find(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',' || c == ';').unwrap_or(s.len());
  if end == 0 { None } else { Some(&s[..end]) }
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

pub fn split_sql_statements(sql: &str) -> Vec<String> {
  let clean_sql = strip_sql_comments(sql);
  split_top_level_semicolon(&clean_sql).into_iter().map(str::trim).filter(|statement| !statement.is_empty()).map(str::to_owned).collect()
}

pub fn parse_schema_file(path: &Path) -> BTreeMap<String, Vec<String>> {
  let sql = fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
  parse_schema_str(&sql)
}

pub fn parse_schema_and_primary_keys(sql: &str) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, Vec<String>>) {
  let mut schemas = BTreeMap::new();
  let mut primary_keys = BTreeMap::new();

  for (table_name, body_inner) in parse_table_definitions(sql) {
    let mut current_cols = Vec::new();
    let mut keys = Vec::new();

    for part in split_top_level_comma(&body_inner) {
      let part = part.trim();
      if part.is_empty() {
        continue;
      }
      let mut words = part.split_whitespace();
      let first_word = words.next().unwrap_or("");
      let second_word = words.next().unwrap_or("");
      let is_direct_pk = is_direct_pk_constraint(first_word, second_word);
      let is_other_constraint = is_other_table_constraint(first_word, second_word);
      let is_named_pk_constraint = first_word.eq_ignore_ascii_case("CONSTRAINT") && has_primary_key_tokens(part);

      if !is_direct_pk
        && !is_other_constraint
        && let Some(col_name) = extract_first_identifier(part)
      {
        let col_name = col_name.trim_matches(['"', '`', '\'', '[', ']']);
        if !col_name.is_empty() {
          current_cols.push(col_name.to_string());
        }
      }

      if is_direct_pk || is_named_pk_constraint {
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
      } else if !is_other_constraint
        && has_primary_key_tokens(part)
        && let Some(col_name) = extract_first_identifier(part)
      {
        let col_name = col_name.trim_matches(['"', '`', '\'', '[', ']']);
        if !col_name.is_empty() {
          keys.push(col_name.to_string());
        }
      }
    }

    schemas.insert(table_name.clone(), current_cols);
    primary_keys.insert(table_name, keys);
  }

  (schemas, primary_keys)
}

pub fn parse_schema_str(sql: &str) -> BTreeMap<String, Vec<String>> {
  parse_schema_and_primary_keys(sql).0
}

pub fn parse_primary_keys_str(sql: &str) -> BTreeMap<String, Vec<String>> {
  parse_schema_and_primary_keys(sql).1
}

pub fn parse_staging_create_statements(sql: &str, staging_suffix: &str) -> BTreeMap<String, String> {
  let mut statements = BTreeMap::new();

  for statement in split_sql_statements(sql) {
    let mut tokens = statement.split_whitespace();
    let is_create_table = tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("CREATE")) && tokens.next().is_some_and(|t| t.eq_ignore_ascii_case("TABLE"));
    if !is_create_table {
      continue;
    }

    let mut rest = statement.trim_start();
    for _ in 0..2 {
      if let Some(end) = rest.find(|c: char| c.is_whitespace()) {
        rest = rest[end..].trim_start();
      }
    }

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

    let suffix = &rest[raw_table.len()..];
    if !suffix.trim_start().starts_with('(') {
      continue;
    }

    let staging_name = format!("{table_name}{staging_suffix}");
    statements.insert(table_name, format!("CREATE TABLE {}{suffix}", quote_sql_identifier(&staging_name)));
  }

  statements
}

fn has_without_rowid_option(options: &str) -> bool {
  let clean = strip_sql_strings(options);
  let mut prev_without = false;
  for token in clean.split(|ch: char| ch.is_whitespace() || ch == ',') {
    if token.is_empty() {
      continue;
    }
    if prev_without && token.eq_ignore_ascii_case("ROWID") {
      return true;
    }
    prev_without = token.eq_ignore_ascii_case("WITHOUT");
  }
  false
}

fn body_has_unique_constraint(body: &str) -> bool {
  for part in split_top_level_comma(body) {
    let part = part.trim();
    if part.is_empty() {
      continue;
    }
    let first_word = part.split_whitespace().next().unwrap_or("");
    let remainder = if first_word.eq_ignore_ascii_case("UNIQUE") || first_word.to_ascii_uppercase().starts_with("UNIQUE(") {
      return true;
    } else if first_word.eq_ignore_ascii_case("CONSTRAINT") {
      let after_keyword = part[first_word.len()..].trim_start();
      if let Some(name) = extract_first_identifier(after_keyword) { &after_keyword[name.len()..] } else { after_keyword }
    } else if let Some(col_name) = extract_first_identifier(part) {
      &part[col_name.len()..]
    } else {
      part
    };
    let clean = strip_sql_strings(remainder);
    if clean.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_').any(|token| token.eq_ignore_ascii_case("UNIQUE")) {
      return true;
    }
  }
  false
}

pub fn staging_create_uses_single_btree(create_sql: &str) -> bool {
  let Some(open_paren_idx) = create_sql.find('(') else {
    return false;
  };
  let body = &create_sql[open_paren_idx + 1..];
  let mut depth = 1_usize;
  let mut in_str = false;
  let mut close_paren_idx = None;
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
          close_paren_idx = Some(idx);
          break;
        }
      }
      _ => {}
    }
  }
  let Some(body_end) = close_paren_idx else {
    return false;
  };
  let options = &body[body_end + 1..];
  has_without_rowid_option(options) && !body_has_unique_constraint(&body[..body_end])
}

pub fn parse_staging_writes_per_row(sql: &str, staging_suffix: &str) -> BTreeMap<String, u64> {
  parse_staging_create_statements(sql, staging_suffix)
    .into_iter()
    .map(|(table_name, create_sql)| {
      let writes = if staging_create_uses_single_btree(&create_sql) { 1 } else { 2 };
      (table_name, writes)
    })
    .collect()
}

#[cfg(test)]
#[path = "../test/unit/scripts/schema_parser.rs"]
mod tests;
