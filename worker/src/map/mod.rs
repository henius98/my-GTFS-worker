use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{Datelike, NaiveDate, Weekday};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use worker::{Cache, Context, D1Database, D1PreparedStatement, D1Result, D1Type, Env, Method, Request, Response, Result, Url};

use crate::database;

#[derive(Deserialize)]
struct MapRoute {
  route_id: String,
  route_short_name: Option<String>,
  route_long_name: Option<String>,
  route_color: Option<String>,
}

#[derive(Deserialize)]
struct RouteWithTrips {
  route_id: String,
  route_short_name: Option<String>,
  route_long_name: Option<String>,
  route_color: Option<String>,
  trip_ids_json: String,
  shape_ids_json: String,
}

#[derive(Deserialize)]
struct ShapePoint {
  route_id: Option<String>,
  shape_id: String,
  trip_ids_json: Option<String>,
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
  #[serde(default)]
  trip_ids_json: Option<String>,
  arrival_times: Option<String>,
  departure_times: Option<String>,
}

#[derive(Deserialize)]
struct StopSegment {
  route_id: String,
  lat: Option<f64>,
  lng: Option<f64>,
  to_lat: Option<f64>,
  to_lng: Option<f64>,
}

#[derive(Deserialize)]
#[serde(tag = "row_kind")]
enum SegmentOrStop {
  #[serde(rename = "segment")]
  Segment(StopSegment),
  #[serde(rename = "stop")]
  Stop(StopPoint),
}

#[derive(Serialize)]
struct FeatureCollection<T> {
  #[serde(rename = "type")]
  kind: &'static str,
  features: T,
}

#[derive(Deserialize)]
struct CachedFeatureCollection {
  features: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct Feature<'a> {
  #[serde(rename = "type")]
  kind: &'static str,
  geometry: Geometry,
  properties: FeatureProperties<'a>,
}

#[derive(Serialize)]
#[serde(tag = "type", content = "coordinates")]
enum Geometry {
  LineString(Vec<[f64; 2]>),
  MultiLineString(Vec<[[f64; 2]; 2]>),
  Point([f64; 2]),
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum FeatureProperties<'a> {
  #[serde(rename = "route")]
  ShapeRoute {
    route_id: String,
    shape_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trip_ids: Option<Vec<String>>,
    geometry_source: &'static str,
    route_short_name: Option<&'a str>,
    route_long_name: Option<&'a str>,
    route_color: Option<&'a str>,
  },
  #[serde(rename = "route")]
  SegmentRoute { route_id: String, geometry_source: &'static str, route_short_name: Option<&'a str>, route_long_name: Option<&'a str>, route_color: Option<&'a str> },
  #[serde(rename = "stop")]
  Stop {
    route_id: String,
    stop_id: String,
    stop_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trip_ids: Option<Vec<String>>,
    arrival_times: Option<String>,
    departure_times: Option<String>,
  },
}

fn coordinates(lat: Option<f64>, lng: Option<f64>) -> Option<[f64; 2]> {
  match (lat, lng) {
    (Some(lat), Some(lng)) if lat.is_finite() && lng.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng) => Some([lng, lat]),
    _ => None,
  }
}

const ACTIVE_SERVICES_CALENDAR_SQL: &str = include_str!("sql/active_services_calendar.sql");
const ACTIVE_SERVICES_WITH_EXCEPTIONS_SQL: &str = include_str!("sql/active_services_with_exceptions.sql");
const ROUTES_SQL: &str = include_str!("sql/routes.sql");
const ROUTE_TRIPS_SQL: &str = include_str!("sql/route_trips.sql");
const SHAPES_SQL: &str = include_str!("sql/shapes.sql");
const SHAPES_BY_ROUTE_SQL: &str = include_str!("sql/shapes_by_route.sql");
const SEGMENTS_AND_STOPS_SQL: &str = include_str!("sql/segments_and_stops.sql");
const SEGMENTS_AND_STOPS_BY_ROUTE_SQL: &str = include_str!("sql/segments_and_stops_by_route.sql");
const STOPS_SQL: &str = include_str!("sql/stops.sql");
const STOPS_BY_ROUTE_SQL: &str = include_str!("sql/stops_by_route.sql");

