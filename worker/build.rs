use std::{env, error::Error, fs, path::PathBuf};

#[path = "../scripts/schema_parser.rs"]
mod schema_parser;
use schema_parser::parse_schema_file;

fn main() -> Result<(), Box<dyn Error>> {
  println!("cargo:rerun-if-changed=../migrations");
  println!("cargo:rerun-if-changed=../scripts/schema_parser.rs");
  println!("cargo:rerun-if-changed=../scripts/sql_strings.rs");
  let out_dir = PathBuf::from(env::var("OUT_DIR")?);

  let migrations_dir = PathBuf::from("../migrations");
  let mut provider_dirs = if migrations_dir.exists() { fs::read_dir(&migrations_dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()? } else { Vec::new() };
  provider_dirs.retain(|path| path.is_dir());
  provider_dirs.sort();

  let mut code = String::from("type TableSchema = (&'static str, &'static [&'static str]);\ntype ProviderSchema = (&'static str, &'static [TableSchema]);\n\nconst SCHEMAS: &[ProviderSchema] = &[\n");
  for path in &provider_dirs {
    println!("cargo:rerun-if-changed={}", path.display());
    let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    let schema_path = path.join("0_gtfs_schema.sql");
    if schema_path.exists() {
      println!("cargo:rerun-if-changed={}", schema_path.display());
      code.push_str(&format!("  (\"{}\", &[\n", provider_name.to_ascii_lowercase()));
      for (table, cols) in parse_schema_file(&schema_path) {
        code.push_str(&format!("    (\"{}\", &[", table.to_ascii_lowercase()));
        for col in &cols {
          code.push_str(&format!("\"{}\", ", col));
        }
        code.push_str("]),\n");
      }
      code.push_str("  ]),\n");
    }
  }
  code.push_str(
    "];\n\npub fn table_columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {\n  SCHEMAS.iter().find(|(p, _)| p.eq_ignore_ascii_case(provider)).and_then(|(_, tables)| tables.iter().find(|(t, _)| t.eq_ignore_ascii_case(table))).map(|(_, cols)| *cols)\n}\n",
  );

  fs::write(out_dir.join("schemas.rs"), code)?;
  Ok(())
}
