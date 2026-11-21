use std::path::PathBuf;

#[path = "../../scripts/schema_parser.rs"]
pub mod schema_parser;

pub use importer::config::ProviderConfig as Provider;

pub type CheckResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub fn root() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

pub fn providers() -> CheckResult<Vec<Provider>> {
  Ok(importer::config::load_config(root().join("providers.toml"))?.providers)
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

pub fn median(mut samples: Vec<f64>) -> f64 {
  samples.sort_by(f64::total_cmp);
  let middle = samples.len() / 2;
  if samples.len().is_multiple_of(2) { (samples[middle - 1] + samples[middle]) / 2.0 } else { samples[middle] }
}
