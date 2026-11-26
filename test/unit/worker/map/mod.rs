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
fn parse_params_validates_route_id_and_date() -> std::result::Result<(), Box<dyn std::error::Error>> {
  let today = ServiceDate { yyyymmdd: 20260924, weekday: "thursday" };
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map")?, today, 20), Ok(MapParams { route_ids: Vec::new(), date: today }));
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map?route_id=KJL")?, today, 20), Ok(MapParams { route_ids: vec!["KJL".to_owned()], date: today }));
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map?route_id=%20KJL%20")?, today, 20), Ok(MapParams { route_ids: vec!["KJL".to_owned()], date: today }));
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map?route_id=%20PYL%20,KJL,PYL")?, today, 2), Ok(MapParams { route_ids: vec!["KJL".to_owned(), "PYL".to_owned()], date: today }));
  assert_eq!(
    parse_params(&Url::parse("https://example.com/ktmb/map?date=2026-09-21&route_id=KJL,AGL")?, today, 20),
    Ok(MapParams { route_ids: vec!["AGL".to_owned(), "KJL".to_owned()], date: ServiceDate { yyyymmdd: 20260921, weekday: "monday" } })
  );
  assert_eq!(
    parse_params(&Url::parse("https://example.com/ktmb/map?date=%2020260927%20")?, today, 20),
    Ok(MapParams { route_ids: Vec::new(), date: ServiceDate { yyyymmdd: 20260927, weekday: "sunday" } })
  );
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map?route_id=A,B,C")?, today, 2), Err((400, Cow::Owned("Too many route_id values; maximum is 2".to_owned()))));
  let long = "x".repeat(257);
  for query in
    ["?route_id=", "?route_id=%20%20", "?route_id=A,", "?route_id=,A", "?route_id=A,,B", "?route_id=A,%20,B", "?route_id=A&route_id=B", &format!("?route_id={long}"), &format!("?route_id=A,{long}")]
  {
    let url = Url::parse(&format!("https://example.com/ktmb/map{query}"))?;
    assert_eq!(parse_params(&url, today, 20), Err((400, Cow::Borrowed("Expected at most one route_id parameter with non-empty comma-separated route IDs"))), "{query}");
  }
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map?other=1")?, today, 20), Err((400, Cow::Borrowed("Unexpected query parameter; expected route_id and/or date"))));
  assert_eq!(parse_params(&Url::parse("https://example.com/ktmb/map?date=2026-09-24&date=2026-09-25")?, today, 20), Err((400, Cow::Borrowed("Expected at most one date"))));
  Ok(())
}

#[test]
fn route_cache_key_and_merge_route_features_order_deterministically() -> std::result::Result<(), Box<dyn std::error::Error>> {
  let url = Url::parse("https://example.com/KTMB/map?route_id=R2,R1&date=2026-09-24")?;
  let date = ServiceDate { yyyymmdd: 20260924, weekday: "thursday" };
  assert_eq!(route_cache_key(&url, "KTMB", Some("R1"), date), "https://example.com/ktmb/map?route_id=R1&date=20260924");
  assert_eq!(route_cache_key(&url, "KTMB", None, date), "https://example.com/ktmb/map?date=20260924");

  let mut by_route = BTreeMap::new();
  by_route.insert(
    "R2".to_owned(),
    vec![serde_json::json!({"type":"Feature","properties":{"kind":"route","route_id":"R2"}}), serde_json::json!({"type":"Feature","properties":{"kind":"stop","route_id":"R2","stop_id":"S2"}})],
  );
  by_route.insert(
    "R1".to_owned(),
    vec![serde_json::json!({"type":"Feature","properties":{"kind":"route","route_id":"R1"}}), serde_json::json!({"type":"Feature","properties":{"kind":"stop","route_id":"R1","stop_id":"S1"}})],
  );
  let merged = merge_route_features(by_route);
  let summary: Vec<(&str, &str)> = merged.iter().filter_map(|f| Some((f.get("properties")?.get("kind")?.as_str()?, f.get("properties")?.get("route_id")?.as_str()?))).collect();
  assert_eq!(summary, vec![("route", "R1"), ("route", "R2"), ("stop", "R1"), ("stop", "R2")]);
  Ok(())
}

