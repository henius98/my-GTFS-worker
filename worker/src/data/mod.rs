use worker::js_sys::{JSON, Object, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Env, Method, Request, Response, Result, Url};

use crate::database;

pub(crate) mod schema;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RangeBounds<'a> {
  pub begin: Option<&'a str>,
  pub end: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RangeFilter<'a> {
  pub column: &'a str,
  pub bounds: RangeBounds<'a>,
}

pub(crate) fn parse_range_bounds(range_val: &str) -> std::result::Result<RangeBounds<'_>, &'static str> {
  if let Some(end) = range_val.strip_prefix(':') {
    if end.is_empty() || end.starts_with(':') {
      return Err("empty or malformed open-start range");
    }
    return Ok(RangeBounds { begin: None, end: Some(end) });
  }

  if let Some(begin) = range_val.strip_suffix(':') {
    if begin.is_empty() || begin.ends_with(':') {
      return Err("empty or malformed open-end range");
    }
    return Ok(RangeBounds { begin: Some(begin), end: None });
  }

  let colon_indices: Vec<usize> = range_val.match_indices(':').map(|(i, _)| i).collect();
  let (begin, end) = match colon_indices.as_slice() {
    &[idx] => (&range_val[..idx], &range_val[idx + 1..]),
    &[_, split_idx, _] => (&range_val[..split_idx], &range_val[split_idx + 1..]),
    &[_, _, split_idx, _, _] => (&range_val[..split_idx], &range_val[split_idx + 1..]),
    _ => return Err("invalid range bounds separator"),
  };

  if begin.is_empty() && end.is_empty() {
    return Err("empty range bounds");
  }

  Ok(RangeBounds { begin: if begin.is_empty() { None } else { Some(begin) }, end: if end.is_empty() { None } else { Some(end) } })
}

pub(crate) fn parse_range_part(part: &str) -> std::result::Result<RangeFilter<'_>, String> {
  let part = part.trim();
  if part.is_empty() {
    return Err("Invalid range format: empty range part".to_string());
  }
  let Some(bracket_start) = part.find('[') else {
    return Err(format!("Invalid range format for '{}'. Expected col[begin:end]", part));
  };
  if !part.ends_with(']') {
    return Err(format!("Invalid range format for '{}'. Expected col[begin:end]", part));
  }
  let bracket_end = part.len() - 1;
  if bracket_start == 0 || bracket_start >= bracket_end {
    return Err(format!("Invalid range format for '{}'. Expected col[begin:end]", part));
  }
  let column = part[..bracket_start].trim();
  let range_val = &part[bracket_start + 1..bracket_end];

  let bounds = parse_range_bounds(range_val).map_err(|_| format!("Invalid range format for {}. Expected col[begin:end]", column))?;

  Ok(RangeFilter { column, bounds })
}

fn parse_filter_items(raw_value: &str) -> Vec<String> {
  if raw_value.trim().is_empty() {
    return vec![raw_value.to_string()];
  }
  let escaped = raw_value.replace(r"\,", "\x00");
  let raw_parts: Vec<&str> = escaped.split(',').collect();
  let mut items = Vec::new();
  let mut buffer = String::new();

  for part in raw_parts {
    if !buffer.is_empty() {
      buffer.push(',');
    }
    buffer.push_str(part);
    if buffer.contains('@') {
      items.push(buffer.replace('\x00', ","));
      buffer.clear();
    }
  }
  if !buffer.is_empty() {
    items.push(buffer.replace('\x00', ","));
  }
  items
}

