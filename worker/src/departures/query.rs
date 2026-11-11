use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1Result, Result};

use super::Parameters;
use super::schedule::{Calendar, Exception, ParsedService, Template};
use crate::data::schema;

const QUERY: &str = include_str!("sql/query.sql");

fn decode<T: DeserializeOwned>(result: D1Result) -> Result<Vec<T>> {
  if !result.success() {
    return Err(worker::Error::RustError("Departure query failed".into()));
  }
  // worker 0.8.x unwraps typed D1 deserialization internally. Decode plain JSON
  // first so nullable/malformed feed fields return an error instead of a panic.
  Ok(result.results::<serde_json::Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<T>, _>>()?)
}

pub(super) async fn load(db: &D1Database, provider: &str, params: &Parameters) -> Result<Vec<Template>> {
  let has = |table: &str, column: &str| schema::has_column(provider, table, column);
  let frequencies = has("frequencies", "trip_id");
  let frequency_columns = if frequencies {
    format!(
      "(SELECT departure_time FROM stop_times first WHERE first.trip_id = st.trip_id ORDER BY stop_sequence LIMIT 1) AS first_departure_time, \
       f.start_time, f.end_time, f.headway_secs, f.trip_id AS frequency_trip_id, {} AS exact_times",
      if has("frequencies", "exact_times") { "f.exact_times" } else { "NULL" },
    )
  } else {
    "NULL AS first_departure_time, NULL AS start_time, NULL AS end_time, NULL AS headway_secs, NULL AS frequency_trip_id, NULL AS exact_times".into()
  };
  let stop_headsign = has("stop_times", "stop_headsign");
  let trip_headsign = has("trips", "trip_headsign");
  let headsign = match (stop_headsign, trip_headsign) {
    (true, true) => "COALESCE(NULLIF(st.stop_headsign, ''), t.trip_headsign)",
    (true, false) => "st.stop_headsign",
    (false, true) => "t.trip_headsign",
    (false, false) => "NULL",
  };
  let route_columns = format!(
    "{}, {}, {}, {}",
    if has("routes", "route_short_name") { "r.route_short_name" } else { "NULL AS route_short_name" },
    if has("routes", "route_long_name") { "r.route_long_name" } else { "NULL AS route_long_name" },
    if has("routes", "route_color") { "r.route_color" } else { "NULL AS route_color" },
    if has("routes", "route_text_color") { "r.route_text_color" } else { "NULL AS route_text_color" },
  );
  let sql = QUERY
    .replace("__ROUTE_COLUMNS__", &route_columns)
    .replace("__HEADSIGN__", headsign)
    .replace("__FREQUENCY_COLUMNS__", &frequency_columns)
    .replace("__FREQUENCY_JOIN__", if frequencies { "LEFT JOIN frequencies f ON f.trip_id = t.trip_id" } else { "" })
    .replace(
      "__AGENCY_JOIN__",
      if has("agency", "agency_id") {
        "LEFT JOIN agency a ON a.agency_id = r.agency_id OR ((r.agency_id IS NULL OR r.agency_id = '') AND (SELECT COUNT(*) FROM agency) = 1)"
      } else {
        "LEFT JOIN agency a ON 1 = 1"
      },
    )
    .replace("__PICKUP_FILTER__", if has("stop_times", "pickup_type") { "AND COALESCE(st.pickup_type, 0) <> 1" } else { "" });
  let result = db
    .prepare(sql)
    .bind(&[params.stop_id.clone().into(), params.route_id.clone().map(JsValue::from).unwrap_or(JsValue::NULL), params.direction_id.map(JsValue::from).unwrap_or(JsValue::NULL)])?
    .all()
    .await?;
  let mut templates = decode::<Template>(result)?;
  if templates.is_empty() {
    return Ok(templates);
  }
  let service_ids = serde_json::to_string(&templates.iter().map(|template| &template.service_id).collect::<HashSet<_>>())?;
  let (calendars_result, exceptions_result) = if has("calendar_dates", "service_id") {
    let mut results = db
      .batch(vec![
        db.prepare("SELECT * FROM calendar WHERE service_id IN (SELECT value FROM json_each(?1))").bind(&[service_ids.clone().into()])?,
        db.prepare("SELECT * FROM calendar_dates WHERE service_id IN (SELECT value FROM json_each(?1))").bind(&[service_ids.into()])?,
      ])
      .await?;
    let exceptions = results.pop().ok_or_else(|| worker::Error::RustError("Missing calendar_dates result".into()))?;
    let calendars = results.pop().ok_or_else(|| worker::Error::RustError("Missing calendar result".into()))?;
    (calendars, Some(exceptions))
  } else {
    let calendars = db.prepare("SELECT * FROM calendar WHERE service_id IN (SELECT value FROM json_each(?1))").bind(&[service_ids.into()])?.all().await?;
    (calendars, None)
  };
  let calendars: Vec<Calendar> = decode(calendars_result)?;
  let exceptions: Vec<Exception> = match exceptions_result {
    Some(res) => decode(res)?,
    None => Vec::new(),
  };
  let mut calendars_by_service: HashMap<String, Vec<Calendar>> = HashMap::new();
  for calendar in calendars {
    calendars_by_service.entry(calendar.service_id.clone()).or_default().push(calendar);
  }
  let mut exceptions_by_service: HashMap<String, Vec<Exception>> = HashMap::new();
  for exception in exceptions {
    exceptions_by_service.entry(exception.service_id.clone()).or_default().push(exception);
  }
  let mut services: HashMap<String, Arc<ParsedService>> = HashMap::new();
  for template in &templates {
    if services.contains_key(&template.service_id) {
      continue;
    }
    let c = calendars_by_service.get(&template.service_id).map(|v| v.as_slice()).unwrap_or(&[]);
    let e = exceptions_by_service.get(&template.service_id).map(|v| v.as_slice()).unwrap_or(&[]);
    if let Some(parsed) = ParsedService::build(c, e).map_err(|message| worker::Error::RustError(message.into()))? {
      services.insert(template.service_id.clone(), Arc::new(parsed));
    }
  }
  for template in &mut templates {
    template.service = services.get(&template.service_id).cloned();
  }
  Ok(templates)
}