fn route_select_sql(provider: &str) -> String {
  let route_columns = crate::data::schema::optional_columns(provider, "routes", "", &["route_short_name", "route_long_name", "route_color"]);
  ROUTES_SQL.replace("__ROUTE_COLUMNS__", &route_columns).trim_end().to_owned()
}

fn route_trips_sql(provider: &str, active_services: &str, has_shapes: bool) -> String {
  let route_columns = crate::data::schema::optional_columns(provider, "routes", "routes.", &["route_short_name", "route_long_name", "route_color"]);
  let shape_ids_expr =
    if has_shapes { "json_group_array(DISTINCT trips.shape_id) FILTER (WHERE trips.shape_id IS NOT NULL AND trips.shape_id <> '') AS shape_ids_json" } else { "'[]' AS shape_ids_json" };
  ROUTE_TRIPS_SQL.replace("__ACTIVE_SERVICES__", active_services).replace("__ROUTE_COLUMNS__", &route_columns).replace("__SHAPE_IDS_EXPR__", shape_ids_expr)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServiceDate {
  yyyymmdd: i32,
  weekday: &'static str,
}

impl ServiceDate {
  fn from_naive_date(date: NaiveDate) -> Self {
    let weekday = match date.weekday() {
      Weekday::Mon => "monday",
      Weekday::Tue => "tuesday",
      Weekday::Wed => "wednesday",
      Weekday::Thu => "thursday",
      Weekday::Fri => "friday",
      Weekday::Sat => "saturday",
      Weekday::Sun => "sunday",
    };
    Self { yyyymmdd: date.year() * 10_000 + (date.month() as i32) * 100 + (date.day() as i32), weekday }
  }
}

#[derive(Debug, PartialEq, Eq)]
struct MapParams {
  route_ids: Vec<String>,
  date: ServiceDate,
}

fn parse_service_date(value: &str) -> std::result::Result<ServiceDate, (u16, &'static str)> {
  const INVALID_DATE: (u16, &str) = (400, "Invalid date; expected YYYY-MM-DD or YYYYMMDD");
  let trimmed = value.trim();
  let bytes = trimmed.as_bytes();
  let (y, m, d) = match bytes.len() {
    8 if bytes.iter().all(u8::is_ascii_digit) => (&trimmed[0..4], &trimmed[4..6], &trimmed[6..8]),
    10 if bytes[4] == b'-' && bytes[7] == b'-' && bytes[..4].iter().chain(&bytes[5..7]).chain(&bytes[8..10]).all(u8::is_ascii_digit) => (&trimmed[0..4], &trimmed[5..7], &trimmed[8..10]),
    _ => return Err(INVALID_DATE),
  };
  let year: i32 = y.parse().map_err(|_| INVALID_DATE)?;
  let month: u32 = m.parse().map_err(|_| INVALID_DATE)?;
  let day: u32 = d.parse().map_err(|_| INVALID_DATE)?;
  if year < 1000 {
    return Err(INVALID_DATE);
  }
  let date = NaiveDate::from_ymd_opt(year, month, day).ok_or(INVALID_DATE)?;
  Ok(ServiceDate::from_naive_date(date))
}

fn active_services_cte(date: ServiceDate, has_calendar_dates: bool) -> String {
  let template = if has_calendar_dates { ACTIVE_SERVICES_WITH_EXCEPTIONS_SQL } else { ACTIVE_SERVICES_CALENDAR_SQL };
  template.trim_end().replace("__WEEKDAY__", date.weekday).replace("__DATE__", &date.yyyymmdd.to_string())
}

fn parse_params(url: &Url, default_date: ServiceDate, max_routes: usize) -> std::result::Result<MapParams, (u16, Cow<'static, str>)> {
  const INVALID_ROUTE_ID: &str = "Expected at most one route_id parameter with non-empty comma-separated route IDs";
  let mut route_ids: Option<Vec<String>> = None;
  let mut date = None;
  for (key, value) in url.query_pairs() {
    match key.as_ref() {
      "route_id" => {
        if route_ids.is_some() {
          return Err((400, Cow::Borrowed(INVALID_ROUTE_ID)));
        }
        let mut unique_ids = BTreeSet::new();
        for part in value.split(',') {
          let trimmed = part.trim();
          if trimmed.is_empty() || trimmed.len() > crate::MAX_IDENTIFIER_BYTES {
            return Err((400, Cow::Borrowed(INVALID_ROUTE_ID)));
          }
          unique_ids.insert(trimmed.to_owned());
          if unique_ids.len() > max_routes {
            return Err((400, Cow::Owned(format!("Too many route_id values; maximum is {max_routes}"))));
          }
        }
        route_ids = Some(unique_ids.into_iter().collect());
      }
      "date" => {
        if date.is_some() {
          return Err((400, Cow::Borrowed("Expected at most one date")));
        }
        date = Some(parse_service_date(&value).map_err(|(status, message)| (status, Cow::Borrowed(message)))?);
      }
      _ => return Err((400, Cow::Borrowed("Unexpected query parameter; expected route_id and/or date"))),
    }
  }
  Ok(MapParams { route_ids: route_ids.unwrap_or_default(), date: date.unwrap_or(default_date) })
}

fn route_cache_key(url: &Url, provider: &str, route_id: Option<&str>, date: ServiceDate) -> String {
  let mut cache_url = crate::canonical_cache_url(url, provider, "map");
  if let Some(route_id) = route_id {
    cache_url.query_pairs_mut().append_pair("route_id", route_id);
  }
  cache_url.query_pairs_mut().append_pair("date", &date.yyyymmdd.to_string());
  cache_url.to_string()
}

fn cache_responses(ctx: &Context, entries: Vec<(String, Response)>) {
  if entries.is_empty() {
    return;
  }
  ctx.wait_until(async move {
    let _ = join_all(entries.into_iter().map(|(key, response)| async move {
      let _ = Cache::default().put(key, response).await;
    }))
    .await;
  });
}

fn prepare_ids_query(db: &D1Database, sql: &str, ids_json: &str) -> Result<D1PreparedStatement> {
  db.prepare(sql).bind_refs(&D1Type::Text(ids_json))
}

fn take_result(results: &mut std::vec::IntoIter<D1Result>, label: &str) -> Result<D1Result> {
  results.next().ok_or_else(|| worker::Error::RustError(format!("Missing {label} result")))
}

#[derive(Debug, Default, PartialEq)]
struct ShapePath {
  points: Vec<[f64; 2]>,
  trip_ids: Option<Vec<String>>,
}

fn collect_paths(shape_points: Vec<ShapePoint>) -> Result<RoutePaths> {
  let mut paths: RoutePaths = BTreeMap::new();
  for point in shape_points {
    let Some(route_id) = point.route_id else {
      continue;
    };
    let entry = paths.entry((route_id, point.shape_id)).or_default();
    if entry.trip_ids.is_none()
      && let Some(trip_ids_json) = point.trip_ids_json
    {
      let mut trip_ids: Vec<String> = serde_json::from_str(&trip_ids_json)?;
      trip_ids.sort_unstable();
      entry.trip_ids = Some(trip_ids);
    }
    if let Some(position) = coordinates(point.lat, point.lng) {
      entry.points.push(position);
    }
  }
  paths.retain(|_, path| !path.points.is_empty());
  Ok(paths)
}

fn collect_paths_by_shape(shape_points: Vec<ShapePoint>, shape_routes: &BTreeMap<String, Vec<String>>) -> RoutePaths {
  let mut by_shape: BTreeMap<String, Vec<[f64; 2]>> = BTreeMap::new();
  for point in shape_points {
    let Some(position) = coordinates(point.lat, point.lng) else {
      continue;
    };
    by_shape.entry(point.shape_id).or_default().push(position);
  }
  let mut paths: RoutePaths = BTreeMap::new();
  for (shape_id, points) in by_shape {
    let Some(route_ids) = shape_routes.get(&shape_id) else {
      continue;
    };
    if let Some((last_route_id, other_route_ids)) = route_ids.split_last() {
      for route_id in other_route_ids {
        paths.insert((route_id.clone(), shape_id.clone()), ShapePath { points: points.clone(), trip_ids: None });
      }
      paths.insert((last_route_id.clone(), shape_id), ShapePath { points, trip_ids: None });
    }
  }
  paths
}

type RouteSegments = BTreeMap<String, Vec<[[f64; 2]; 2]>>;
type RoutePaths = BTreeMap<(String, String), ShapePath>;

struct MapData {
  routes: Vec<MapRoute>,
  paths: RoutePaths,
  segments: RouteSegments,
  stops: Vec<StopPoint>,
}

fn collect_segments_and_stops(rows: Vec<SegmentOrStop>) -> (RouteSegments, Vec<StopPoint>) {
  let mut segments: RouteSegments = BTreeMap::new();
  let mut stops = Vec::new();
  for row in rows {
    match row {
      SegmentOrStop::Segment(segment) => {
        if let (Some(from), Some(to)) = (coordinates(segment.lat, segment.lng), coordinates(segment.to_lat, segment.to_lng)) {
          segments.entry(segment.route_id).or_default().push([from, to]);
        }
      }
      SegmentOrStop::Stop(stop) => stops.push(stop),
    }
  }
  (segments, stops)
}

fn build_features<'a>(routes: &'a HashMap<String, MapRoute>, paths: RoutePaths, segments: RouteSegments, stops: Vec<StopPoint>) -> Vec<Feature<'a>> {
  let mut features = Vec::with_capacity(paths.len() + segments.len() + stops.len());
  for ((route_id, shape_id), path) in paths {
    if path.points.len() < 2 {
      continue;
    }
    let route = routes.get(&route_id);
    features.push(Feature {
      kind: "Feature",
      geometry: Geometry::LineString(path.points),
      properties: FeatureProperties::ShapeRoute {
        route_id,
        shape_id,
        trip_ids: path.trip_ids,
        geometry_source: "shape",
        route_short_name: route.and_then(|r| r.route_short_name.as_deref()),
        route_long_name: route.and_then(|r| r.route_long_name.as_deref()),
        route_color: route.and_then(|r| r.route_color.as_deref()),
      },
    });
  }
  for (route_id, lines) in segments {
    let route = routes.get(&route_id);
    features.push(Feature {
      kind: "Feature",
      geometry: Geometry::MultiLineString(lines),
      properties: FeatureProperties::SegmentRoute {
        route_id,
        geometry_source: "stop_sequence",
        route_short_name: route.and_then(|r| r.route_short_name.as_deref()),
        route_long_name: route.and_then(|r| r.route_long_name.as_deref()),
        route_color: route.and_then(|r| r.route_color.as_deref()),
      },
    });
  }
  for stop in stops {
    if let Some(position) = coordinates(stop.lat, stop.lng) {
      let trip_ids = stop.trip_ids_json.as_deref().and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
      features.push(Feature {
        kind: "Feature",
        geometry: Geometry::Point(position),
        properties: FeatureProperties::Stop {
          route_id: stop.route_id,
          stop_id: stop.stop_id,
          stop_name: stop.stop_name,
          trip_ids,
          arrival_times: stop.arrival_times,
          departure_times: stop.departure_times,
        },
      });
    }
  }
  features
}

