use worker::js_sys::{Object, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::{Env, Method, Request, Response, Result};

use crate::database;

#[path = "../../../scripts/sql_strings.rs"]
mod sql_strings;
use sql_strings::strip_sql_strings;

const QUERY_SQL: &str = include_str!("query.sql");

fn build_wrapped_query(sql: &str, max_rows: u32) -> String {
  QUERY_SQL.trim_end().replace("__LIMIT__", &max_rows.to_string()).replace("__SQL__", sql)
}

fn extract_bearer_token(header: &str) -> Option<&str> {
  let (scheme, rest) = header.trim().split_once(' ')?;
  scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
  let (a, b) = (a.as_bytes(), b.as_bytes());
  let mut diff = a.len() ^ b.len();
  for (&x, &y) in a.iter().zip(b.iter()) {
    diff |= usize::from(x ^ y);
  }
  diff == 0
}

pub async fn handle(req: &mut Request, env: &Env, provider: &str) -> Result<Response> {
  if req.method() != Method::Post {
    return crate::method_not_allowed("POST", false);
  }

  let admin_key = env.secret("WORKER_ADMIN_KEY").map(|s| s.to_string()).or_else(|_| env.var("WORKER_ADMIN_KEY").map(|v| v.to_string())).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());

  let is_test = env.var("WORKER_TEST_NOW").is_ok() || env.var("WORKER_ALLOW_ANONYMOUS_SQL").is_ok_and(|v| v.to_string() == "true");

  match admin_key {
    Some(expected_key) => {
      let auth_header = req.headers().get("Authorization").ok().flatten();
      let bearer_matches = auth_header.as_deref().and_then(extract_bearer_token).is_some_and(|token| constant_time_eq(token, &expected_key));
      let x_admin_key = req.headers().get("X-Admin-Key").ok().flatten();
      let x_key_matches = x_admin_key.as_deref().map(str::trim).is_some_and(|key| constant_time_eq(key, &expected_key));

      if !bearer_matches && !x_key_matches {
        return Response::error("Unauthorized", 401);
      }
    }
    None if !is_test => {
      return Response::error("Forbidden: WORKER_ADMIN_KEY is not configured", 403);
    }
    None => {}
  }

  let sql = req.text().await?;
  let sql = sql.trim();
  let sql = sql.strip_suffix(';').unwrap_or(sql).trim_end();
  if sql.is_empty() {
    return Response::error("SQL query is required", 400);
  }
  let sql_without_strings = strip_sql_strings(sql);
  if !sql.split_whitespace().next().is_some_and(|keyword| keyword.eq_ignore_ascii_case("SELECT") || keyword.eq_ignore_ascii_case("WITH"))
    || sql_without_strings.contains(';')
    || sql_without_strings.contains("--")
    || sql_without_strings.contains("/*")
  {
    return Response::error("Only one SELECT query without comments is allowed", 400);
  }

  let d1 = match database::for_provider(env, provider, "sql") {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // A subquery accepts SELECT statements (including WITH ... SELECT) but rejects writes. Cap returned rows as in /data.
  let query = build_wrapped_query(sql, super::max_rows(env)?);
  let statement = d1.prepare(query);
  let details = match d1.query_raw_results("sql", &statement).await {
    Ok(details) => details,
    Err(err) if crate::is_d1_row_read_limit_error(&err) => return Err(err),
    Err(err) => return Response::error(format!("Query error: {err}"), 400),
  };

  let response_data = Object::new();
  Reflect::set(&response_data, &JsValue::from_str("data"), &details)?;
  database::json_response_from_js(&response_data)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/sql/mod.rs"]
mod tests;
