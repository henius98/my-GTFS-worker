use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use worker::{D1Result, D1Type, Result};

use super::schedule::{Calendar, Exception, ParsedService, Template};
use super::{Parameters, Stop};
use crate::data::schema;
use crate::database::ProviderDb;

const STOP_QUERY: &str = include_str!("sql/stop.sql");
const QUERY: &str = include_str!("sql/query.sql");
const FREQUENCIES_QUERY: &str = include_str!("sql/query_frequencies.sql");
const AGENCY_JOIN_SQL: &str = include_str!("sql/agency_join.sql");
const CALENDAR_QUERY: &str = include_str!("sql/calendar.sql");
const CALENDAR_DATES_QUERY: &str = include_str!("sql/calendar_dates.sql");

fn decode<T: DeserializeOwned>(result: D1Result) -> Result<Vec<T>> {
  if !result.success() {
    return Err(worker::Error::RustError("Departure query failed".into()));
  }
  // worker 0.9+ returns typed D1 deserialization errors as Result instead of panicking.
  result.results::<T>()
}

pub(super) async fn load_stop_and_templates(db: &ProviderDb, provider: &str, params: &Parameters) -> Result<Option<(Stop, Vec<Template>)>> {
  let has = |table: &str, column: &str| schema::has_column(provider, table, column);
  let frequencies = has("frequencies", "trip_id");
  let stop_headsign = has("stop_times", "stop_headsign");
  let trip_headsign = has("trips", "trip_headsign");
  let headsign = match (stop_headsign, trip_headsign) {
    (true, true) => "COALESCE(NULLIF(st.stop_headsign, ''), t.trip_headsign)",
    (true, false) => "st.stop_headsign",
    (false, true) => "t.trip_headsign",
    (false, false) => "NULL",
  };
  let route_columns = schema::optional_columns(provider, "routes", "r.", &["route_short_name", "route_long_name", "route_color", "route_text_color"]);
  let base_sql = if frequencies { FREQUENCIES_QUERY } else { QUERY };
  let sql = base_sql
    .replace("__ROUTE_COLUMNS__", &route_columns)
    .replace("__HEADSIGN__", headsign)
    .replace("__EXACT_TIMES__", if has("frequencies", "exact_times") { "f.exact_times" } else { "NULL" })
    .replace("__AGENCY_JOIN__", if has("agency", "agency_id") { AGENCY_JOIN_SQL.trim_end() } else { "LEFT JOIN agency a ON 1 = 1" })
    .replace("__PICKUP_FILTER__", if has("stop_times", "pickup_type") { "AND COALESCE(st.pickup_type, 0) <> 1" } else { "" });

  let stop_stmt = db.prepare(STOP_QUERY).bind_refs(&D1Type::Text(&params.stop_id))?;
  let templates_stmt =
    db.prepare(sql).bind_refs(&[D1Type::Text(&params.stop_id), params.route_id.as_deref().map_or(D1Type::Null, D1Type::Text), params.direction_id.map_or(D1Type::Null, D1Type::Integer)])?;
  let mut initial_results = db.batch(vec![("stop", stop_stmt), ("templates", templates_stmt)]).await?.into_iter();
  let stop_result = initial_results.next().ok_or_else(|| worker::Error::RustError("Missing stop result".into()))?;
  let templates_result = initial_results.next().ok_or_else(|| worker::Error::RustError("Missing templates result".into()))?;

  let Some(stop) = decode::<Stop>(stop_result)?.into_iter().next() else {
    return Ok(None);
  };
  let mut templates = decode::<Template>(templates_result)?;
  if templates.is_empty() {
    return Ok(Some((stop, templates)));
  }

  let service_ids = serde_json::to_string(&templates.iter().map(|template| &template.service_id).collect::<HashSet<_>>())?;
  let service_ids_arg = D1Type::Text(&service_ids);
  let (calendars_result, exceptions_result) = if has("calendar_dates", "service_id") {
    let mut results =
      db.batch(vec![("calendar", db.prepare(CALENDAR_QUERY).bind_refs(&service_ids_arg)?), ("calendar_dates", db.prepare(CALENDAR_DATES_QUERY).bind_refs(&service_ids_arg)?)]).await?.into_iter();
    let calendars = results.next().ok_or_else(|| worker::Error::RustError("Missing calendar result".into()))?;
    let exceptions = results.next().ok_or_else(|| worker::Error::RustError("Missing calendar_dates result".into()))?;
    (calendars, Some(exceptions))
  } else {
    let stmt = db.prepare(CALENDAR_QUERY).bind_refs(&service_ids_arg)?;
    let calendars = db.all("calendar", &stmt).await?;
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
    let c = calendars_by_service.get(&template.service_id).map_or(&[][..], Vec::as_slice);
    let e = exceptions_by_service.get(&template.service_id).map_or(&[][..], Vec::as_slice);
    if let Some(parsed) = ParsedService::build(c, e).map_err(|message| worker::Error::RustError(message.into()))? {
      services.insert(template.service_id.clone(), Arc::new(parsed));
    }
  }
  for template in &mut templates {
    template.service = services.get(&template.service_id).cloned();
  }
  Ok(Some((stop, templates)))
}
