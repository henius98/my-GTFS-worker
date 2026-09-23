use serde::Deserialize;
use worker::js_sys::{JSON, Object, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Env, Response, Result, Url};

use crate::database;

#[derive(Deserialize)]
struct ColumnName {
  name: String,
}

pub async fn handle(env: &Env, url: &Url, provider: &str, table_name: &str) -> Result<Response> {
  let mut limit: u32 = url.query_pairs().find(|(k, _)| k == "limit").and_then(|(_, v)| v.parse().ok()).unwrap_or(100);
  limit = std::cmp::min(limit, 1000); // Enforce maximum limit of 1000
  let offset: u32 = url.query_pairs().find(|(k, _)| k == "offset").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);

  let d1 = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // Validate table name to prevent SQL injection
  let check_query = "SELECT name FROM sqlite_master WHERE type='table' AND name=?1";
  let statement = match d1.prepare(check_query).bind(&[table_name.into()]) {
    Ok(stmt) => stmt,
    Err(e) => return Response::error(format!("Database error: {}", e), 500),
  };

  let table_exists = match statement.first::<String>(Some("name")).await {
    Ok(Some(_)) => true,
    Ok(None) => false,
    Err(e) => return Response::error(format!("Database error: {}", e), 500),
  };

  if !table_exists {
    return Response::error(format!("Table '{}' not found", table_name), 404);
  }

  // Get valid columns for the table
  let columns_statement = match d1.prepare("SELECT name FROM pragma_table_info(?1)").bind(&[table_name.into()]) {
    Ok(stmt) => stmt,
    Err(e) => return Response::error(format!("Database error: {}", e), 500),
  };

  let valid_columns: Vec<String> = match columns_statement.all().await {
    Ok(res) => {
      if let Ok(rows) = res.results::<ColumnName>() {
        rows.into_iter().map(|row| row.name).collect()
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
          if let Some(bracket_start) = part.find('[')
            && let Some(bracket_end) = part.find(']')
          {
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
      if let Some(col_name) = col.strip_prefix('-') {
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

  let data_results = JsFuture::from(statement.inner().all()?).await?;
  let details = Reflect::get(&data_results, &JsValue::from_str("results"))?;
  let response_data = Object::new();
  Reflect::set(&response_data, &JsValue::from_str("data"), &details)?;
  Reflect::set(&response_data, &JsValue::from_str("limit"), &JsValue::from_f64(limit as f64))?;
  Reflect::set(&response_data, &JsValue::from_str("offset"), &JsValue::from_f64(offset as f64))?;

  let body = JSON::stringify(&response_data)?.as_string().ok_or_else(|| worker::Error::RustError("Failed to serialize data response".into()))?;
  let mut response = Response::ok(body)?;
  response.headers_mut().set("Content-Type", "application/json")?;
  Ok(response)
}
