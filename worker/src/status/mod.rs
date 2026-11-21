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
  let mut cache_url = url.clone();
  cache_url.set_path(&format!("/{}/status", provider.to_ascii_lowercase()));
  cache_url.set_query(None);
  cache_url.to_string()
}

pub async fn handle(req: &Request, env: &Env, ctx: Context, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return Response::error(format!("{} Method Not Allowed", req.method()), 405);
  }

  let cache_control = crate::api_cache_control(env)?;

  // Query strings, path casing, and a trailing slash do not alter status
  // output; canonicalize them so callers cannot create arbitrary cache keys.
  let cache_key = canonical_cache_key(url, provider);
  let cache = Cache::default();
  if let Ok(Some(response)) = cache.get(&cache_key, false).await {
    return Ok(response);
  }

  let d1 = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // Fetch detailed import progress
  let progress_results = match d1.prepare(include_str!("sql/import_progress.sql")).all().await {
    Ok(res) => res,
    Err(e) if crate::is_d1_row_read_limit(&e.to_string()) => return crate::d1_row_read_limit_response(),
    Err(e) => return Response::error(format!("Database error: {}", e), 500),
  };

  // Deserialize directly into the response shape to avoid building an
  // intermediate dynamic JSON object tree on every cache miss.
  let details = match progress_results.results::<ImportProgress>() {
    Ok(results) => results,
    Err(e) => return Response::error(format!("Database response error: {}", e), 500),
  };

  let mut response = Response::from_json(&details)?;
  response.headers_mut().set("Cache-Control", &cache_control)?;
  let cached_response = response.cloned()?;
  ctx.wait_until(async move {
    let _ = Cache::default().put(cache_key, cached_response).await;
  });
  Ok(response)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/status/mod.rs"]
mod tests;
