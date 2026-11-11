use worker::js_sys::{JSON, Object, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Env, Method, Request, Response, Result};

use crate::database;

pub async fn handle(req: &mut Request, env: &Env, provider: &str) -> Result<Response> {
  if req.method() != Method::Post {
    return Response::error("Method Not Allowed", 405);
  }

  let admin_key = env.secret("WORKER_ADMIN_KEY").map(|s| s.to_string()).or_else(|_| env.var("WORKER_ADMIN_KEY").map(|v| v.to_string())).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());

  let is_test = env.var("WORKER_TEST_NOW").is_ok() || env.var("WORKER_ALLOW_ANONYMOUS_SQL").is_ok_and(|v| v.to_string() == "true");

  match admin_key {
    Some(expected_key) => {
      let auth_header = req.headers().get("Authorization").ok().flatten();
      let bearer = auth_header.as_deref().and_then(|h| {
        let trimmed = h.trim();
        if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") { Some(trimmed[7..].trim()) } else { None }
      });
      let x_admin_key = req.headers().get("X-Admin-Key").ok().flatten();
      let x_key = x_admin_key.as_deref().map(str::trim);

      if bearer != Some(&expected_key) && x_key != Some(&expected_key) {
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

  let d1 = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // A subquery accepts SELECT statements (including WITH ... SELECT) but rejects writes. Cap returned rows as in /data.
  let query = format!("SELECT * FROM (\n{}\n) LIMIT {}", sql, super::max_rows(env));
  let statement = d1.prepare(query);
  let data_results = match statement.inner().all() {
    Ok(promise) => match JsFuture::from(promise).await {
      Ok(result) => result,
      Err(error) => return Response::error(format!("Query error: {:?}", error), 400),
    },
    Err(error) => return Response::error(format!("Query error: {:?}", error), 400),
  };

  let details = Reflect::get(&data_results, &JsValue::from_str("results"))?;
  let response_data = Object::new();
  Reflect::set(&response_data, &JsValue::from_str("data"), &details)?;
  let body = JSON::stringify(&response_data)?.as_string().ok_or_else(|| worker::Error::RustError("Failed to serialize SQL response".into()))?;
  let mut response = Response::ok(body)?;
  response.headers_mut().set("Content-Type", "application/json")?;
  Ok(response)
}

fn strip_sql_strings(s: &str) -> String {
  let mut result = String::new();
  let mut in_string = false;
  let mut chars = s.chars().peekable();
  while let Some(ch) = chars.next() {
    match ch {
      '\'' if !in_string => {
        in_string = true;
        result.push(' ');
      }
      '\'' if in_string => {
        if chars.peek() == Some(&'\'') {
          chars.next();
          result.push(' ');
          result.push(' ');
        } else {
          in_string = false;
          result.push(' ');
        }
      }
      _ if in_string => result.push(' '),
      _ => result.push(ch),
    }
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_strip_sql_strings_replaces_literals_with_whitespace() {
    let sql = "SELECT * FROM routes WHERE desc = 'Express -- Non-stop; /* test */'";
    let stripped = strip_sql_strings(sql);
    assert!(!stripped.contains("--"));
    assert!(!stripped.contains(';'));
    assert!(!stripped.contains("/*"));
    assert!(stripped.starts_with("SELECT * FROM routes WHERE desc = "));
  }

  #[test]
  fn test_strip_sql_strings_escaped_quotes() {
    let sql = "SELECT * FROM routes WHERE desc = 'it''s a -- test'";
    let stripped = strip_sql_strings(sql);
    assert!(!stripped.contains("--"));
  }

  #[test]
  fn test_delimiters_outside_string_literals_detected() {
    let sql = "SELECT * FROM routes WHERE desc = 'safe'; DROP TABLE routes; --";
    let stripped = strip_sql_strings(sql);
    assert!(stripped.contains(';'));
    assert!(stripped.contains("--"));
  }
}
