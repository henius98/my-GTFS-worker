use worker::js_sys::{JSON, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen_futures::JsFuture;
use worker::{D1Database, D1PreparedStatement, Env, Response, Result};

pub fn for_provider(env: &Env, provider: &str) -> Result<D1Database> {
  env.d1(&binding_name(provider))
}

fn binding_name(provider: &str) -> String {
  format!("DB_{}", provider.to_ascii_uppercase().replace('-', "_"))
}

pub fn provider_not_found(provider: &str) -> Result<Response> {
  Response::error(format!("Provider '{}' not found or DB not bound", provider), 404)
}

pub async fn query_raw_results(statement: &D1PreparedStatement) -> Result<JsValue> {
  let data_results = JsFuture::from(statement.inner().all()?).await?;
  Ok(Reflect::get(&data_results, &JsValue::from_str("results"))?)
}

pub fn json_response_from_js(value: &JsValue) -> Result<Response> {
  let body = JSON::stringify(value)?.as_string().ok_or_else(|| worker::Error::RustError("Failed to serialize JSON response".into()))?;
  Ok(Response::builder().with_header("Content-Type", "application/json")?.fixed(body.into_bytes()))
}