pub async fn handle(req: &Request, env: &Env, url: &Url, provider: &str, table_name: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return Response::error("Method Not Allowed", 405);
  }
  let limit: u32 = url.query_pairs().find(|(k, _)| k == "limit").and_then(|(_, v)| v.parse::<u32>().ok()).map(|v| v.min(super::max_rows(env))).unwrap_or_else(|| super::max_rows(env)); // Enforce the configured maximum limit
  let offset: u32 = url.query_pairs().find(|(k, _)| k == "offset").and_then(|(_, v)| v.parse::<u32>().ok()).unwrap_or(0);

  let binding_name = database::binding_name(provider);
  let d1 = match env.d1(&binding_name) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // Validate table name to prevent SQL injection
  let Some(valid_columns) = schema::columns(provider, table_name) else {
    return Response::error(format!("Table '{}' not found", table_name), 404);
  };

  // Parse 'include' and 'exclude' from url
  let mut selected_columns = "*".to_string();

  let include_cols: Option<Vec<String>> = url.query_pairs().find(|(k, _)| k == "include").map(|(_, v)| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());

  let exclude_cols: Option<Vec<String>> = url.query_pairs().find(|(k, _)| k == "exclude").map(|(_, v)| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());

  if let Some(cols) = include_cols {
    for col in &cols {
      if !valid_columns.contains(&col.as_str()) {
        return Response::error(format!("Invalid include column: {}", col), 400);
      }
    }
    selected_columns = cols.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
  } else if let Some(cols) = exclude_cols {
    let mut final_cols = Vec::new();
    for &valid_col in valid_columns {
      if !cols.iter().any(|c| c == valid_col) {
        final_cols.push(valid_col.to_string());
      }
    }
    for col in &cols {
      if !valid_columns.contains(&col.as_str()) {
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
        for item in parse_filter_items(v.as_ref()) {
          let Some((val, col)) = item.rsplit_once('@') else {
            return Response::error(format!("Invalid {} format. Expected val@col", k), 400);
          };
          let col = col.trim();
          if col.is_empty() || !valid_columns.contains(&col) {
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
        if v.trim().is_empty() {
          return Response::error("Invalid range format: empty range", 400);
        }
        for part in v.split(',') {
          let range_filter = match parse_range_part(part) {
            Ok(rf) => rf,
            Err(err) => return Response::error(err, 400),
          };

          if !valid_columns.contains(&range_filter.column) {
            return Response::error(format!("Invalid range column: {}", range_filter.column), 400);
          }

          if let Some(begin) = range_filter.bounds.begin {
            filter_clauses.push(format!("\"{}\" >= ?{}", range_filter.column, filter_clauses.len() + 1));
            filter_values.push(begin.to_string());
          }
          if let Some(end) = range_filter.bounds.end {
            filter_clauses.push(format!("\"{}\" <= ?{}", range_filter.column, filter_clauses.len() + 1));
            filter_values.push(end.to_string());
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
        if !valid_columns.contains(&col_name) {
          return Response::error(format!("Invalid sort column: {}", col_name), 400);
        }
        sort_clauses.push(format!("\"{}\" DESC", col_name));
      } else {
        if !valid_columns.contains(&col) {
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

  let data_results = match statement.inner().all() {
    Ok(promise) => match JsFuture::from(promise).await {
      Ok(result) => result,
      Err(error) => return Response::error(format!("Query error: {:?}", error), 500),
    },
    Err(error) => return Response::error(format!("Query error: {:?}", error), 500),
  };
  let details = match Reflect::get(&data_results, &JsValue::from_str("results")) {
    Ok(d) => d,
    Err(e) => return Response::error(format!("Failed to parse query results: {:?}", e), 500),
  };
  let response_data = Object::new();
  let _ = Reflect::set(&response_data, &JsValue::from_str("data"), &details);
  let _ = Reflect::set(&response_data, &JsValue::from_str("limit"), &JsValue::from_f64(limit as f64));
  let _ = Reflect::set(&response_data, &JsValue::from_str("offset"), &JsValue::from_f64(offset as f64));

  let body = match JSON::stringify(&response_data) {
    Ok(js_str) => match js_str.as_string() {
      Some(s) => s,
      None => return Response::error("Failed to stringify response JSON", 500),
    },
    Err(e) => return Response::error(format!("Failed to serialize data response: {:?}", e), 500),
  };
  let mut response = Response::ok(body)?;
  response.headers_mut().set("Content-Type", "application/json")?;
  Ok(response)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_filter_items_single_and_escaped() {
    assert_eq!(parse_filter_items(""), vec![""]);
    assert_eq!(parse_filter_items("   "), vec!["   "]);
    assert_eq!(parse_filter_items("val@col"), vec!["val@col"]);
    assert_eq!(parse_filter_items(r"val1\,val2@col"), vec!["val1,val2@col"]);
    assert_eq!(parse_filter_items("val1@col1,val2@col2"), vec!["val1@col1", "val2@col2"]);
  }

  #[test]
  fn test_parse_range_bounds_valid_cases() {
    assert_eq!(parse_range_bounds("1:10"), Ok(RangeBounds { begin: Some("1"), end: Some("10") }));
    assert_eq!(parse_range_bounds(":10"), Ok(RangeBounds { begin: None, end: Some("10") }));
    assert_eq!(parse_range_bounds("5:"), Ok(RangeBounds { begin: Some("5"), end: None }));
    assert_eq!(parse_range_bounds("08:00:10:00"), Ok(RangeBounds { begin: Some("08:00"), end: Some("10:00") }));
    assert_eq!(parse_range_bounds("08:00:00:10:00:00"), Ok(RangeBounds { begin: Some("08:00:00"), end: Some("10:00:00") }));
    assert_eq!(parse_range_bounds(":10:00:00"), Ok(RangeBounds { begin: None, end: Some("10:00:00") }));
    assert_eq!(parse_range_bounds("08:00:00:"), Ok(RangeBounds { begin: Some("08:00:00"), end: None }));
  }

  #[test]
  fn test_parse_range_bounds_invalid_cases() {
    assert!(parse_range_bounds(":").is_err());
    assert!(parse_range_bounds("::").is_err());
    assert!(parse_range_bounds("0:1:2").is_err());
    assert!(parse_range_bounds("08:00:00").is_err());
  }

  #[test]
  fn test_parse_range_part_validation() {
    assert_eq!(parse_range_part("Status[1:10]"), Ok(RangeFilter { column: "Status", bounds: RangeBounds { begin: Some("1"), end: Some("10") } }));
    assert!(parse_range_part("").is_err());
    assert!(parse_range_part("Status").is_err());
    assert!(parse_range_part("Status[1:10").is_err());
    assert!(parse_range_part("[1:10]").is_err());
    assert_eq!(parse_range_part("Status[0:1:2]"), Err("Invalid range format for Status. Expected col[begin:end]".into()));
    assert_eq!(parse_range_part("Status[08:00:00]"), Err("Invalid range format for Status. Expected col[begin:end]".into()));
    assert_eq!(parse_range_part("Status[:]"), Err("Invalid range format for Status. Expected col[begin:end]".into()));
    assert_eq!(parse_range_part("Status[::]"), Err("Invalid range format for Status. Expected col[begin:end]".into()));
  }
}