fn group_features_by_route<'a>(routes: &'a HashMap<String, MapRoute>, paths: RoutePaths, segments: RouteSegments, stops: Vec<StopPoint>) -> BTreeMap<String, Vec<Feature<'a>>> {
  let mut by_route: BTreeMap<String, Vec<Feature<'a>>> = routes.keys().map(|route_id| (route_id.clone(), Vec::new())).collect();
  for feature in build_features(routes, paths, segments, stops) {
    let route_id = match &feature.properties {
      FeatureProperties::ShapeRoute { route_id, .. } | FeatureProperties::SegmentRoute { route_id, .. } | FeatureProperties::Stop { route_id, .. } => route_id.clone(),
    };
    by_route.entry(route_id).or_default().push(feature);
  }
  by_route
}

fn merge_route_features(by_route: BTreeMap<String, Vec<serde_json::Value>>) -> Vec<serde_json::Value> {
  let total: usize = by_route.values().map(Vec::len).sum();
  let mut route_features = Vec::with_capacity(total);
  let mut stop_features = Vec::new();
  for features in by_route.into_values() {
    for feature in features {
      if feature.get("properties").and_then(|properties| properties.get("kind")).and_then(serde_json::Value::as_str) == Some("route") {
        route_features.push(feature);
      } else {
        stop_features.push(feature);
      }
    }
  }
  route_features.extend(stop_features);
  route_features
}

