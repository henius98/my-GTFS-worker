use std::collections::{HashMap, HashSet};

use serde::{Deserialize, de::DeserializeOwned};
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1Result, Result};

use super::Parameters;
use super::schedule::{Calendar, Exception, Service, Template};

const QUERY: &str = include_str!("query.sql");

#[derive(Deserialize)]
struct Column {
  table_name: String,
  name: String,
}

fn decode<T: DeserializeOwned>(result: D1Result) -> Result<Vec<T>> {
  if !result.success() {
    return Err(worker::Error::RustError("Departure query failed".into()));
  }
  // worker 0.8.x unwraps typed D1 deserialization internally. Decode plain JSON
  // first so nullable/malformed feed fields return an error instead of a panic.
  Ok(result.results::<serde_json::Value>()?.into_iter().map(serde_json::from_value).collect::<std::result::Result<Vec<T>, _>>()?)
}

pub(super) async fn load(db: &D1Database, params: &Parameters) -> Result<Vec<Template>> {
  let result = db
    .prepare(
      "SELECT 'trips' AS table_name, name FROM pragma_table_info('trips') \
       UNION ALL SELECT 'agency', name FROM pragma_table_info('agency') \
       UNION ALL SELECT 'frequencies', name FROM pragma_table_info('frequencies') \
       UNION ALL SELECT 'calendar_dates', name FROM pragma_table_info('calendar_dates') \
       UNION ALL SELECT 'stop_times', name FROM pragma_table_info('stop_times')",
    )
    .all()
    .await?;
  let columns: HashSet<_> = decode::<Column>(result)?.into_iter().map(|column| (column.table_name, column.name)).collect();
  let has = |table: &str, column: &str| columns.contains(&(table.to_owned(), column.to_owned()));
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
  let sql = QUERY
    .replace("__HEADSIGN__", if has("trips", "trip_headsign") { "t.trip_headsign" } else { "NULL" })
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
  let result = db.prepare("SELECT * FROM calendar WHERE service_id IN (SELECT value FROM json_each(?1))").bind(&[service_ids.clone().into()])?.all().await?;
  let mut services: HashMap<String, Service> = HashMap::new();
  for calendar in decode::<Calendar>(result)? {
    services.entry(calendar.service_id.clone()).or_default().calendars.push(calendar);
  }
  if has("calendar_dates", "service_id") {
    let result = db.prepare("SELECT * FROM calendar_dates WHERE service_id IN (SELECT value FROM json_each(?1))").bind(&[service_ids.into()])?.all().await?;
    for exception in decode::<Exception>(result)? {
      services.entry(exception.service_id.clone()).or_default().exceptions.push(exception);
    }
  }
  for template in &mut templates {
    template.service = services.get(&template.service_id).cloned().unwrap_or_default();
  }
  Ok(templates)
}
