use worker::Env;

pub mod data;
pub mod sql;
pub mod status;

const DEFAULT_MAX_ROWS: u32 = 1000;

pub fn max_rows(env: &Env) -> u32 {
  env.var("WORKER_SQL_MAX_ROWS").ok().and_then(|value| value.to_string().trim().parse::<u32>().ok()).filter(|rows| *rows > 0).unwrap_or(DEFAULT_MAX_ROWS)
}
