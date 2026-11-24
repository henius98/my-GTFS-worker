use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, NaiveDate, Weekday};
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
  lat: Option<f64>,
  lng: Option<f64>,
}

#[derive(Deserialize)]
struct StopPoint {
  route_id: Option<String>,
  stop_id: String,
  stop_name: Option<String>,
  lat: Option<f64>,
  lng: Option<f64>,
  arrival_times: Option<String>,
  departure_times: Option<String>,
}

#[derive(Deserialize)]
struct StopSegment {
  route_id: Option<String>,
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
struct FeatureCollection<'a> {
  #[serde(rename = "type")]
  kind: &'static str,
  features: Vec<Feature<'a>>,
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
  ShapeRoute { route_id: String, shape_id: String, geometry_source: &'static str, route_short_name: Option<&'a str>, route_long_name: Option<&'a str>, route_color: Option<&'a str> },
  #[serde(rename = "route")]
  SegmentRoute { route_id: String, geometry_source: &'static str, route_short_name: Option<&'a str>, route_long_name: Option<&'a str>, route_color: Option<&'a str> },
  #[serde(rename = "stop")]
  Stop { route_id: String, stop_id: String, stop_name: Option<String>, arrival_times: Option<String>, departure_times: Option<String> },
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
  route_id: Option<String>,
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

fn parse_params(url: &Url, default_date: ServiceDate) -> std::result::Result<MapParams, (u16, &'static str)> {
  let mut route_id = None;
  let mut date = None;
  for (key, value) in url.query_pairs() {
    match key.as_ref() {
      "route_id" => {
        let trimmed = value.trim();
        if route_id.is_some() || trimmed.is_empty() || trimmed.len() > crate::MAX_IDENTIFIER_BYTES {
          return Err((400, "Expected at most one non-empty route_id"));
        }
        route_id = Some(trimmed.to_owned());
      }
      "date" => {
        if date.is_some() {
          return Err((400, "Expected at most one date"));
        }
        date = Some(parse_service_date(&value)?);
      }
      _ => return Err((400, "Unexpected query parameter; expected route_id and/or date")),
    }
  }
  Ok(MapParams { route_id, date: date.unwrap_or(default_date) })
}

fn prepare_ids_query(db: &D1Database, sql: &str, ids_json: &str) -> Result<D1PreparedStatement> {
  db.prepare(sql).bind_refs(&D1Type::Text(ids_json))
}

fn take_result(results: &mut std::vec::IntoIter<D1Result>, label: &str) -> Result<D1Result> {
  results.next().ok_or_else(|| worker::Error::RustError(format!("Missing {label} result")))
}

fn collect_paths(shape_points: Vec<ShapePoint>, default_route_id: Option<&str>) -> BTreeMap<(String, String), Vec<[f64; 2]>> {
  let mut paths: BTreeMap<(String, String), Vec<[f64; 2]>> = BTreeMap::new();
  for point in shape_points {
    let Some(position) = coordinates(point.lat, point.lng) else {
      continue;
    };
    let Some(route_id) = point.route_id.or_else(|| default_route_id.map(str::to_owned)) else {
      continue;
    };
    paths.entry((route_id, point.shape_id)).or_default().push(position);
  }
  paths
}

type RouteSegments = BTreeMap<String, Vec<[[f64; 2]; 2]>>;

fn collect_segments_and_stops(rows: Vec<SegmentOrStop>, default_route_id: Option<&str>) -> (RouteSegments, Vec<StopPoint>) {
  let mut segments: RouteSegments = BTreeMap::new();
  let mut stops = Vec::new();
  for row in rows {
    match row {
      SegmentOrStop::Segment(segment) => {
        if let (Some(from), Some(to), Some(route_id)) =
          (coordinates(segment.lat, segment.lng), coordinates(segment.to_lat, segment.to_lng), segment.route_id.or_else(|| default_route_id.map(str::to_owned)))
        {
          segments.entry(route_id).or_default().push([from, to]);
        }
      }
      SegmentOrStop::Stop(stop) => stops.push(stop),
    }
  }
  (segments, stops)
}

pub async fn handle(req: &Request, env: &Env, ctx: Context, url: &Url, provider: &str) -> Result<Response> {
  if req.method() != Method::Get {
    return crate::method_not_allowed("GET", true);
  }

  let today = ServiceDate::from_naive_date(crate::current_time(env)?.with_timezone(&crate::default_timezone(env)?).date_naive());
  let MapParams { route_id, date } = match parse_params(url, today) {
    Ok(params) => params,
    Err((status, message)) => return crate::json_error(status, message),
  };

  let mut cache_url = crate::canonical_cache_url(url, provider, "map");
  if let Some(route_id) = &route_id {
    cache_url.query_pairs_mut().append_pair("route_id", route_id);
  }
  cache_url.query_pairs_mut().append_pair("date", &date.yyyymmdd.to_string());
  let cache_key = cache_url.to_string();
  if let Ok(Some(response)) = Cache::default().get(&cache_key, false).await {
    return Ok(response);
  }

  let db = match database::for_provider(env, provider) {
    Ok(db) => db,
    Err(_) => return crate::json_error(404, "Database is unavailable for this provider"),
  };
  let filter = route_id.as_deref();
  let active_services = active_services_cte(date, crate::data::schema::has_column(provider, "calendar_dates", "service_id"));
  let has_shapes = crate::data::schema::has_column(provider, "shapes", "shape_id") && crate::data::schema::has_column(provider, "trips", "shape_id");

  let (route_result, paths, segments, stops) = if let Some(route_id) = filter {
    let query_sql = route_trips_sql(provider, &active_services, has_shapes);
    // Validate the requested route_id via primary key before scanning trips, and fetch active trip_id/shape_id once for reuse.
    let route_result = db.prepare(&query_sql).bind_refs(&D1Type::Text(route_id))?.all().await?;
    if !route_result.success() {
      return crate::json_error(503, "Map data is unavailable");
    }
    let Some(RouteWithTrips { route_id: id, route_short_name, route_long_name, route_color, trip_ids_json, shape_ids_json }) = route_result.results::<RouteWithTrips>()?.into_iter().next() else {
      return crate::json_error(404, "Not found route_id");
    };
    let route_rows = vec![MapRoute { route_id: id, route_short_name, route_long_name, route_color }];
    if has_shapes && shape_ids_json != "[]" {
      let geometry_stmt = prepare_ids_query(&db, SHAPES_BY_ROUTE_SQL, &shape_ids_json)?;
      let stop_stmt = prepare_ids_query(&db, STOPS_BY_ROUTE_SQL, &trip_ids_json)?;
      let mut second_batch = db.batch(vec![geometry_stmt, stop_stmt]).await?.into_iter();
      let geometry_result = take_result(&mut second_batch, "geometry")?;
      let stop_result = take_result(&mut second_batch, "stops")?;
      if !geometry_result.success() || !stop_result.success() {
        return crate::json_error(503, "Map data is unavailable");
      }
      let paths = collect_paths(geometry_result.results::<ShapePoint>()?, Some(route_id));
      let segments = if paths.is_empty() {
        let fallback_result = prepare_ids_query(&db, SEGMENTS_AND_STOPS_BY_ROUTE_SQL, &trip_ids_json)?.all().await?;
        if !fallback_result.success() {
          return crate::json_error(503, "Map data is unavailable");
        }
        collect_segments_and_stops(fallback_result.results::<SegmentOrStop>()?, Some(route_id)).0
      } else {
        BTreeMap::new()
      };
      (route_rows, paths, segments, stop_result.results::<StopPoint>()?)
    } else {
      let merged_result = prepare_ids_query(&db, SEGMENTS_AND_STOPS_BY_ROUTE_SQL, &trip_ids_json)?.all().await?;
      if !merged_result.success() {
        return crate::json_error(503, "Map data is unavailable");
      }
      let (segments, stops) = collect_segments_and_stops(merged_result.results::<SegmentOrStop>()?, Some(route_id));
      (route_rows, BTreeMap::new(), segments, stops)
    }
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
      let paths = collect_paths(geometry_result.results::<ShapePoint>()?, None);
      let segments = if paths.is_empty() {
        let fallback_sql = SEGMENTS_AND_STOPS_SQL.replace("__ACTIVE_SERVICES__", &active_services);
        let fallback_result = db.prepare(&fallback_sql).all().await?;
        if !fallback_result.success() {
          return crate::json_error(503, "Map data is unavailable");
        }
        collect_segments_and_stops(fallback_result.results::<SegmentOrStop>()?, None).0
      } else {
        BTreeMap::new()
      };
      (route_result.results::<MapRoute>()?, paths, segments, stop_result.results::<StopPoint>()?)
    } else {
      let merged_sql = SEGMENTS_AND_STOPS_SQL.replace("__ACTIVE_SERVICES__", &active_services);
      let mut batch = db.batch(vec![db.prepare(&route_sql), db.prepare(&merged_sql)]).await?.into_iter();
      let route_result = take_result(&mut batch, "routes")?;
      let merged_result = take_result(&mut batch, "segments_and_stops")?;
      if !route_result.success() || !merged_result.success() {
        return crate::json_error(503, "Map data is unavailable");
      }
      let (segments, stops) = collect_segments_and_stops(merged_result.results::<SegmentOrStop>()?, None);
      (route_result.results::<MapRoute>()?, BTreeMap::new(), segments, stops)
    }
  };

