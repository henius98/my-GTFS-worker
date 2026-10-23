use std::{
  env,
  error::Error,
  fs,
  path::{Path, PathBuf},
  process::Command,
};

#[path = "../scripts/schema_parser.rs"]
mod schema_parser;
use schema_parser::parse_schema_file;

fn watch_git_path(manifest_dir: &str, path: &str) {
  if let Ok(output) = Command::new("git").args(["rev-parse", "--path-format=absolute", "--git-path", path]).current_dir(manifest_dir).output()
    && output.status.success()
    && let Ok(git_path) = String::from_utf8(output.stdout)
  {
    let trimmed = git_path.trim();
    if !trimmed.is_empty() {
      println!("cargo:rerun-if-changed={}", trimmed);
    }
  }
}

fn get_commit_sha(manifest_dir: &str) -> String {
  for env_key in ["CF_PAGES_COMMIT_SHA", "GIT_SHA"] {
    if let Ok(sha) = env::var(env_key) {
      let sha = sha.trim();
      if sha.len() >= 6 && sha[..6].bytes().all(|b| b.is_ascii_hexdigit()) {
        return sha[..6].to_string();
      }
    }
  }
  if let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(manifest_dir).output()
    && output.status.success()
    && let Ok(sha) = String::from_utf8(output.stdout)
  {
    let sha = sha.trim();
    if sha.len() >= 6 && sha[..6].bytes().all(|b| b.is_ascii_hexdigit()) {
      return sha[..6].to_string();
    }
  }
  "000000".to_string()
}

fn main() -> Result<(), Box<dyn Error>> {
  println!("cargo:rerun-if-changed=../migrations");
  println!("cargo:rerun-if-changed=../scripts/schema_parser.rs");
  let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
  let out_dir = env::var("OUT_DIR")?;
  let version = get_commit_sha(&manifest_dir);
  println!("cargo:rustc-env=SCHEMA_CACHE_VERSION={version}");

  watch_git_path(&manifest_dir, "HEAD");
  watch_git_path(&manifest_dir, "packed-refs");
  if let Ok(reference) = Command::new("git").args(["symbolic-ref", "HEAD"]).current_dir(&manifest_dir).output()
    && reference.status.success()
    && let Ok(ref_name) = String::from_utf8(reference.stdout)
  {
    watch_git_path(&manifest_dir, ref_name.trim());
  }

  // Generate schemas.rs
  let migrations_dir = PathBuf::from("../migrations");
  let mut provider_dirs = if migrations_dir.exists() { fs::read_dir(&migrations_dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()? } else { Vec::new() };
  provider_dirs.retain(|path| path.is_dir());
  provider_dirs.sort();

  let dest_path = PathBuf::from(&out_dir).join("schemas.rs");
  let mut code = String::new();

  code.push_str("pub fn table_columns(provider: &str, table: &str) -> Option<&'static [&'static str]> {\n");
  code.push_str("  match (provider, table) {\n");
  for path in &provider_dirs {
    if let Some(provider_name) = path.file_name().and_then(|name| name.to_str()) {
      let schema_path = path.join("0_gtfs_schema.sql");
      if schema_path.exists() {
        let parsed = parse_schema_file(&schema_path);
        for (table, cols) in parsed {
          code.push_str(&format!("    (\"{}\", \"{}\") => Some(&[\n", provider_name, table));
          for col in cols {
            code.push_str(&format!("      \"{}\",\n", col));
          }
          code.push_str("    ]),\n");
        }
      }
    }
  }
  code.push_str("    _ => None,\n");
  code.push_str("  }\n");
  code.push_str("}\n\n");

  code.push_str("pub fn has_table_column(provider: &str, table: &str, column: &str) -> bool {\n");
  code.push_str("  table_columns(provider, table).is_some_and(|cols| cols.contains(&column))\n");
  code.push_str("}\n");

  fs::write(&dest_path, code)?;
  Ok(())
}
