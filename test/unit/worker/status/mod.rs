use super::*;

#[test]
fn canonical_cache_key_strips_query_and_lowercases_provider() -> std::result::Result<(), Box<dyn std::error::Error>> {
  let first = canonical_cache_key(&Url::parse("https://example.com/KTMB/status/?limit=1")?, "KTMB");
  let second = canonical_cache_key(&Url::parse("https://example.com/ktmb/status")?, "ktmb");
  assert_eq!(first, "https://example.com/ktmb/status");
  assert_eq!(first, second);
  Ok(())
}

#[test]
fn import_progress_round_trips_pascal_case_and_crc() -> std::result::Result<(), serde_json::Error> {
  let row = serde_json::json!({
    "FileName": "stops.txt",
    "CRC": "a1b2c3d4",
    "LastProcessedLine": 42,
    "LastProcessedByte": 1024,
    "Status": 0,
    "UpdatedAt": "2026-09-24 00:00:00"
  });
  let progress: ImportProgress = serde_json::from_value(row.clone())?;
  assert_eq!(serde_json::to_value(progress)?, row);
  Ok(())
}
