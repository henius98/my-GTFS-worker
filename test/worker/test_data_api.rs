//! HTTP integration checks for a Worker running at localhost:8787.

use gtfs_checks::CheckResult;
use reqwest::{
  StatusCode,
  blocking::{Client, Response},
};
use serde_json::{Value, json};
use std::{thread, time::Duration};

const BASE_URL: &str = "http://127.0.0.1:8787";

fn get(client: &Client, path: &str) -> CheckResult<Response> {
  Ok(client.get(format!("{BASE_URL}{path}")).send()?)
}

fn get_json(client: &Client, path: &str) -> CheckResult<Value> {
  Ok(get(client, path)?.error_for_status()?.json()?)
}

fn require_status(response: Response, expected: StatusCode) -> CheckResult {
  if response.status() != expected {
    return Err(format!("expected {expected}, got {} for {}", response.status(), response.url()).into());
  }
  Ok(())
}

fn data_rows<'a>(value: &'a Value, label: &str) -> CheckResult<&'a Vec<Value>> {
  value["data"].as_array().ok_or_else(|| format!("{label}: response has no data array").into())
}

fn main() -> CheckResult {
  let client = Client::builder().timeout(Duration::from_secs(5)).build()?;
  println!("Waiting for wrangler dev server to start...");
  let mut ready = false;
  for _ in 0..30 {
    // We hit a known path just to test server is up.
    if get(&client, "/").is_ok() {
      ready = true;
      break;
    }
    thread::sleep(Duration::from_secs(1));
  }
  if !ready {
    return Err("server failed to start".into());
  }
  println!("Server is up!\n--- Running Integration Tests ---");
  let provider = "ktmb";
  let valid_table = "import_progress";
  let invalid_table = "this_table_does_not_exist";

  // 1. Test Valid Table
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?limit=2&offset=0"))?;
  // Check structure.
  data_rows(&data, "valid table")?;
  if data["limit"] != 2 || data["offset"] != 0 {
    return Err("valid table limit or offset mismatch".into());
  }
  println!("✅ Valid table test passed!");

  // 2. Test Invalid Table
  require_status(get(&client, &format!("/{provider}/data/{invalid_table}"))?, StatusCode::NOT_FOUND)?;
  println!("✅ Invalid table test passed!");

  // 3. Test Include Column Selection
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?include=Provider,Status&limit=2"))?;
  for row in data_rows(&data, "include")? {
    if row.get("Provider").is_none() || row.get("Status").is_none() || row.get("FileName").is_some() {
      return Err("include selection mismatch".into());
    }
  }
  println!("✅ Include selection test passed!");

  // 4. Test Exclude Column Selection
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?exclude=FileName&limit=2"))?;
  for row in data_rows(&data, "exclude")? {
    if row.get("Provider").is_none() || row.get("FileName").is_some() {
      return Err("exclude selection mismatch".into());
    }
  }
  println!("✅ Exclude selection test passed!");

  // 5. Test Exact Filtering
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?filter=1@Status"))?;
  for row in data_rows(&data, "filter")? {
    if row["Status"].to_string().trim_matches('"') != "1" {
      return Err("exact filtering mismatch".into());
    }
  }
  println!("✅ Exact filtering test passed!");

  // 6. Test icontains Filtering
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?icontains=theR.zi@FileName"))?;
  for row in data_rows(&data, "icontains")? {
    if !row["FileName"].as_str().unwrap_or_default().to_lowercase().contains("ther.zi") {
      return Err("icontains filtering mismatch".into());
    }
  }
  println!("✅ icontains test passed!");

  // 7. Test Sorting
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?sort=-Status"))?;
  let rows = data_rows(&data, "sort")?;
  if rows.windows(2).any(|pair| pair[0]["Status"].as_i64().unwrap_or(i64::MIN) < pair[1]["Status"].as_i64().unwrap_or(i64::MIN)) {
    return Err("sort order mismatch".into());
  }
  println!("✅ Sorting test passed!");

  // 8. Test Range Filtering
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?range=Status[0:1]"))?;
  for row in data_rows(&data, "range")? {
    if !matches!(row["Status"].as_i64(), Some(0 | 1)) {
      return Err("range filtering mismatch".into());
    }
  }
  println!("✅ Range test passed!");

  let sql_url = format!("{BASE_URL}/{provider}/sql");
  // 9. Test raw SQL selection
  let selected: Value = client.post(&sql_url).header("Content-Type", "text/plain").body("WITH row AS (SELECT 1 AS value) SELECT value FROM row").send()?.error_for_status()?.json()?;
  if selected["data"] != json!([{"value":1}]) {
    return Err("raw SQL selection mismatch".into());
  }
  println!("✅ Raw SQL selection test passed!");

  // 10. Test SQL route rejects writes and multiple statements
  for sql in ["PRAGMA table_info(import_progress)", "SELECT 1; SELECT 2"] {
    require_status(client.post(&sql_url).body(sql).send()?, StatusCode::BAD_REQUEST)?;
  }
  println!("✅ Invalid SQL tests passed!");

  // 11. Test SQL route requires POST
  require_status(client.get(&sql_url).send()?, StatusCode::METHOD_NOT_ALLOWED)?;
  println!("✅ SQL method test passed!\nAll tests passed!");
  Ok(())
}