  let routes: HashMap<String, MapRoute> = route_result.into_iter().map(|route| (route.route_id.clone(), route)).collect();

  let mut features = Vec::with_capacity(paths.len() + segments.len() + stops.len());
  for ((route_id, shape_id), points) in paths {
    if points.len() < 2 {
      continue;
    }
    let route = routes.get(&route_id);
    features.push(Feature {
      kind: "Feature",
      geometry: Geometry::LineString(points),
      properties: FeatureProperties::ShapeRoute {
        route_id,
        shape_id,
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
    let Some(route_id) = stop.route_id.or_else(|| filter.map(str::to_owned)) else {
      continue;
    };
    if let Some(position) = coordinates(stop.lat, stop.lng) {
      features.push(Feature {
        kind: "Feature",
        geometry: Geometry::Point(position),
        properties: FeatureProperties::Stop { route_id, stop_id: stop.stop_id, stop_name: stop.stop_name, arrival_times: stop.arrival_times, departure_times: stop.departure_times },
      });
    }
  }

  let mut response = Response::from_json(&FeatureCollection { kind: "FeatureCollection", features })?;
  let cache_control = crate::feed_cache_control(env)?;
  crate::cache_response(&ctx, cache_key, &mut response, &cache_control)?;
  Ok(response)
}

#[cfg(test)]
#[path = "../../../test/unit/worker/map/mod.rs"]
mod tests;