async fn query_filtered_routes(db: &D1Database, provider: &str, route_ids: &[String], active_services: &str, has_shapes: bool) -> Result<Option<MapData>> {
  let query_sql = route_trips_sql(provider, active_services, has_shapes);
  let route_ids_json = serde_json::to_string(route_ids)?;
  // Validate the requested route_ids via primary key before scanning trips, and fetch active trip_id/shape_id once for reuse.
  let route_result = prepare_ids_query(db, &query_sql, &route_ids_json)?.all().await?;
  if !route_result.success() {
    return Ok(None);
  }
  let matched_routes = route_result.results::<RouteWithTrips>()?;
  if matched_routes.is_empty() {
    return Ok(Some(MapData { routes: Vec::new(), paths: BTreeMap::new(), segments: BTreeMap::new(), stops: Vec::new() }));
  }

  let mut route_rows = Vec::with_capacity(matched_routes.len());
  let mut shape_routes: BTreeMap<String, Vec<String>> = BTreeMap::new();
  let mut trips_by_shaped_route: HashMap<String, Vec<String>> = HashMap::new();
  let mut shaped_trip_pairs: Vec<(String, String)> = Vec::new();
  let mut fallback_trip_pairs: Vec<(String, String)> = Vec::new();

  for route in matched_routes {
    let trip_ids: Vec<String> = serde_json::from_str(&route.trip_ids_json)?;
    let shape_ids: Vec<String> = if has_shapes { serde_json::from_str(&route.shape_ids_json)? } else { Vec::new() };
    if !shape_ids.is_empty() {
      for shape_id in shape_ids {
        shape_routes.entry(shape_id).or_default().push(route.route_id.clone());
      }
      for trip_id in &trip_ids {
        shaped_trip_pairs.push((route.route_id.clone(), trip_id.clone()));
      }
      trips_by_shaped_route.insert(route.route_id.clone(), trip_ids);
    } else {
      for trip_id in trip_ids {
        fallback_trip_pairs.push((route.route_id.clone(), trip_id));
      }
    }
    route_rows.push(MapRoute { route_id: route.route_id, route_short_name: route.route_short_name, route_long_name: route.route_long_name, route_color: route.route_color });
  }

  if !shape_routes.is_empty() {
    let shape_ids: Vec<&str> = shape_routes.keys().map(String::as_str).collect();
    let shape_ids_json = serde_json::to_string(&shape_ids)?;
    let shaped_trips_json = serde_json::to_string(&shaped_trip_pairs)?;
    let mut statements = vec![prepare_ids_query(db, SHAPES_BY_ROUTE_SQL, &shape_ids_json)?, prepare_ids_query(db, STOPS_BY_ROUTE_SQL, &shaped_trips_json)?];
    let has_unshaped = !fallback_trip_pairs.is_empty();
    if has_unshaped {
      let unshaped_trips_json = serde_json::to_string(&fallback_trip_pairs)?;
      statements.push(prepare_ids_query(db, SEGMENTS_AND_STOPS_BY_ROUTE_SQL, &unshaped_trips_json)?);
    }
    let mut second_batch = db.batch(statements).await?.into_iter();
    let geometry_result = take_result(&mut second_batch, "geometry")?;
    let stop_result = take_result(&mut second_batch, "stops")?;
    let unshaped_result = if has_unshaped { Some(take_result(&mut second_batch, "segments_and_stops")?) } else { None };
    if !geometry_result.success() || !stop_result.success() || unshaped_result.as_ref().is_some_and(|r| !r.success()) {
      return Ok(None);
    }
    let paths = collect_paths_by_shape(geometry_result.results::<ShapePoint>()?, &shape_routes);
    let mut stops = stop_result.results::<StopPoint>()?;
    let mut segments = if let Some(unshaped) = unshaped_result {
      let (unshaped_segments, unshaped_stops) = collect_segments_and_stops(unshaped.results::<SegmentOrStop>()?);
      stops.extend(unshaped_stops);
      stops.sort_by(|a, b| a.route_id.cmp(&b.route_id).then_with(|| a.stop_id.cmp(&b.stop_id)));
      unshaped_segments
    } else {
      BTreeMap::new()
    };

    let routes_with_paths: HashSet<&str> = paths.iter().filter(|(_, path)| path.points.len() >= 2).map(|((rid, _), _)| rid.as_str()).collect();
    let missing_shape_trip_pairs: Vec<(&str, &str)> =
      trips_by_shaped_route.iter().filter(|(rid, _)| !routes_with_paths.contains(rid.as_str())).flat_map(|(rid, tids)| tids.iter().map(move |tid| (rid.as_str(), tid.as_str()))).collect();
    if !missing_shape_trip_pairs.is_empty() {
      let missing_trips_json = serde_json::to_string(&missing_shape_trip_pairs)?;
      let fallback_result = prepare_ids_query(db, SEGMENTS_AND_STOPS_BY_ROUTE_SQL, &missing_trips_json)?.all().await?;
      if !fallback_result.success() {
        return Ok(None);
      }
      segments.extend(collect_segments_and_stops(fallback_result.results::<SegmentOrStop>()?).0);
    }
    Ok(Some(MapData { routes: route_rows, paths, segments, stops }))
  } else {
    let all_trips_json = serde_json::to_string(&fallback_trip_pairs)?;
    let merged_result = prepare_ids_query(db, SEGMENTS_AND_STOPS_BY_ROUTE_SQL, &all_trips_json)?.all().await?;
    if !merged_result.success() {
      return Ok(None);
    }
    let (segments, stops) = collect_segments_and_stops(merged_result.results::<SegmentOrStop>()?);
    Ok(Some(MapData { routes: route_rows, paths: BTreeMap::new(), segments, stops }))
  }
}

