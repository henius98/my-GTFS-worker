use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use worker::js_sys::{Object, Reflect};
use worker::wasm_bindgen::JsValue;
use worker::{Env, Method, Request, Response, Result, Url};

use crate::database;

pub(crate) mod schema;

const SELECT_SQL: &str = include_str!("sql/select.sql");

fn build_select_sql(columns: &str, table: &str, where_clause: &str, order_clause: &str, limit_param: usize, offset_param: usize) -> String {
  SELECT_SQL
    .trim_end()
    .replace("__COLUMNS__", columns)
    .replace("__TABLE__", table)
    .replace("__WHERE_CLAUSE__", where_clause)
    .replace("__ORDER_CLAUSE__", order_clause)
    .replace("__LIMIT_PARAM__", &limit_param.to_string())
    .replace("__OFFSET_PARAM__", &offset_param.to_string())
}

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

#[derive(Clone, Copy)]
enum FilterOp {
  Exact,
  IExact,
  Contains,
  IContains,
}

impl FilterOp {
  const ALL: [(&'static str, Self); 4] = [("filter", Self::Exact), ("ifilter", Self::IExact), ("contains", Self::Contains), ("icontains", Self::IContains)];

  fn clause(self, col: &str, param_idx: usize) -> String {
    match self {
      Self::Exact => format!("\"{col}\" = ?{param_idx}"),
      Self::IExact => format!("\"{col}\" COLLATE NOCASE = ?{param_idx}"),
      Self::Contains => format!("INSTR(\"{col}\", ?{param_idx}) > 0"),
      Self::IContains => format!("\"{col}\" LIKE ?{param_idx} ESCAPE '\\'"),
    }
  }

  fn bind_value(self, val: &str) -> String {
    match self {
      Self::IContains => {
        let escaped = val.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        format!("%{escaped}%")
      }
      _ => val.to_string(),
    }
  }
}

fn resolve_column(valid_columns: &'static [&'static str], col: &str) -> Option<&'static str> {
  valid_columns.iter().copied().find(|vc| vc.eq_ignore_ascii_case(col))
}

fn is_valid_range_time(s: &str) -> bool {
  let colon_count = s.chars().filter(|&c| c == ':').count();
  match colon_count {
    1 => (s.len() == 4 || s.len() == 5) && s.split(':').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
    2 => (s.len() == 7 || s.len() == 8) && s.split(':').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
    _ => false,
  }
}

fn parse_range_time_secs(s: &str) -> Option<u32> {
  let mut parts = s.split(':');
  let hours: u32 = parts.next()?.parse().ok()?;
  let minutes: u32 = parts.next()?.parse().ok()?;
  let seconds: u32 = match parts.next() {
    Some(sec) => sec.parse().ok()?,
    None => 0,
  };
  if parts.next().is_some() {
    return None;
  }
  hours.checked_mul(3600)?.checked_add(minutes * 60 + seconds)
}

fn validate_open_bound<'a>(bound: &'a str, err: &'static str) -> std::result::Result<&'a str, &'static str> {
  if bound.is_empty() || bound.starts_with(':') || bound.ends_with(':') || bound.contains("..") || (bound.contains(':') && !is_valid_range_time(bound)) {
    return Err(err);
  }
  Ok(bound)
}