#[test]
fn parse_service_date_calculates_weekday_and_rejects_invalid_dates() {
  for (input, yyyymmdd, weekday) in [
    ("2026-09-21", 20260921, "monday"),
    ("20260922", 20260922, "tuesday"),
    ("2026-09-23", 20260923, "wednesday"),
    ("20260924", 20260924, "thursday"),
    ("2026-09-25", 20260925, "friday"),
    ("20260926", 20260926, "saturday"),
    ("2026-09-27", 20260927, "sunday"),
    ("2024-02-29", 20240229, "thursday"),
  ] {
    assert_eq!(parse_service_date(input), Ok(ServiceDate { yyyymmdd, weekday }), "{input}");
  }
  for invalid in ["", " ", "monday", "2026-9-24", "2026-02-29", "2026-13-01", "0999-01-01", "2026/09/24", "202609241", "2026-09-24T00:00:00Z"] {
    assert_eq!(parse_service_date(invalid), Err((400, "Invalid date; expected YYYY-MM-DD or YYYYMMDD")), "{invalid}");
  }
}

#[test]
fn active_services_cte_renders_calendar_and_exception_variants() {
  let d = ServiceDate { yyyymmdd: 20260921, weekday: "monday" };

  let cal_only = active_services_cte(d, false);
  assert!(cal_only.contains("FROM calendar") && cal_only.contains("monday = 1") && cal_only.contains("20260921"));
  assert!(!cal_only.contains("calendar_dates"));

  let with_exc = active_services_cte(d, true);
  assert!(with_exc.contains("FROM calendar") && with_exc.contains("UNION") && with_exc.contains("exception_type = 2") && with_exc.contains("exception_type = 1"));
}