pub async fn handle(req: &Request, env: &Env, ctx: Context, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return crate::method_not_allowed("GET", true);
  }

  let today = ServiceDate::from_naive_date(crate::current_time(env)?.with_timezone(&crate::default_timezone(env)?).date_naive());
  let max_routes = crate::map_max_routes(env)?;
  let MapParams { route_ids, date } = match parse_params(url, today, max_routes) {
    Ok(params) => params,
    Err((status, message)) => return crate::json_error(status, &message),
  };
  let cache_control = crate::feed_cache_control(env)?;

  if route_ids.len() > 1 {
    let db = match database::for_provider(env, provider) {
      Ok(db) => db,
      Err(_) => return crate::json_error(404, "Database is unavailable for this provider"),
    };

    let cache_lookups = join_all(route_ids.iter().map(|route_id| {
      let key = route_cache_key(url, provider, Some(route_id), date);
      async move {
        let response = Cache::default().get(&key, false).await.ok().flatten();
        (route_id.as_str(), response)
      }
    }))
    .await;

    let mut features_by_route: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    let mut missing_route_ids = Vec::new();
    for (route_id, cached) in cache_lookups {
      if let Some(mut response) = cached
        && let Ok(collection) = response.json::<CachedFeatureCollection>().await
      {
        features_by_route.insert(route_id.to_owned(), collection.features);
      } else {
        missing_route_ids.push(route_id.to_owned());
      }
    }

    if !missing_route_ids.is_empty() {
      let active_services = active_services_cte(date, crate::data::schema::has_column(provider, "calendar_dates", "service_id"));
      let has_shapes = crate::data::schema::has_column(provider, "shapes", "shape_id") && crate::data::schema::has_column(provider, "trips", "shape_id");
      let Some(MapData { routes, paths, segments, stops }) = query_filtered_routes(&db, provider, &missing_route_ids, &active_services, has_shapes).await? else {
        return crate::json_error(503, "Map data is unavailable");
      };
      let routes_map: HashMap<String, MapRoute> = routes.into_iter().map(|route| (route.route_id.clone(), route)).collect();
      let fetched_by_route = group_features_by_route(&routes_map, paths, segments, stops);
      let mut cache_writes = Vec::with_capacity(fetched_by_route.len());
      for (route_id, route_features) in fetched_by_route {
        let route_values: Vec<serde_json::Value> = route_features.iter().map(serde_json::to_value).collect::<std::result::Result<_, _>>()?;
        let mut route_response = Response::from_json(&FeatureCollection { kind: "FeatureCollection", features: &route_values })?;
        route_response.headers_mut().set("Cache-Control", &cache_control)?;
        cache_writes.push((route_cache_key(url, provider, Some(&route_id), date), route_response));
        features_by_route.insert(route_id, route_values);
      }
      cache_responses(&ctx, cache_writes);
    }

    if features_by_route.is_empty() {
      return crate::json_error(404, "Not found route_id");
    }
    let features = merge_route_features(features_by_route);
    let mut response = Response::from_json(&FeatureCollection { kind: "FeatureCollection", features })?;
    response.headers_mut().set("Cache-Control", &cache_control)?;
    return Ok(response);
  }

  let cache_key = route_cache_key(url, provider, route_ids.first().map(String::as_str), date);
  if let Ok(Some(response)) = Cache::default().get(&cache_key, false).await {
    return Ok(response);
  }

  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return crate::json_error(404, "Database is unavailable for this provider"),
  };
  let active_services = active_services_cte(date, crate::data::schema::has_column(provider, "calendar_dates", "service_id"));
  let has_shapes = crate::data::schema::has_column(provider, "shapes", "shape_id") && crate::data::schema::has_column(provider, "trips", "shape_id");

  let MapData { routes: route_result, paths, segments, stops } = if !route_ids.is_empty() {
    let Some(data) = query_filtered_routes(&db, provider, &route_ids, &active_services, has_shapes).await? else {
      return crate::json_error(503, "Map data is unavailable");
    };
    if data.routes.is_empty() {
      return crate::json_error(404, "Not found route_id");
    }
    data
  } else {
    let route_sql = route_select_sql(provider);
    if has_shapes {
      let shape_sql = SHAPES_SQL.replace("__ACTIVE_SERVICES__", &active_services);
      let stop_sql = STOPS_SQL.replace("__ACTIVE_SERVICES__", &active_services);
      let mut batch = db.batch(vec![db.prepare(&route_sql), db.prepare(&shape_sql), db.prepare(&stop_sql)]).await?.into_iter();
      let route_result = take_result(&mut batch, "routes")?;
      let geometry_result = take_result(&mut batch, "geometry")?;
      let stop_result = take_result(&mut batch, "stops")?;
      if !route_result.success() || !geometry_result.success() || !stop_result.success() {
        return crate::json_error(503, "Map data is unavailable");
      }
      let paths = collect_paths(geometry_result.results::<ShapePoint>()?)?;
      let segments = if paths.is_empty() {
        let fallback_sql = SEGMENTS_AND_STOPS_SQL.replace("__ACTIVE_SERVICES__", &active_services);
        let fallback_result = db.prepare(&fallback_sql).all().await?;
        if !fallback_result.success() {
          return crate::json_error(503, "Map data is unavailable");
        }
        collect_segments_and_stops(fallback_result.results::<SegmentOrStop>()?).0
      } else {
        BTreeMap::new()
      };
      MapData { routes: route_result.results::<MapRoute>()?, paths, segments, stops: stop_result.results::<StopPoint>()? }
    } else {
      let merged_sql = SEGMENTS_AND_STOPS_SQL.replace("__ACTIVE_SERVICES__", &active_services);
      let mut batch = db.batch(vec![db.prepare(&route_sql), db.prepare(&merged_sql)]).await?.into_iter();
      let route_result = take_result(&mut batch, "routes")?;
      let merged_result = take_result(&mut batch, "segments_and_stops")?;
      if !route_result.success() || !merged_result.success() {
        return crate::json_error(503, "Map data is unavailable");
      }
      let (segments, stops) = collect_segments_and_stops(merged_result.results::<SegmentOrStop>()?);
      MapData { routes: route_result.results::<MapRoute>()?, paths: BTreeMap::new(), segments, stops }
    }
  };

  let routes: HashMap<String, MapRoute> = route_result.into_iter().map(|route| (route.route_id.clone(), route)).collect();
  let features = build_features(&routes, paths, segments, stops);
  let mut response = Response::from_json(&FeatureCollection { kind: "FeatureCollection", features })?;
  crate::cache_response(&ctx, cache_key, &mut response, &cache_control)?;
  Ok(response)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/map/mod.rs"]
mod tests;
