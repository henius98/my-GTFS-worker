//! GTFS Worker — Cloudflare Worker API for GTFS static data.
//!
//! Note: Heavy GTFS import processing (ZIP download, CSV parsing) has been
//! moved to an external GitHub Actions workflow to respect Cloudflare Worker CPU limits.

use serde::{Deserialize, Serialize};
use worker::{event, Cache, Context, Env, Method, Request, Response, Result};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
struct ImportProgress {
  provider: Option<String>,
  file_name: Option<String>,
  #[serde(rename = "CRC")]
  crc: Option<String>,
  last_processed_line: Option<i64>,
  last_processed_byte: Option<i64>,
  status: Option<i64>,
  updated_at: Option<String>,
}

#[event(fetch)]
pub async fn fetch_route(req: Request, env: Env, ctx: Context) -> Result<Response> {
  console_error_panic_hook::set_once();
  let url = req.url()?;

  let path = url.path();
  let mut segments = path.trim_matches('/').split('/');

  match (segments.next(), segments.next(), segments.next()) {
    (Some(provider), Some("status"), None) => {
      if req.method() != Method::Get {
        return Response::error("Method Not Allowed", 405);
      }

      // Query strings, path casing, and a trailing slash do not alter status
      // output; canonicalize them so callers cannot create arbitrary cache keys.
      let mut cache_url = url.clone();
      cache_url.set_path(&format!("/{}/status", provider.to_ascii_lowercase()));
      cache_url.set_query(None);
      let cache_key = cache_url.to_string();
      let cache = Cache::default();
      if let Ok(Some(response)) = cache.get(&cache_key, false).await {
        return Ok(response);
      }

      let binding_name = format!("DB_{}", provider.to_uppercase().replace("-", "_"));
      let d1 = match env.d1(&binding_name) {
        Ok(db) => db,
        Err(_) => {
          return Response::error(format!("Provider '{}' not found or DB not bound", provider), 404);
        }
      };

      // Fetch detailed import progress
      let progress_results = match d1.prepare("SELECT Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt FROM import_progress").all().await {
        Ok(res) => res,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      // Deserialize directly into the response shape to avoid building an
      // intermediate dynamic JSON object tree on every cache miss.
      let details = match progress_results.results::<ImportProgress>() {
        Ok(results) => results,
        Err(e) => return Response::error(format!("Database response error: {}", e), 500),
      };

      let mut response = Response::from_json(&details)?;
      response.headers_mut().set("Cache-Control", "public, max-age=60")?;
      let cached_response = response.cloned()?;
      ctx.wait_until(async move {
        let _ = Cache::default().put(cache_key, cached_response).await;
      });
      Ok(response)
    }

    (Some(provider), Some("data"), Some(table_name)) => {
      let mut limit: u32 = url.query_pairs().find(|(k, _)| k == "limit").and_then(|(_, v)| v.parse().ok()).unwrap_or(100);
      limit = std::cmp::min(limit, 1000); // Enforce maximum limit of 1000
      let offset: u32 = url.query_pairs().find(|(k, _)| k == "offset").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);

      let binding_name = format!("DB_{}", provider.to_uppercase().replace("-", "_"));
      let d1 = match env.d1(&binding_name) {
        Ok(db) => db,
        Err(_) => return Response::error(format!("Provider '{}' not found or DB not bound", provider), 404),
      };

      // Validate table name to prevent SQL injection
      let check_query = "SELECT name FROM sqlite_master WHERE type='table' AND name=?1";
      let statement = match d1.prepare(check_query).bind(&[table_name.into()]) {
        Ok(stmt) => stmt,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      let table_exists = match statement.first::<serde_json::Value>(None).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      if !table_exists {
        return Response::error(format!("Table '{}' not found", table_name), 404);
      }

      // Get valid columns for the table
      let columns_query = format!("PRAGMA table_info(\"{}\")", table_name);
      let columns_statement = match d1.prepare(&columns_query).bind(&[]) {
        Ok(stmt) => stmt,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      let valid_columns: Vec<String> = match columns_statement.all().await {
        Ok(res) => {
          if let Ok(rows) = res.results::<serde_json::Value>() {
            rows.into_iter().filter_map(|row| row.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())).collect()
          } else {
            Vec::new()
          }
        }
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      // Parse 'include' and 'exclude' from url
      let mut selected_columns = "*".to_string();

      let include_cols: Option<Vec<String>> = url.query_pairs().find(|(k, _)| k == "include").map(|(_, v)| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());

      let exclude_cols: Option<Vec<String>> = url.query_pairs().find(|(k, _)| k == "exclude").map(|(_, v)| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());

      if let Some(cols) = include_cols {
        for col in &cols {
          if !valid_columns.contains(col) {
            return Response::error(format!("Invalid include column: {}", col), 400);
          }
        }
        selected_columns = cols.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
      } else if let Some(cols) = exclude_cols {
        let mut final_cols = Vec::new();
        for valid_col in &valid_columns {
          if !cols.contains(valid_col) {
            final_cols.push(valid_col.clone());
          }
        }
        for col in &cols {
          if !valid_columns.contains(col) {
            return Response::error(format!("Invalid exclude column: {}", col), 400);
          }
        }
        if final_cols.is_empty() {
          return Response::error("Cannot exclude all columns", 400);
        }
        selected_columns = final_cols.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
      }

      // Parse filters
      let mut filter_clauses = Vec::new();
      let mut filter_values = Vec::new();

      for (k, v) in url.query_pairs() {
        match k.as_ref() {
          "filter" | "ifilter" | "contains" | "icontains" => {
            let (operator, is_like, is_instr) = match k.as_ref() {
              "filter" => ("=", false, false),
              "ifilter" => ("COLLATE NOCASE =", false, false),
              "contains" => ("", false, true),
              "icontains" => ("LIKE", true, false),
              _ => unreachable!(),
            };
            for part in v.split(',') {
              let parts: Vec<&str> = part.rsplitn(2, '@').collect();
              if parts.len() != 2 {
                return Response::error(format!("Invalid {} format. Expected val@col", k), 400);
              }
              let col = parts[0];
              let val = parts[1];

              if !valid_columns.contains(&col.to_string()) {
                return Response::error(format!("Invalid filter column: {}", col), 400);
              }

              if is_instr {
                filter_clauses.push(format!("INSTR(\"{}\", ?{}) > 0", col, filter_clauses.len() + 1));
              } else {
                filter_clauses.push(format!("\"{}\" {} ?{}", col, operator, filter_clauses.len() + 1));
              }

              if is_like {
                filter_values.push(format!("%{}%", val));
              } else {
                filter_values.push(val.to_string());
              }
            }
          }
          "range" => {
            for part in v.split(',') {
              if let Some(bracket_start) = part.find('[') {
                if let Some(bracket_end) = part.find(']') {
                  let col = &part[..bracket_start];
                  let range_val = &part[bracket_start + 1..bracket_end];
                  let range_parts: Vec<&str> = range_val.split(':').collect();

                  if !valid_columns.contains(&col.to_string()) {
                    return Response::error(format!("Invalid range column: {}", col), 400);
                  }

                  if range_parts.len() == 2 {
                    let begin = range_parts[0];
                    let end = range_parts[1];

                    if !begin.is_empty() {
                      filter_clauses.push(format!("\"{}\" >= ?{}", col, filter_clauses.len() + 1));
                      filter_values.push(begin.to_string());
                    }
                    if !end.is_empty() {
                      filter_clauses.push(format!("\"{}\" <= ?{}", col, filter_clauses.len() + 1));
                      filter_values.push(end.to_string());
                    }
                  } else {
                    return Response::error(format!("Invalid range format for {}. Expected col[begin:end]", col), 400);
                  }
                }
              }
            }
          }
          _ => {}
        }
      }

      let mut sort_clauses = Vec::new();
      if let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "sort") {
        let cols: Vec<&str> = v.split(',').collect();
        for col in cols {
          let col = col.trim();
          if col.is_empty() {
            continue;
          }
          if col.starts_with('-') {
            let col_name = &col[1..];
            if !valid_columns.contains(&col_name.to_string()) {
              return Response::error(format!("Invalid sort column: {}", col_name), 400);
            }
            sort_clauses.push(format!("\"{}\" DESC", col_name));
          } else {
            if !valid_columns.contains(&col.to_string()) {
              return Response::error(format!("Invalid sort column: {}", col), 400);
            }
            sort_clauses.push(format!("\"{}\" ASC", col));
          }
        }
      }

      let where_clause = if filter_clauses.is_empty() { "".to_string() } else { format!("WHERE {}", filter_clauses.join(" AND ")) };

      let order_clause = if sort_clauses.is_empty() { "".to_string() } else { format!("ORDER BY {}", sort_clauses.join(", ")) };

      // Execute the paginated query
      let query = format!("SELECT {} FROM {} {} {} LIMIT ?{} OFFSET ?{}", selected_columns, table_name, where_clause, order_clause, filter_clauses.len() + 1, filter_clauses.len() + 2);

      let mut params: Vec<worker::wasm_bindgen::JsValue> = Vec::new();
      for v in filter_values {
        params.push(worker::wasm_bindgen::JsValue::from_str(&v));
      }
      params.push(worker::wasm_bindgen::JsValue::from_f64(limit as f64));
      params.push(worker::wasm_bindgen::JsValue::from_f64(offset as f64));

      let statement = match d1.prepare(&query).bind(&params) {
        Ok(stmt) => stmt,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      let data_results = match statement.all().await {
        Ok(res) => res,
        Err(e) => return Response::error(format!("Database error: {}", e), 500),
      };

      let mut details = Vec::new();
      if let Ok(results) = data_results.results::<serde_json::Value>() {
        details = results;
      }

      Response::from_json(&serde_json::json!({
          "data": details,
          "limit": limit,
          "offset": offset
      }))
    }

    (Some(""), None, None) | (None, None, None) => Response::ok("Worker is running. Use /<provider>/status to check GTFS import progress."),

    _ => Response::error("Not Found", 404),
  }
}
