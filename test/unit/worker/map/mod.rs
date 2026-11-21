use super::*;

#[test]
fn coordinates_validates_bounds_and_finiteness() {
  assert_eq!(coordinates(Some(3.1), Some(101.7)), Some([101.7, 3.1]));
  assert_eq!(coordinates(Some(-90.0), Some(180.0)), Some([180.0, -90.0]));
  for (lat, lng) in [(None, Some(101.7)), (Some(90.1), Some(101.7)), (Some(3.1), Some(-180.1)), (Some(f64::NAN), Some(101.7)), (Some(3.1), Some(f64::INFINITY))] {
    assert_eq!(coordinates(lat, lng), None);
  }
}

#[test]
fn parse_route_id_validates_query_parameters() -> std::result::Result<(), Box<dyn std::error::Error>> {
  assert_eq!(parse_route_id(&Url::parse("https://example.com/ktmb/map")?), Ok(None));
  assert_eq!(parse_route_id(&Url::parse("https://example.com/ktmb/map?route_id=KJL")?), Ok(Some("KJL".to_owned())));
  let long = "x".repeat(257);
  for query in ["?route_id=", "?route_id=%20%20", "?other=1", "?route_id=A&route_id=B", &format!("?route_id={long}")] {
    let url = Url::parse(&format!("https://example.com/ktmb/map{query}"))?;
    assert_eq!(parse_route_id(&url), Err((400, "Expected at most one non-empty route_id")), "{query}");
  }
  Ok(())
}

#[test]
fn route_select_sql_handles_missing_route_color() {
  assert_eq!(route_select_sql("rapid-bus-mrtfeeder", true), "SELECT route_id, route_short_name, route_long_name, NULL AS route_color FROM routes WHERE route_id = ?1 LIMIT 1");
  assert_eq!(route_select_sql("ktmb", true), "SELECT route_id, route_short_name, route_long_name, route_color FROM routes WHERE route_id = ?1 LIMIT 1");
  assert_eq!(route_select_sql("ktmb", false), "SELECT route_id, route_short_name, route_long_name, route_color FROM routes");
}
