use serde::Deserialize;
use std::{fs, path::PathBuf};

pub type CheckResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
pub struct Provider {
  pub name: String,
  #[serde(default)]
  pub is_active: bool,
  pub static_url: String,
  pub static_provider: String,
}

#[derive(Deserialize)]
struct ProviderFile {
  providers: Vec<Provider>,
}

pub fn root() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

pub fn providers() -> CheckResult<Vec<Provider>> {
  Ok(toml::from_str::<ProviderFile>(&fs::read_to_string(root().join("providers.toml"))?)?.providers)
}

pub fn benchmark_options(default_rows: usize, default_iterations: usize) -> CheckResult<(usize, usize)> {
  let mut args = std::env::args().skip(1);
  let mut rows = default_rows;
  let mut iterations = default_iterations;
  while let Some(arg) = args.next() {
    let value: usize = args.next().ok_or_else(|| format!("missing value for {arg}"))?.parse()?;
    if value == 0 {
      return Err(format!("{arg} must be positive").into());
    }
    match arg.as_str() {
      "--rows" => rows = value,
      "--iterations" => iterations = value,
      _ => return Err(format!("unknown argument: {arg}").into()),
    }
  }
  Ok((rows, iterations))
}
