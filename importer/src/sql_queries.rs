pub fn provider_maintenance_queries(provider_name: &str) -> Vec<&'static str> {
  match provider_name {
    "rapid-bus-penang" => vec![
      "DELETE FROM stop_times WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM trips WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM trips WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
    ],
    "rapid-bus-mrtfeeder" => vec![
      "DELETE FROM stop_times WHERE trip_id IN (SELECT trip_id FROM trips WHERE service_id IN (SELECT service_id FROM (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id UNION ALL SELECT service_id, MAX(date) AS max_end FROM calendar_dates WHERE exception_type = 1 GROUP BY service_id) GROUP BY service_id HAVING MAX(max_end) < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)))",
      "DELETE FROM trips WHERE service_id IN (SELECT service_id FROM (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id UNION ALL SELECT service_id, MAX(date) AS max_end FROM calendar_dates WHERE exception_type = 1 GROUP BY service_id) GROUP BY service_id HAVING MAX(max_end) < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM trips WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
      "DELETE FROM calendar_dates WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
    ],
    "ktmb" => vec![
      "INSERT OR IGNORE INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES ('37200', 'LARKIN / TAMPOI HALTE', 1.5050, 103.7400)",
      "DELETE FROM stop_times WHERE stop_sequence < 100 AND trip_id IN (SELECT trip_id FROM stop_times WHERE stop_sequence >= 100)",
      "DELETE FROM trips WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM stop_times WHERE trip_id IN (SELECT trip_id FROM trips WHERE service_id IN (SELECT service_id FROM (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id UNION ALL SELECT service_id, MAX(date) AS max_end FROM calendar_dates WHERE exception_type = 1 GROUP BY service_id) GROUP BY service_id HAVING MAX(max_end) < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER)))",
      "DELETE FROM trips WHERE service_id IN (SELECT service_id FROM (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id UNION ALL SELECT service_id, MAX(date) AS max_end FROM calendar_dates WHERE exception_type = 1 GROUP BY service_id) GROUP BY service_id HAVING MAX(max_end) < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
      "DELETE FROM calendar_dates WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
    ],
    "mybas-johor" => vec![
      "DELETE FROM fare_leg_rules WHERE fare_product_id NOT IN (SELECT fare_product_id FROM fare_products)",
      "DELETE FROM trips WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM stop_times WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM trips WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
    ],
    "rapid-bus-kl" => vec![
      "DELETE FROM frequencies WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM trips WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM stop_times WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM trips WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM frequencies WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM trips)",
      "DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
    ],
    "rapid-rail-kl" => vec![
      "DELETE FROM frequencies WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM trips WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM stop_times)",
      "DELETE FROM stop_times WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM trips WHERE trip_id IN (SELECT t.trip_id FROM trips t JOIN (SELECT service_id, MAX(end_date) AS max_end FROM calendar GROUP BY service_id) c ON t.service_id = c.service_id WHERE c.max_end < CAST(strftime('%Y%m%d', 'now', '+8 hours') AS INTEGER))",
      "DELETE FROM frequencies WHERE trip_id NOT IN (SELECT DISTINCT trip_id FROM trips)",
      "DELETE FROM calendar WHERE service_id NOT IN (SELECT DISTINCT service_id FROM trips)",
    ],
    _ => Vec::new(),
  }
}

pub fn quote_identifier(identifier: &str) -> String {
  format!("\"{}\"", identifier.replace('"', "\"\""))
}
