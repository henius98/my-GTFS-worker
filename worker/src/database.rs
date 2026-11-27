use serde::Serialize;
use worker::d1::serde_wasm_bindgen;
use worker::js_sys::{JSON, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen_futures::JsFuture;
use worker::{D1Database, D1PreparedStatement, D1Result, D1ResultMeta, Env, Response, Result};

pub struct ProviderDb {
  db: D1Database,
  provider: String,
  database: String,
  route: &'static str,
}

#[derive(Debug, Serialize)]
struct D1QueryLog<'a> {
  event: &'static str,
  provider: &'a str,
  database: &'a str,
  route: &'a str,
  query: &'a str,
  success: bool,
  rows_read: Option<usize>,
  rows_written: Option<usize>,
  duration_ms: Option<f64>,
  changes: Option<usize>,
  changed_db: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  size_after: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  last_row_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<&'a str>,
}

fn format_d1_query_log(provider: &str, database: &str, route: &str, query: &str, success: bool, meta: Option<&D1ResultMeta>, error: Option<&str>) -> Option<String> {
  let record = D1QueryLog {
    event: "d1_query",
    provider,
    database,
    route,
    query,
    success,
    rows_read: meta.and_then(|m| m.rows_read),
    rows_written: meta.and_then(|m| m.rows_written),
    duration_ms: meta.and_then(|m| m.duration),
    changes: meta.and_then(|m| m.changes),
    changed_db: meta.and_then(|m| m.changed_db),
    size_after: meta.and_then(|m| m.size_after),
    last_row_id: meta.and_then(|m| m.last_row_id),
    error,
  };
  serde_json::to_string(&record).ok()
}

fn emit_d1_query_log(provider: &str, database: &str, route: &str, query: &str, success: bool, meta: Option<&D1ResultMeta>, error: Option<&str>) {
  let Some(line) = format_d1_query_log(provider, database, route, query, success, meta, error) else {
    return;
  };
  // Pass a plain JS object so Cloudflare Workers Persistent Logs indexes every field natively.
  let value = JSON::parse(&line).unwrap_or_else(|_| JsValue::from_str(&line));
  if success {
    worker::web_sys::console::log_1(&value);
  } else {
    worker::web_sys::console::warn_1(&value);
  }
}

impl ProviderDb {
  pub fn prepare<T: Into<String>>(&self, query: T) -> D1PreparedStatement {
    self.db.prepare(query)
  }

  fn log_result(&self, query: &str, result: &D1Result) {
    let success = result.success();
    let meta = result.meta().ok().flatten();
    let error = result.error();
    emit_d1_query_log(&self.provider, &self.database, self.route, query, success, meta.as_ref(), error.as_deref());
  }

  fn log_error(&self, query: &str, err: &worker::Error) {
    let message = err.to_string();
    emit_d1_query_log(&self.provider, &self.database, self.route, query, false, None, Some(&message));
  }

  pub async fn all(&self, query: &str, statement: &D1PreparedStatement) -> Result<D1Result> {
    match statement.all().await {
      Ok(result) => {
        self.log_result(query, &result);
        Ok(result)
      }
      Err(err) => {
        self.log_error(query, &err);
        Err(err)
      }
    }
  }

  pub async fn batch(&self, statements: Vec<(&str, D1PreparedStatement)>) -> Result<Vec<D1Result>> {
    let (labels, stmts): (Vec<&str>, Vec<D1PreparedStatement>) = statements.into_iter().unzip();
    match self.db.batch(stmts).await {
      Ok(results) => {
        for (idx, result) in results.iter().enumerate() {
          let label = labels.get(idx).copied().unwrap_or("batch_item");
          self.log_result(label, result);
        }
        Ok(results)
      }
      Err(err) => {
        let label = labels.join(",");
        self.log_error(&label, &err);
        Err(err)
      }
    }
  }

  pub async fn query_raw_results(&self, query: &str, statement: &D1PreparedStatement) -> Result<JsValue> {
    let promise = match statement.inner().all() {
      Ok(promise) => promise,
      Err(err) => {
        let err = worker::Error::from(err);
        self.log_error(query, &err);
        return Err(err);
      }
    };
    let data_results = match JsFuture::from(promise).await {
      Ok(data_results) => data_results,
      Err(err) => {
        let err = worker::Error::from(err);
        self.log_error(query, &err);
        return Err(err);
      }
    };
    let success = Reflect::get(&data_results, &JsValue::from_str("success")).ok().and_then(|v| v.as_bool()).unwrap_or(true);
    let error = Reflect::get(&data_results, &JsValue::from_str("error")).ok().and_then(|v| v.as_string());
    let meta = Reflect::get(&data_results, &JsValue::from_str("meta")).ok().filter(|v| !v.is_null() && !v.is_undefined()).and_then(|v| serde_wasm_bindgen::from_value::<D1ResultMeta>(v).ok());
    emit_d1_query_log(&self.provider, &self.database, self.route, query, success, meta.as_ref(), error.as_deref());
    Ok(Reflect::get(&data_results, &JsValue::from_str("results"))?)
  }
}

pub fn for_provider(env: &Env, provider: &str, route: &'static str) -> Result<ProviderDb> {
  let database = binding_name(provider);
  let db = env.d1(&database)?;
  Ok(ProviderDb { db, provider: provider.to_ascii_lowercase(), database, route })
}

fn binding_name(provider: &str) -> String {
  format!("DB_{}", provider.to_ascii_uppercase().replace('-', "_"))
}

pub fn provider_not_found(provider: &str) -> Result<Response> {
  Response::error(format!("Provider '{}' not found or DB not bound", provider), 404)
}

pub fn json_response_from_js(value: &JsValue) -> Result<Response> {
  let body = JSON::stringify(value)?.as_string().ok_or_else(|| worker::Error::RustError("Failed to serialize JSON response".into()))?;
  Ok(Response::builder().with_header("Content-Type", "application/json")?.fixed(body.into_bytes()))
}

#[cfg(test)]
#[path = "../../test/unit/worker/database.rs"]
mod tests;
