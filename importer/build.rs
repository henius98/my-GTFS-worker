use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "../scripts/schema_parser.rs"]
mod schema_parser;
use schema_parser::{parse_primary_keys_str, parse_schema_str};

fn migration_sort_key(path: &Path) -> (u64, String) {
  let name = path.file_name().and_then(|value| value.to_str()).unwrap_or_default().to_owned();
  let sequence = name.split('_').next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(u64::MAX);
  (sequence, name)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  println!("cargo:rerun-if-changed=../migrations");
  println!("cargo:rerun-if-changed=../scripts/schema_parser.rs");
  let out_dir = PathBuf::from(env::var("OUT_DIR")?);

  let migrations_dir = PathBuf::from("../migrations");
  let mut provider_dirs = if migrations_dir.exists() { fs::read_dir(&migrations_dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()? } else { Vec::new() };
  provider_dirs.retain(|path| path.is_dir());
  provider_dirs.sort();

  let mut generated_code = String::from("match provider_name {\n");
  let mut keys_code = String::from("match (provider_name, table_name) {\n");
  let mut sql_code = String::from("match provider_name {\n");

  for path in &provider_dirs {
    let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    let schema_path = path.join("0_gtfs_schema.sql");
    if schema_path.exists() {
      let sql = fs::read_to_string(&schema_path)?;
      generated_code.push_str(&format!("    \"{}\" => Some(&[\n", provider_name));
      for (table, cols) in parse_schema_str(&sql) {
        generated_code.push_str(&format!("        (\"{}\", &[\n", table.to_ascii_lowercase()));
        for col in cols {
          generated_code.push_str(&format!("            \"{}\",\n", col));
        }
        generated_code.push_str("        ]),\n");
      }
      generated_code.push_str("    ]),\n");

      for (table_name, keys) in parse_primary_keys_str(&sql) {
        let lower = table_name.to_ascii_lowercase();
        keys_code.push_str(&format!("    ({provider_name:?}, {lower:?}) => &{keys:?},\n"));
      }
    }

    let mut sql_files = fs::read_dir(path)?
      .filter_map(|entry| entry.ok().map(|e| e.path()))
      .filter(|sql_path| sql_path.is_file() && sql_path.extension().is_some_and(|extension| extension == "sql"))
      .collect::<Vec<_>>();
    sql_files.sort_by_cached_key(|path| migration_sort_key(path));

    if !sql_files.is_empty() {
      sql_code.push_str(&format!("    \"{}\" => Some(&[\n", provider_name));
      for sql_path in sql_files {
        let sql = fs::read_to_string(&sql_path)?;
        sql_code.push_str(&format!("        r#\"{}\"#,\n", sql));
      }
      sql_code.push_str("    ]),\n");
    }
  }

  generated_code.push_str("    _ => None,\n}\n");
  keys_code.push_str("    _ => &[],\n}\n");
  sql_code.push_str("    _ => None,\n}\n");

  fs::write(out_dir.join("schemas.rs"), generated_code)?;
  fs::write(out_dir.join("primary_keys.rs"), keys_code)?;
  fs::write(out_dir.join("schema_sql.rs"), sql_code)?;
  Ok(())
}