#[test]
fn collect_paths_groups_direct_and_shared_shapes() -> std::result::Result<(), Box<dyn std::error::Error>> {
  let row_without_route: ShapePoint = serde_json::from_str(r#"{"shape_id":"SH1","lat":3.1,"lng":101.6}"#)?;
  assert!(row_without_route.route_id.is_none());
  assert!(row_without_route.trip_ids_json.is_none());
  let mut shape_routes = BTreeMap::new();
  shape_routes.insert("SH1".to_owned(), vec!["R1".to_owned(), "R2".to_owned()]);
  let paths = collect_paths_by_shape(vec![row_without_route], &shape_routes);
  assert_eq!(paths.get(&("R1".to_owned(), "SH1".to_owned())), Some(&ShapePath { points: vec![[101.6, 3.1]], trip_ids: None }));
  assert_eq!(paths.get(&("R2".to_owned(), "SH1".to_owned())), Some(&ShapePath { points: vec![[101.6, 3.1]], trip_ids: None }));

  // First point has invalid coordinates but carries trip_ids_json; subsequent valid points must still keep sorted trip_ids.
  let rows_with_trips: Vec<ShapePoint> = serde_json::from_str(
    r#"[
      {"route_id":"R2","shape_id":"SH2","trip_ids_json":"[\"T2\",\"T1\"]","lat":99.0,"lng":101.7},
      {"route_id":"R2","shape_id":"SH2","trip_ids_json":null,"lat":3.2,"lng":101.7},
      {"route_id":"R2","shape_id":"SH2","trip_ids_json":null,"lat":3.3,"lng":101.8},
      {"route_id":"R3","shape_id":"SH_BAD","trip_ids_json":"[\"T3\"]","lat":null,"lng":101.7}
    ]"#,
  )?;
  let paths = collect_paths(rows_with_trips)?;
  assert_eq!(paths.get(&("R2".to_owned(), "SH2".to_owned())), Some(&ShapePath { points: vec![[101.7, 3.2], [101.8, 3.3]], trip_ids: Some(vec!["T1".to_owned(), "T2".to_owned()]) }));
  assert!(!paths.contains_key(&("R3".to_owned(), "SH_BAD".to_owned())));

  let routes = HashMap::from([("R2".to_owned(), MapRoute { route_id: "R2".to_owned(), route_short_name: Some("R2".to_owned()), route_long_name: None, route_color: None })]);
  let unfiltered_json = serde_json::to_value(build_features(&routes, paths, BTreeMap::new(), Vec::new()))?;
  assert_eq!(unfiltered_json[0]["properties"]["trip_ids"], serde_json::json!(["T1", "T2"]));

  let filtered_paths = BTreeMap::from([(("R2".to_owned(), "SH2".to_owned()), ShapePath { points: vec![[101.7, 3.2], [101.8, 3.3]], trip_ids: None })]);
  let filtered_json = serde_json::to_value(build_features(&routes, filtered_paths, BTreeMap::new(), Vec::new()))?;
  assert!(filtered_json[0]["properties"].get("trip_ids").is_none());

  let stop: StopPoint = serde_json::from_str(r#"{"route_id":"R1","stop_id":"S1","stop_name":"A","lat":3.1,"lng":101.6,"trip_ids_json":"[\"T1\",\"T2\"]","arrival_times":"08:00:00, 08:10:00"}"#)?;
  assert_eq!(stop.route_id, "R1");
  let stop_features = serde_json::to_value(build_features(&routes, BTreeMap::new(), BTreeMap::new(), vec![stop]))?;
  assert_eq!(stop_features[0]["properties"]["trip_ids"], serde_json::json!(["T1", "T2"]));
  assert_eq!(stop_features[0]["properties"]["arrival_times"], "08:00:00, 08:10:00");
  assert!(stop_features[0]["properties"].get("departure_times").is_none());
  let filtered_stop: StopPoint = serde_json::from_str(r#"{"route_id":"R1","stop_id":"S1","stop_name":"A","lat":3.1,"lng":101.6,"arrival_times":null}"#)?;
  let filtered_stop_features = serde_json::to_value(build_features(&routes, BTreeMap::new(), BTreeMap::new(), vec![filtered_stop]))?;
  assert!(filtered_stop_features[0]["properties"].get("trip_ids").is_none());
  assert!(filtered_stop_features[0]["properties"].get("departure_times").is_none());
  let seg: StopSegment = serde_json::from_str(r#"{"route_id":"R1","lat":3.1,"lng":101.6,"to_lat":3.2,"to_lng":101.7}"#)?;
  assert_eq!(seg.route_id, "R1");
  Ok(())
}

#[test]
fn collect_segments_and_stops_groups_multilinestring_and_splits_stops() -> std::result::Result<(), Box<dyn std::error::Error>> {
  let rows: Vec<SegmentOrStop> = serde_json::from_str(
    r#"[
      {"row_kind":"segment","route_id":"R1","lat":3.1,"lng":101.6,"to_lat":3.2,"to_lng":101.7,"stop_id":null,"stop_name":null,"arrival_times":null},
      {"row_kind":"segment","route_id":"R1","lat":3.2,"lng":101.7,"to_lat":3.3,"to_lng":101.8,"stop_id":null,"stop_name":null,"arrival_times":null},
      {"row_kind":"segment","route_id":"R1","lat":99.0,"lng":101.7,"to_lat":3.3,"to_lng":101.8,"stop_id":null,"stop_name":null,"arrival_times":null},
      {"row_kind":"segment","route_id":"R2","lat":4.0,"lng":102.0,"to_lat":4.1,"to_lng":102.1,"stop_id":null,"stop_name":null,"arrival_times":null},
      {"row_kind":"stop","route_id":"R1","lat":3.1,"lng":101.6,"to_lat":null,"to_lng":null,"stop_id":"S1","stop_name":"First","arrival_times":"08:00:00"}
    ]"#,
  )?;
  let (segments, stops) = collect_segments_and_stops(rows);
  assert_eq!(segments.get("R1"), Some(&vec![[[101.6, 3.1], [101.7, 3.2]], [[101.7, 3.2], [101.8, 3.3]]]));
  assert_eq!(segments.get("R2"), Some(&vec![[[102.0, 4.0], [102.1, 4.1]]]));
  assert_eq!(stops.len(), 1);
  assert_eq!(stops[0].route_id, "R1");
  assert_eq!(stops[0].stop_id, "S1");
  assert_eq!(stops[0].arrival_times.as_deref(), Some("08:00:00"));

  let feature = Feature {
    kind: "Feature",
    geometry: Geometry::MultiLineString(segments.get("R1").cloned().unwrap_or_default()),
    properties: FeatureProperties::SegmentRoute { route_id: "R1".to_owned(), geometry_source: "stop_sequence", route_short_name: Some("101"), route_long_name: None, route_color: Some("112233") },
  };
  let json = serde_json::to_value(&feature)?;
  assert_eq!(json["geometry"]["type"], "MultiLineString");
  assert_eq!(json["geometry"]["coordinates"], serde_json::json!([[[101.6, 3.1], [101.7, 3.2]], [[101.7, 3.2], [101.8, 3.3]]]));
  assert_eq!(json["properties"]["kind"], "route");
  assert_eq!(json["properties"]["route_id"], "R1");
  assert_eq!(json["properties"]["geometry_source"], "stop_sequence");
  assert!(json["properties"].get("from_stop_id").is_none());
  assert!(json["properties"].get("to_stop_id").is_none());
  Ok(())
}
