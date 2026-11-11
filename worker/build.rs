use std::{env, error::Error, fs, path::PathBuf};

#[path = "../scripts/schema_parser.rs"]
mod schema_parser;
use schema_parser::parse_schema_file;

fn main() -> Result<(), Box<dyn Error>> {
  println!("cargo:rerun-if-changed=../migrations");
  println!("cargo:rerun-if-changed=../scripts/schema_parser.rs");
  let out_dir = PathBuf::from(env::var("OUT_DIR")?);

  let migrations_dir = PathBuf::from("../migrations");
  let mut provider_dirs = if migrations_dir.exists() { fs::read_dir(&migrations_dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()? } else { Vec::new() };
  provider_dirs.retain(|path| path.is_dir());
  provider_dirs.sort();

  let mut code = String::from("pub fn table_columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {\n  match (provider, table) {\n");
  for path in &provider_dirs {
    let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    let schema_path = path.join("0_gtfs_schema.sql");
    if schema_path.exists() {
      for (table, cols) in parse_schema_file(&schema_path) {
        code.push_str(&format!("    (\"{}\", \"{}\") => Some(&[\n", provider_name.to_ascii_lowercase(), table.to_ascii_lowercase()));
        for col in &cols {
          code.push_str(&format!("      \"{}\",\n", col));
        }
        code.push_str("    ]),\n");
      }
    }
  }
  code.push_str("    _ => None,\n  }\n}\n");

  fs::write(out_dir.join("schemas.rs"), code)?;
  Ok(())
}
