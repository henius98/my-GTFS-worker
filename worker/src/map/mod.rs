use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{Cache, Context, D1Database, D1Result, Env, Method, Request, Response, Result, Url};

use crate::database;

#[derive(Deserialize)]
struct MapRoute {
  route_id: String,
  route_short_name: Option<String>,
  route_long_name: Option<String>,
  route_color: Option<String>,
}

#[derive(Deserialize)]
struct ShapePoint {
  route_id: String,
  shape_id: String,
  lat: Option<f64>,
  lng: Option<f64>,
}

#[derive(Deserialize)]
struct StopPoint {
  route_id: String,
  stop_id: String,
  stop_name: Option<String>,
  lat: Option<f64>,
  lng: Option<f64>,
}

#[derive(Deserialize)]
struct StopSegment {
  route_id: String,
  from_stop_id: String,
  to_stop_id: String,
  from_lat: Option<f64>,
  from_lng: Option<f64>,
  to_lat: Option<f64>,
  to_lng: Option<f64>,
}

fn coordinates(lat: Option<f64>, lng: Option<f64>) -> Option<[f64; 2]> {
  match (lat, lng) {
    (Some(lat), Some(lng)) if lat.is_finite() && lng.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng) => Some([lng, lat]),
    _ => None,
  }
}

const ROUTES_SQL: &str = include_str!("sql/routes.sql");
const SHAPES_SQL: &str = include_str!("sql/shapes.sql");
const SEGMENTS_SQL: &str = include_str!("sql/segments.sql");
const STOPS_SQL: &str = include_str!("sql/stops.sql");
const STOPS_BY_ROUTE_SQL: &str = include_str!("sql/stops_by_route.sql");

fn route_select_sql(provider: &str, filter_by_route: bool) -> String {
  let has = |column: &str| crate::data::schema::has_column(provider, "routes", column);
  let where_clause = if filter_by_route { " WHERE route_id = ?1 LIMIT 1" } else { "" };
  let route_columns = format!(
    "{}, {}, {}",
    if has("route_short_name") { "route_short_name" } else { "NULL AS route_short_name" },
    if has("route_long_name") { "route_long_name" } else { "NULL AS route_long_name" },
    if has("route_color") { "route_color" } else { "NULL AS route_color" },
  );
  ROUTES_SQL.trim_end().replace("__ROUTE_COLUMNS__", &route_columns).replace("__WHERE_CLAUSE__", where_clause)
}

fn json_error(status: u16, message: &str) -> Result<Response> {
  Ok(Response::from_json(&json!({ "error": message }))?.with_status(status))
}

fn parse_route_id(url: &Url) -> std::result::Result<Option<String>, (u16, &'static str)> {
  let mut route_id = None;
  for (key, value) in url.query_pairs() {
    if key != "route_id" || route_id.is_some() || value.trim().is_empty() || value.len() > crate::MAX_IDENTIFIER_BYTES {
      return Err((400, "Expected at most one non-empty route_id"));
    }
    route_id = Some(value.into_owned());
  }
  Ok(route_id)
}

async fn query_all(db: &D1Database, sql: &str, filter: Option<&JsValue>) -> Result<D1Result> {
  let stmt = db.prepare(sql);
  match filter {
    Some(f) => stmt.bind(std::slice::from_ref(f))?.all().await,
    None => stmt.all().await,
  }
}

