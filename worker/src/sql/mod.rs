use worker::js_sys::{JSON, Object, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Env, Method, Request, Response, Result};

use crate::database;

pub async fn handle(req: &mut Request, env: &Env, provider: &str) -> Result<Response> {
  if req.method() != Method::Post {
    return Response::error("Method Not Allowed", 405);
  }

  let sql = req.text().await?;
  let sql = sql.trim();
  let sql = sql.strip_suffix(';').unwrap_or(sql).trim_end();
  if sql.is_empty() {
    return Response::error("SQL query is required", 400);
  }
  if !sql.split_whitespace().next().is_some_and(|keyword| keyword.eq_ignore_ascii_case("SELECT") || keyword.eq_ignore_ascii_case("WITH"))
    || sql.contains(';')
    || sql.contains("--")
    || sql.contains("/*")
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
