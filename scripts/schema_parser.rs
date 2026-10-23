use std::{collections::BTreeMap, fs, path::Path};

pub fn split_top_level_comma(s: &str) -> Vec<&str> {
  let mut parts = Vec::new();
  let mut start = 0;
  let mut depth = 0;
  for (idx, ch) in s.char_indices() {
    match ch {
      '(' => depth += 1,
      ')' => depth = depth.saturating_sub(1),
      ',' if depth == 0 => {
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

pub fn parse_schema_file(path: &Path) -> BTreeMap<String, Vec<String>> {
  let sql = fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
  let mut schemas = BTreeMap::new();
  let mut in_table = None;
  let mut current_cols = Vec::new();

  for line in sql.lines() {
    let line = line.trim();
    let upper_line = line.to_uppercase();
    if line.is_empty() || line.starts_with("--") {
      continue;
    }

    if upper_line.starts_with("CREATE TABLE") {
      let parts: Vec<&str> = line.split_whitespace().collect();
      let mut table_name = "";
      for (i, p) in parts.iter().enumerate() {
        if p.starts_with('(') {
          table_name = parts.get(i.saturating_sub(1)).unwrap_or(&"");
          break;
        }
        let p_upper = p.to_uppercase();
        if p_upper == "EXISTS" || (p_upper == "TABLE" && parts.get(i + 1).map(|s| s.to_uppercase()) != Some("IF".to_string())) {
          table_name = parts.get(i + 1).unwrap_or(&"");
        }
      }
      let table_name = table_name.trim_end_matches('(').to_string();
      if !table_name.is_empty() {
        in_table = Some(table_name.clone());
        current_cols.clear();

        if line.ends_with(");") {
          if let (Some(start), Some(end)) = (line.find('('), line.rfind(')'))
            && start < end
          {
            let cols_str = &line[start + 1..end];
            for col_def in split_top_level_comma(cols_str) {
              let col_def = col_def.trim();
              let col_upper = col_def.to_uppercase();
              if col_upper.starts_with("PRIMARY KEY")
                || col_upper.starts_with("FOREIGN KEY")
                || col_upper.starts_with("UNIQUE")
                || col_upper.starts_with("CHECK")
                || col_upper.starts_with("CONSTRAINT")
              {
                continue;
              }
              if let Some(col_name) = col_def.split_whitespace().next()
                && !col_name.is_empty()
              {
                current_cols.push(col_name.to_string());
              }
            }
          }
          schemas.insert(table_name, current_cols.clone());
          in_table = None;
        }
      }
    } else if in_table.is_some() {
      if line.starts_with(");") || line == ")" {
        if let Some(table) = in_table.take() {
          schemas.insert(table, current_cols.clone());
        }
      } else {
        if !upper_line.starts_with("PRIMARY KEY")
          && !upper_line.starts_with("FOREIGN KEY")
          && !upper_line.starts_with("UNIQUE")
          && !upper_line.starts_with("CHECK")
          && !upper_line.starts_with("CONSTRAINT")
          && let Some(col_name) = line.split_whitespace().next()
        {
          let col_name = col_name.trim_end_matches(',');
          if !col_name.is_empty() {
            current_cols.push(col_name.to_string());
          }
        }
      }
    }
  }
  schemas
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_split_top_level_comma_nested_parentheses() {
    let input = "Id INTEGER PRIMARY KEY, Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5)), Message TEXT NOT NULL";
    let parts = split_top_level_comma(input);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].trim(), "Id INTEGER PRIMARY KEY");
    assert_eq!(parts[1].trim(), "Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5))");
    assert_eq!(parts[2].trim(), "Message TEXT NOT NULL");
  }

  #[test]
  fn test_split_top_level_comma_composite_keys() {
    let input = "service_id TEXT, date INTEGER, exception_type INTEGER, PRIMARY KEY (service_id, date)";
    let parts = split_top_level_comma(input);
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[3].trim(), "PRIMARY KEY (service_id, date)");
  }
}
