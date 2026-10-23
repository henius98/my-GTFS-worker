use std::{
  collections::BTreeMap,
  fs,
  path::Path,
};

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
            let mut in_constraint = false;
            for col_def in cols_str.split(',') {
              let col_def = col_def.trim();
              let col_upper = col_def.to_uppercase();
              if in_constraint {
                if col_def.contains(')') {
                  in_constraint = false;
                }
                continue;
              }
              if col_upper.starts_with("PRIMARY KEY")
                || col_upper.starts_with("FOREIGN KEY")
                || col_upper.starts_with("UNIQUE")
                || col_upper.starts_with("CHECK")
                || col_upper.starts_with("CONSTRAINT")
              {
                if col_def.contains('(') && !col_def.contains(')') {
                  in_constraint = true;
                }
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