pub(crate) fn parse_range_bounds(range_val: &str) -> std::result::Result<RangeBounds<'_>, &'static str> {
  if range_val.contains("...") {
    return Err("invalid range bounds separator");
  }

  if let Some(end) = range_val.strip_prefix(':') {
    return Ok(RangeBounds { begin: None, end: Some(validate_open_bound(end, "empty or malformed open-start range")?) });
  }

  if let Some(begin) = range_val.strip_suffix(':') {
    return Ok(RangeBounds { begin: Some(validate_open_bound(begin, "empty or malformed open-end range")?), end: None });
  }

  let (begin, end) = if let Some((b, e)) = range_val.split_once("..") {
    if b.contains("..") || e.contains("..") {
      return Err("invalid range bounds");
    }
    (b, e)
  } else {
    let mut colons = [0usize; 6];
    let mut count = 0;
    for (idx, _) in range_val.match_indices(':') {
      if count == colons.len() {
        return Err("invalid range bounds separator");
      }
      colons[count] = idx;
      count += 1;
    }
    match &colons[..count] {
      [idx] => (&range_val[..*idx], &range_val[*idx + 1..]),
      [_, split_idx, _] => (&range_val[..*split_idx], &range_val[*split_idx + 1..]),
      [_, idx1, idx2, _] => {
        let (left_1, right_1) = (&range_val[..*idx1], &range_val[*idx1 + 1..]);
        let (left_2, right_2) = (&range_val[..*idx2], &range_val[*idx2 + 1..]);
        let valid_1 = is_valid_range_time(left_1) && is_valid_range_time(right_1);
        let valid_2 = is_valid_range_time(left_2) && is_valid_range_time(right_2);
        match (valid_1, valid_2) {
          (true, true) => {
            let ordered_1 = parse_range_time_secs(left_1).zip(parse_range_time_secs(right_1)).is_some_and(|(a, b)| a <= b);
            let ordered_2 = parse_range_time_secs(left_2).zip(parse_range_time_secs(right_2)).is_some_and(|(a, b)| a <= b);
            if ordered_1 && !ordered_2 { (left_1, right_1) } else { (left_2, right_2) }
          }
          (false, true) => (left_2, right_2),
          (true, false) => (left_1, right_1),
          (false, false) => return Err("invalid range bounds separator"),
        }
      }
      [_, _, split_idx, _, _] => (&range_val[..*split_idx], &range_val[*split_idx + 1..]),
      _ => return Err("invalid range bounds separator"),
    }
  };

  if (begin.is_empty() && end.is_empty()) || begin.starts_with(':') || begin.ends_with(':') || end.starts_with(':') || end.ends_with(':') {
    return Err("empty or malformed range bounds");
  }

  if (!begin.is_empty() && begin.contains(':') && !is_valid_range_time(begin)) || (!end.is_empty() && end.contains(':') && !is_valid_range_time(end)) {
    return Err("invalid range bounds time format");
  }

  Ok(RangeBounds { begin: (!begin.is_empty()).then_some(begin), end: (!end.is_empty()).then_some(end) })
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
  let mut items = Vec::new();
  let mut buffer = String::new();
  let mut has_at = false;
  let mut chars = raw_value.chars().peekable();

  while let Some(ch) = chars.next() {
    match ch {
      '\\' if chars.peek() == Some(&',') => {
        chars.next();
        buffer.push(',');
      }
      ',' if has_at => {
        items.push(std::mem::take(&mut buffer));
        has_at = false;
      }
      '@' => {
        has_at = true;
        buffer.push('@');
      }
      _ => buffer.push(ch),
    }
  }
  if !buffer.is_empty() {
    items.push(buffer);
  }
  items
}

