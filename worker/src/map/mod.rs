use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{Cache, Context, Env, Method, Request, Response, Result, Url};

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

fn json_error(status: u16, message: &str) -> Result<Response> {
  Ok(Response::from_json(&json!({ "error": message }))?.with_status(status))
}

pub async fn handle(req: &Request, env: &Env, ctx: Context, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return json_error(405, "Method not allowed");
  }

  let mut route_id = None;
  for (key, value) in url.query_pairs() {
    if key != "route_id" || route_id.is_some() || value.trim().is_empty() || value.len() > 256 {
      return json_error(400, "Expected at most one non-empty route_id");
    }
    route_id = Some(value.into_owned());
  }

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
  let filter = route_id.as_deref().map(JsValue::from).unwrap_or(JsValue::NULL);

  let route_result = db.prepare("SELECT * FROM routes WHERE (?1 IS NULL OR route_id = ?1) ORDER BY route_id").bind(std::slice::from_ref(&filter))?.all().await?;
  if !route_result.success() {
    return json_error(503, "Map data is unavailable");
  }
  let routes = route_result
    .results::<Value>()?
    .into_iter()
    .map(serde_json::from_value)
    .collect::<std::result::Result<Vec<MapRoute>, _>>()?
    .into_iter()
    .map(|route| (route.route_id.clone(), route))
    .collect::<HashMap<String, MapRoute>>();
  if route_id.is_some() && routes.is_empty() {
    return json_error(404, "Unknown route_id");
  }

  let mut paths: BTreeMap<(String, String), Vec<[f64; 2]>> = BTreeMap::new();
  let has_shapes = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='shapes'").first::<String>(Some("name")).await?.is_some();
  if has_shapes {
    let shape_result = db
      .prepare(
        "SELECT trip_shapes.route_id, trip_shapes.shape_id, shapes.shape_pt_lat AS lat, shapes.shape_pt_lon AS lng \
         FROM (SELECT DISTINCT route_id, shape_id FROM trips WHERE shape_id IS NOT NULL AND (?1 IS NULL OR route_id = ?1)) AS trip_shapes \
         JOIN shapes ON shapes.shape_id = trip_shapes.shape_id \
         ORDER BY trip_shapes.route_id, trip_shapes.shape_id, shapes.shape_pt_sequence",
      )
      .bind(std::slice::from_ref(&filter))?
      .all()
      .await?;
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

  let mut segments = Vec::new();
  if !has_shapes {
    let segment_result = db
      .prepare(
        "WITH consecutive AS ( \
           SELECT trips.route_id, stop_times.stop_id AS to_stop_id, \
                  LAG(stop_times.stop_id) OVER (PARTITION BY trips.trip_id ORDER BY stop_times.stop_sequence) AS from_stop_id \
           FROM trips JOIN stop_times ON stop_times.trip_id = trips.trip_id \
           WHERE (?1 IS NULL OR trips.route_id = ?1) \
         ) \
         SELECT DISTINCT consecutive.route_id, consecutive.from_stop_id, consecutive.to_stop_id, \
                from_stop.stop_lat AS from_lat, from_stop.stop_lon AS from_lng, \
                to_stop.stop_lat AS to_lat, to_stop.stop_lon AS to_lng \
         FROM consecutive \
         JOIN stops AS from_stop ON from_stop.stop_id = consecutive.from_stop_id \
         JOIN stops AS to_stop ON to_stop.stop_id = consecutive.to_stop_id \
         WHERE consecutive.from_stop_id <> consecutive.to_stop_id \
         ORDER BY consecutive.route_id, consecutive.from_stop_id, consecutive.to_stop_id",
      )
      .bind(std::slice::from_ref(&filter))?
      .all()
      .await?;
    if !segment_result.success() {
      return json_error(503, "Map data is unavailable");
    }
    segments = segment_result.results::<Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<StopSegment>, _>>()?;
  }

  let stop_result = db
    .prepare(
      "SELECT DISTINCT trips.route_id, stops.stop_id, stops.stop_name, stops.stop_lat AS lat, stops.stop_lon AS lng \
       FROM trips JOIN stop_times ON stop_times.trip_id = trips.trip_id \
       JOIN stops ON stops.stop_id = stop_times.stop_id \
       WHERE (?1 IS NULL OR trips.route_id = ?1) \
       ORDER BY trips.route_id, stops.stop_id",
    )
    .bind(&[filter])?
    .all()
    .await?;
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
        "route_short_name": route.and_then(|route| route.route_short_name.as_deref()),
        "route_long_name": route.and_then(|route| route.route_long_name.as_deref()),
        "route_color": route.and_then(|route| route.route_color.as_deref()),
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
          "route_short_name": route.and_then(|route| route.route_short_name.as_deref()),
          "route_long_name": route.and_then(|route| route.route_long_name.as_deref()),
          "route_color": route.and_then(|route| route.route_color.as_deref()),
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
  response.headers_mut().set("Cache-Control", "public, max-age=86400")?;
  let cached_response = response.cloned()?;
  ctx.wait_until(async move {
    let _ = Cache::default().put(cache_key, cached_response).await;
  });
  Ok(response)
}
