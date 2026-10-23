use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "../scripts/schema_parser.rs"]
mod schema_parser;
use schema_parser::parse_schema_file;

fn migration_sort_key(path: &Path) -> (u64, String) {
  let name = path.file_name().and_then(|value| value.to_str()).unwrap_or_default().to_owned();
  let sequence = name.split('_').next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(u64::MAX);
  (sequence, name)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  println!("cargo:rerun-if-changed=../migrations");
  println!("cargo:rerun-if-changed=../scripts/schema_parser.rs");
  let out_dir = env::var("OUT_DIR")?;
  let dest_path = PathBuf::from(&out_dir).join("schemas.rs");

  let mut generated_code = String::new();
  generated_code.push_str("match provider_name {\n");

  let migrations_dir = PathBuf::from("../migrations");
  let mut provider_dirs = if migrations_dir.exists() { fs::read_dir(&migrations_dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()? } else { Vec::new() };
  provider_dirs.retain(|path| path.is_dir());
  provider_dirs.sort();

  for path in &provider_dirs {
    if let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) {
      let schema_path = path.join("0_gtfs_schema.sql");
      if schema_path.exists() {
        let parsed = parse_schema_file(&schema_path);
        generated_code.push_str(&format!("    \"{}\" => Some(&[\n", provider_name));
        for (table, cols) in parsed {
          generated_code.push_str(&format!("        (\"{}\", &[\n", table));
          for col in cols {
            generated_code.push_str(&format!("            \"{}\",\n", col));
          }
          generated_code.push_str("        ]),\n");
        }
        generated_code.push_str("    ]),\n");
      }
    }
  }

  generated_code.push_str("    _ => None,\n");
  generated_code.push_str("}\n");

  fs::write(&dest_path, generated_code)?;

  let mut keys_code = String::from("match (provider_name, table_name) {\n");
  for path in &provider_dirs {
    let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    let schema_path = path.join("0_gtfs_schema.sql");
    if !schema_path.exists() {
      continue;
    }
    let sql = fs::read_to_string(schema_path)?;
    for statement in sql.split(';') {
      let Some((_, definition)) = statement.split_once("CREATE TABLE IF NOT EXISTS ") else {
        continue;
      };
      let Some((table, body)) = definition.split_once('(') else {
        continue;
      };
      let mut keys = Vec::new();
      if let Some((_, primary)) = body.split_once("PRIMARY KEY (") {
        if let Some((columns, _)) = primary.split_once(')') {
          keys.extend(columns.split(',').map(str::trim));
        }
      } else {
        for line in body.lines() {
          if line.contains("PRIMARY KEY")
            && let Some(column) = line.split_whitespace().next()
          {
            keys.push(column);
          }
        }
      }
      keys_code.push_str(&format!("    ({provider_name:?}, {:?}) => &{keys:?},\n", table.trim()));
    }
  }
  keys_code.push_str("    _ => &[],\n}\n");
  fs::write(PathBuf::from(&out_dir).join("primary_keys.rs"), keys_code)?;

  // Generate schema_sql.rs to embed the full SQL content for D1 database rotation
  let sql_dest_path = PathBuf::from(&out_dir).join("schema_sql.rs");
  let mut sql_code = String::new();
  sql_code.push_str("match provider_name {\n");

  for path in &provider_dirs {
    if let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) {
      let mut sql_files = Vec::new();
      for entry in fs::read_dir(path)? {
        let sql_path = entry?.path();
        if sql_path.is_file() && sql_path.extension().is_some_and(|extension| extension == "sql") {
          sql_files.push(sql_path);
        }
      }

      sql_files.sort_by_cached_key(|path| migration_sort_key(path));

      if !sql_files.is_empty() {
        sql_code.push_str(&format!("    \"{}\" => Some(&[\n", provider_name));
        for schema_path in sql_files {
          let sql = fs::read_to_string(&schema_path)?;
          sql_code.push_str(&format!("        r#\"{}\"#,\n", sql));
        }
        sql_code.push_str("    ]),\n");
      }
    }
  }

  sql_code.push_str("    _ => None,\n");
  sql_code.push_str("}\n");

  fs::write(&sql_dest_path, sql_code)?;
  Ok(())
}
