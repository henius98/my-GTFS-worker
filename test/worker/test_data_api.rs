//! HTTP integration checks for a seeded Worker running at localhost:8787 (see `test/worker/worker.test.mjs` for deterministic local fixtures).

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

fn non_empty_rows<'a>(value: &'a Value, label: &str) -> CheckResult<&'a Vec<Value>> {
  let rows = value["data"].as_array().ok_or_else(|| format!("{label}: response has no data array"))?;
  if rows.is_empty() {
    return Err(format!("{label}: expected non-empty data rows from seeded Worker").into());
  }
  Ok(rows)
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
  let valid_table = "0_import_progress";
  let invalid_table = "this_table_does_not_exist";

  // 1. Test Valid Table
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?limit=2&offset=0"))?;
  // Check structure.
  let baseline = non_empty_rows(&data, "valid table")?;
  if data["limit"] != 2 || data["offset"] != 0 {
    return Err("valid table limit or offset mismatch".into());
  }
  let sample_status = baseline[0]["Status"].to_string().trim_matches('"').to_owned();
  let sample_file = baseline[0]["FileName"].as_str().ok_or("valid table row missing FileName")?;
  let contains_token = sample_file.split(|ch: char| !ch.is_ascii_alphanumeric()).find(|part| part.len() >= 2).ok_or("valid table FileName missing alphanumeric token")?;
  let icontains_token: String = contains_token.chars().map(|ch| if ch.is_ascii_lowercase() { ch.to_ascii_uppercase() } else { ch.to_ascii_lowercase() }).collect();
  println!("✅ Valid table test passed!");

  // 2. Test Invalid Table
  require_status(get(&client, &format!("/{provider}/data/{invalid_table}"))?, StatusCode::NOT_FOUND)?;
  println!("✅ Invalid table test passed!");

  // 3. Test Include Column Selection
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?include=CRC,Status&limit=2"))?;
  for row in non_empty_rows(&data, "include")? {
    if row.get("CRC").is_none() || row.get("Status").is_none() || row.get("FileName").is_some() {
      return Err("include selection mismatch".into());
    }
  }
  println!("✅ Include selection test passed!");

  // 4. Test Exclude Column Selection
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?exclude=FileName&limit=2"))?;
  for row in non_empty_rows(&data, "exclude")? {
    if row.get("CRC").is_none() || row.get("FileName").is_some() {
      return Err("exclude selection mismatch".into());
    }
  }
  println!("✅ Exclude selection test passed!");

  // 5. Test Exact Filtering
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?filter={sample_status}@Status"))?;
  for row in non_empty_rows(&data, "filter")? {
    if row["Status"].to_string().trim_matches('"') != sample_status {
      return Err("exact filtering mismatch".into());
    }
  }
  println!("✅ Exact filtering test passed!");

  // 6. Test icontains Filtering
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?icontains={icontains_token}@FileName"))?;
  let expected_lower = icontains_token.to_ascii_lowercase();
  for row in non_empty_rows(&data, "icontains")? {
    if !row["FileName"].as_str().unwrap_or_default().to_ascii_lowercase().contains(&expected_lower) {
      return Err("icontains filtering mismatch".into());
    }
  }
  println!("✅ icontains test passed!");

  // 7. Test Sorting
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?sort=-Status"))?;
  let rows = non_empty_rows(&data, "sort")?;
  if rows.windows(2).any(|pair| pair[0]["Status"].as_i64().unwrap_or(i64::MIN) < pair[1]["Status"].as_i64().unwrap_or(i64::MIN)) {
    return Err("sort order mismatch".into());
  }
  println!("✅ Sorting test passed!");

  // 8. Test Range Filtering
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?range=Status[0:1]"))?;
  for row in non_empty_rows(&data, "range")? {
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
  for sql in ["PRAGMA table_info(\"0_import_progress\")", "SELECT 1; SELECT 2"] {
    require_status(client.post(&sql_url).body(sql).send()?, StatusCode::BAD_REQUEST)?;
  }
  println!("✅ Invalid SQL tests passed!");

  // 11. Test SQL route requires POST
  require_status(client.get(&sql_url).send()?, StatusCode::METHOD_NOT_ALLOWED)?;
  println!("✅ SQL method test passed!");

  // 12. Test Data route requires GET
  require_status(client.post(format!("{BASE_URL}/{provider}/data/{valid_table}")).send()?, StatusCode::METHOD_NOT_ALLOWED)?;
  println!("✅ Data method test passed!");

  // 13. Test Data route rejects extraneous path segments
  require_status(get(&client, &format!("/{provider}/data/{valid_table}/extra"))?, StatusCode::NOT_FOUND)?;
  println!("✅ Data extra segment 404 test passed!");

  // 14. Test invalid filter format returns 400
  for bad_filter in ["bad", ""] {
    require_status(get(&client, &format!("/{provider}/data/{valid_table}?filter={bad_filter}"))?, StatusCode::BAD_REQUEST)?;
  }
  println!("✅ Invalid filter format tests passed!");

  // 15. Test invalid range format returns 400
  for bad_range in ["Status[0:1:2]", "Status[:]", "Status[1..5..10]"] {
    require_status(get(&client, &format!("/{provider}/data/{valid_table}?range={bad_range}"))?, StatusCode::BAD_REQUEST)?;
  }
  println!("✅ Invalid range format tests passed!");

  // 16. Test conflicting include and exclude return 400
  require_status(get(&client, &format!("/{provider}/data/{valid_table}?include=CRC&exclude=Status"))?, StatusCode::BAD_REQUEST)?;
  println!("✅ Conflicting include/exclude 400 test passed!");

  // 17. Test empty include and exclude return 400
  for bad_selection in ["include=", "include=   ", "exclude=", "exclude=   "] {
    require_status(get(&client, &format!("/{provider}/data/{valid_table}?{bad_selection}"))?, StatusCode::BAD_REQUEST)?;
  }
  println!("✅ Empty include/exclude 400 tests passed!");

  // 18. Test case-insensitive table and column selection
  let data = get_json(&client, &format!("/{provider}/data/0_IMPORT_PROGRESS?include=crc,status&limit=2"))?;
  for row in non_empty_rows(&data, "case-insensitive table and include")? {
    if row.get("CRC").is_none() || row.get("Status").is_none() || row.get("FileName").is_some() {
      return Err("case-insensitive include selection mismatch".into());
    }
  }

  // Test case-insensitive exclude
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?exclude=filename&limit=2"))?;
  for row in non_empty_rows(&data, "case-insensitive exclude")? {
    if row.get("CRC").is_none() || row.get("FileName").is_some() {
      return Err("case-insensitive exclude selection mismatch".into());
    }
  }

  // Test case-insensitive sort
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?sort=-status"))?;
  let rows = non_empty_rows(&data, "case-insensitive sort")?;
  if rows.windows(2).any(|pair| pair[0]["Status"].as_i64().unwrap_or(i64::MIN) < pair[1]["Status"].as_i64().unwrap_or(i64::MIN)) {
    return Err("case-insensitive sort order mismatch".into());
  }

  // Test contains (INSTR)
  let data = get_json(&client, &format!("/{provider}/data/{valid_table}?contains={contains_token}@FileName"))?;
  for row in non_empty_rows(&data, "contains")? {
    if !row["FileName"].as_str().unwrap_or_default().contains(contains_token) {
      return Err("contains filtering mismatch".into());
    }
  }

  // Test Cache-Control header
  let resp = get(&client, &format!("/{provider}/data/{valid_table}?limit=1"))?;
  if resp.headers().get("Cache-Control").and_then(|v| v.to_str().ok()) != Some("public, max-age=60") {
    return Err("missing or incorrect Cache-Control header on data endpoint".into());
  }

  // 19. Test malformed limit and offset return 400
  require_status(get(&client, &format!("/{provider}/data/{valid_table}?limit=abc"))?, StatusCode::BAD_REQUEST)?;
  require_status(get(&client, &format!("/{provider}/data/{valid_table}?offset=-1"))?, StatusCode::BAD_REQUEST)?;

  // 20. Test duplicate query parameters return 400
  require_status(get(&client, &format!("/{provider}/data/{valid_table}?include=CRC&include=Status"))?, StatusCode::BAD_REQUEST)?;
  require_status(get(&client, &format!("/{provider}/data/{valid_table}?exclude=FileName&exclude=Status"))?, StatusCode::BAD_REQUEST)?;

  println!("✅ Case-insensitive table and column selection tests passed!\nAll tests passed!");
  Ok(())
}