pub async fn handle(req: &Request, env: &Env, url: &Url, provider: &str, table_name: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return crate::method_not_allowed("GET", false);
  }

  let mut query_params: HashMap<Cow<'_, str>, Cow<'_, str>> = HashMap::new();
  for (k, v) in url.query_pairs() {
    if query_params.insert(k.clone(), v).is_some() {
      return Response::error(format!("Duplicate query parameter: {k}"), 400);
    }
  }

  let configured_max_rows = super::max_rows(env)?;
  let cache_control = crate::api_cache_control(env)?;
  let limit: u32 = match query_params.get("limit") {
    Some(v) => match v.parse::<u32>() {
      Ok(val) => val.min(configured_max_rows), // Enforce the configured maximum limit
      Err(_) => return Response::error("Invalid limit parameter", 400),
    },
    None => configured_max_rows, // Enforce the configured maximum limit
  };

  let offset: u32 = match query_params.get("offset") {
    Some(v) => match v.parse::<u32>() {
      Ok(val) => val,
      Err(_) => return Response::error("Invalid offset parameter", 400),
    },
    None => 0,
  };

  let d1 = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return database::provider_not_found(provider),
  };

  // Validate table name to prevent SQL injection
  let Some(valid_columns) = schema::columns(provider, table_name) else {
    return Response::error(format!("Table '{}' not found", table_name), 404);
  };

  // Parse 'include' and 'exclude' from url
  let include_cols: Option<Vec<&str>> = query_params.get("include").map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).collect());
  let exclude_cols: Option<Vec<&str>> = query_params.get("exclude").map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).collect());
  if include_cols.is_some() && exclude_cols.is_some() {
    return Response::error("Cannot specify both include and exclude parameters", 400);
  }

  let selected_columns = if let Some(cols) = include_cols {
    if cols.is_empty() {
      return Response::error("Include parameter cannot be empty", 400);
    }
    let mut canonical_cols = Vec::with_capacity(cols.len());
    for col in cols {
      let Some(canonical) = resolve_column(valid_columns, col) else {
        return Response::error(format!("Invalid include column: {col}"), 400);
      };
      canonical_cols.push(format!("\"{canonical}\""));
    }
    canonical_cols.join(", ")
  } else if let Some(cols) = exclude_cols {
    if cols.is_empty() {
      return Response::error("Exclude parameter cannot be empty", 400);
    }
    let mut excluded = HashSet::with_capacity(cols.len());
    for col in cols {
      let Some(canonical) = resolve_column(valid_columns, col) else {
        return Response::error(format!("Invalid exclude column: {col}"), 400);
      };
      excluded.insert(canonical);
    }
    let final_cols: Vec<String> = valid_columns.iter().copied().filter(|col| !excluded.contains(col)).map(|c| format!("\"{c}\"")).collect();
    if final_cols.is_empty() {
      return Response::error("Cannot exclude all columns", 400);
    }
    final_cols.join(", ")
  } else {
    "*".to_string()
  };

  // Parse filters
  let mut filter_clauses = Vec::new();
  let mut filter_values = Vec::new();

  for (key, op) in FilterOp::ALL {
    let Some(v) = query_params.get(key) else {
      continue;
    };
    for item in parse_filter_items(v.as_ref()) {
      let Some((val, col)) = item.rsplit_once('@') else {
        return Response::error(format!("Invalid {key} format. Expected val@col"), 400);
      };
      let col = col.trim();
      let Some(canonical_col) = resolve_column(valid_columns, col) else {
        return Response::error(format!("Invalid filter column: {col}"), 400);
      };
      filter_clauses.push(op.clause(canonical_col, filter_clauses.len() + 1));
      filter_values.push(op.bind_value(val));
    }
  }

  if let Some(v) = query_params.get("range") {
    if v.trim().is_empty() {
      return Response::error("Invalid range format: empty range", 400);
    }
    for part in v.split(',') {
      let range_filter = match parse_range_part(part) {
        Ok(rf) => rf,
        Err(err) => return Response::error(err, 400),
      };
      let Some(canonical_col) = resolve_column(valid_columns, range_filter.column) else {
        return Response::error(format!("Invalid range column: {}", range_filter.column), 400);
      };
      if let Some(begin) = range_filter.bounds.begin {
        filter_clauses.push(format!("\"{canonical_col}\" >= ?{}", filter_clauses.len() + 1));
        filter_values.push(begin.to_string());
      }
      if let Some(end) = range_filter.bounds.end {
        filter_clauses.push(format!("\"{canonical_col}\" <= ?{}", filter_clauses.len() + 1));
        filter_values.push(end.to_string());
      }
    }
  }

  let mut sort_clauses = Vec::new();
  let mut seen_sort_cols: HashSet<&'static str> = HashSet::new();
  if let Some(v) = query_params.get("sort") {
    for col in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
      let (col_name, dir) = match col.strip_prefix('-') {
        Some(stripped) => (stripped, "DESC"),
        None => (col, "ASC"),
      };
      let Some(canonical_col) = resolve_column(valid_columns, col_name) else {
        return Response::error(format!("Invalid sort column: {col_name}"), 400);
      };
      if seen_sort_cols.insert(canonical_col) {
        sort_clauses.push(format!("\"{canonical_col}\" {dir}"));
      }
    }
  }

  let where_clause = if filter_clauses.is_empty() { String::new() } else { format!("\nWHERE {}", filter_clauses.join(" AND ")) };
  let order_clause = if sort_clauses.is_empty() { String::new() } else { format!("\nORDER BY {}", sort_clauses.join(", ")) };

  // Execute the paginated query
  let normalized_table_name = table_name.to_ascii_lowercase();
  let query = build_select_sql(&selected_columns, &normalized_table_name, &where_clause, &order_clause, filter_clauses.len() + 1, filter_clauses.len() + 2);

  let mut params: Vec<JsValue> = Vec::with_capacity(filter_values.len() + 2);
  for v in filter_values {
    params.push(JsValue::from_str(&v));
  }
  params.push(JsValue::from_f64(limit as f64));
  params.push(JsValue::from_f64(offset as f64));

  let statement = d1.prepare(&query).bind(&params)?;
  let details = database::query_raw_results(&statement).await?;
  let response_data = Object::new();
  Reflect::set(&response_data, &JsValue::from_str("data"), &details)?;
  Reflect::set(&response_data, &JsValue::from_str("limit"), &JsValue::from_f64(limit as f64))?;
  Reflect::set(&response_data, &JsValue::from_str("offset"), &JsValue::from_f64(offset as f64))?;

  let mut response = database::json_response_from_js(&response_data)?;
  response.headers_mut().set("Cache-Control", &cache_control)?;
  Ok(response)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/data/mod.rs"]
mod tests;
