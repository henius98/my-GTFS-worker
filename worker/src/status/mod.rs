use serde::{Deserialize, Serialize};
use worker::{Cache, Context, Env, Method, Request, Response, Result, Url};

use crate::database;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct ImportProgress {
  file_name: Option<String>,
  #[serde(rename = "CRC")]
  crc: Option<String>,
  last_processed_line: Option<i64>,
  last_processed_byte: Option<i64>,
  status: Option<i64>,
  updated_at: Option<String>,
}

fn canonical_cache_key(url: &Url, provider: &str) -> String {
  crate::canonical_cache_url(url, provider, "status").to_string()
}

pub async fn handle(req: &Request, env: &Env, ctx: Context, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return crate::method_not_allowed("GET", false);
  }

  let cache_control = crate::api_cache_control(env)?;

  // Query strings, path casing, and a trailing slash do not alter status
  // output; canonicalize them so callers cannot create arbitrary cache keys.
  let cache_key = canonical_cache_key(url, provider);
  if let Ok(Some(response)) = Cache::default().get(&cache_key, false).await {
    return Ok(response);
  }

  let d1 = match database::for_provider(env, provider, "status") {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // Fetch detailed import progress
  let progress_stmt = d1.prepare(include_str!("sql/import_progress.sql"));
  let progress_results = d1.all("import_progress", &progress_stmt).await?;
  if !progress_results.success() {
    return Err(worker::Error::RustError("Status query failed".into()));
  }

  // Deserialize directly into the response shape to avoid building an
  // intermediate dynamic JSON object tree on every cache miss.
  let details = progress_results.results::<ImportProgress>()?;

  let mut response = Response::from_json(&details)?;
  crate::cache_response(&ctx, cache_key, &mut response, &cache_control)?;
  Ok(response)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/status/mod.rs"]
mod tests;