pub async fn handle(req: &Request, env: &Env, ctx: Context, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return json_error(405, "Method not allowed");
  }

  let route_id = match parse_route_id(url) {
    Ok(route_id) => route_id,
    Err((status, message)) => return json_error(status, message),
  };
  let cache_control = crate::feed_cache_control(env)?;

  let mut cache_url = url.clone();
  cache_url.set_path(&format!("/{}/map", provider.to_ascii_lowercase()));
  cache_url.set_query(None);
  if let Some(route_id) = &route_id {
    cache_url.query_pairs_mut().append_pair("route_id", route_id);
  }
  let cache_key = cache_url.to_string();
  if let Ok(Some(response)) = Cache::default().get(&cache_key, false).await {
    return Ok(response);
  }

  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return json_error(404, "Map database is unavailable"),
  };
  let filter = route_id.as_deref().map(JsValue::from);

  let route_sql = route_select_sql(provider, route_id.is_some());
  let route_result = query_all(&db, &route_sql, filter.as_ref()).await?;
  if !route_result.success() {
    return json_error(503, "Map data is unavailable");
  }
  let routes: HashMap<String, MapRoute> = route_result
    .results::<Value>()?
    .into_iter()
    .map(serde_json::from_value::<MapRoute>)
    .collect::<std::result::Result<Vec<_>, _>>()?
    .into_iter()
    .map(|route| (route.route_id.clone(), route))
    .collect();
  if route_id.is_some() && routes.is_empty() {
    return json_error(404, "Unknown route_id");
  }

  let mut paths: BTreeMap<(String, String), Vec<[f64; 2]>> = BTreeMap::new();
  let has_shapes = crate::data::schema::has_column(provider, "shapes", "shape_id") && crate::data::schema::has_column(provider, "trips", "shape_id");
  if has_shapes {
    let shape_filter = if route_id.is_some() { " AND route_id = ?1" } else { "" };
    let shape_sql = SHAPES_SQL.replace("__SHAPE_FILTER__", shape_filter);
    let shape_result = query_all(&db, &shape_sql, filter.as_ref()).await?;
    if !shape_result.success() {
      return json_error(503, "Map data is unavailable");
    }
    let shape_points = shape_result.results::<Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<ShapePoint>, _>>()?;
    for point in shape_points {
      if let Some(position) = coordinates(point.lat, point.lng) {
        paths.entry((point.route_id, point.shape_id)).or_default().push(position);
      }
    }
  }

  let trip_filter = if route_id.is_some() { " WHERE trips.route_id = ?1" } else { "" };
  let mut segments = Vec::new();
  if paths.is_empty() {
    let segment_sql = SEGMENTS_SQL.replace("__TRIP_FILTER__", trip_filter);
    let segment_result = query_all(&db, &segment_sql, filter.as_ref()).await?;
    if !segment_result.success() {
      return json_error(503, "Map data is unavailable");
    }
    segments = segment_result.results::<Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<StopSegment>, _>>()?;
  }

  let stop_sql = if route_id.is_some() { STOPS_BY_ROUTE_SQL } else { STOPS_SQL };
  let stop_result = query_all(&db, stop_sql, filter.as_ref()).await?;
  if !stop_result.success() {
    return json_error(503, "Map data is unavailable");
  }
  let stops = stop_result.results::<Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<StopPoint>, _>>()?;

  let mut features = Vec::new();
  for ((route_id, shape_id), points) in paths {
    if points.len() < 2 {
      continue;
    }
    let route = routes.get(&route_id);
    features.push(json!({
      "type": "Feature",
      "geometry": { "type": "LineString", "coordinates": points },
      "properties": {
        "kind": "route",
        "route_id": route_id,
        "shape_id": shape_id,
        "geometry_source": "shape",
        "route_short_name": route.and_then(|r| r.route_short_name.as_deref()),
        "route_long_name": route.and_then(|r| r.route_long_name.as_deref()),
        "route_color": route.and_then(|r| r.route_color.as_deref()),
      },
    }));
  }
  for segment in segments {
    if let (Some(from), Some(to)) = (coordinates(segment.from_lat, segment.from_lng), coordinates(segment.to_lat, segment.to_lng)) {
      let route = routes.get(&segment.route_id);
      features.push(json!({
        "type": "Feature",
        "geometry": { "type": "LineString", "coordinates": [from, to] },
        "properties": {
          "kind": "route",
          "route_id": segment.route_id,
          "from_stop_id": segment.from_stop_id,
          "to_stop_id": segment.to_stop_id,
          "geometry_source": "stop_sequence",
          "route_short_name": route.and_then(|r| r.route_short_name.as_deref()),
          "route_long_name": route.and_then(|r| r.route_long_name.as_deref()),
          "route_color": route.and_then(|r| r.route_color.as_deref()),
        },
      }));
    }
  }
  for stop in stops {
    if let Some(position) = coordinates(stop.lat, stop.lng) {
      features.push(json!({
        "type": "Feature",
        "geometry": { "type": "Point", "coordinates": position },
        "properties": {
          "kind": "stop",
          "route_id": stop.route_id,
          "stop_id": stop.stop_id,
          "stop_name": stop.stop_name,
        },
      }));
    }
  }

  let mut response = Response::from_json(&json!({ "type": "FeatureCollection", "features": features }))?;
  response.headers_mut().set("Cache-Control", &cache_control)?;
  let cached_response = response.cloned()?;
  ctx.wait_until(async move {
    let _ = Cache::default().put(cache_key, cached_response).await;
  });
  Ok(response)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/map/mod.rs"]
mod tests;
