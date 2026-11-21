use super::*;
use crate::config::load_config;
use crate::d1::{D1DatabaseInfo, D1Error, D1Meta, D1Result, tests::InMemoryTransport};
use std::sync::atomic::{AtomicU64, Ordering};

static ROTATION_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFile(PathBuf);
impl Drop for TempFile {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

fn test_provider(name: &str, database_id: &str) -> ProviderConfig {
  ProviderConfig { name: name.to_owned(), is_active: true, static_url: String::new(), static_provider: String::new(), database_id: database_id.to_owned() }
}

fn ok_rows_result(results: Vec<serde_json::Value>) -> D1Result {
  D1Result { meta: Some(D1Meta { rows_read: u64::try_from(results.len()).unwrap_or(u64::MAX), rows_written: Some(0) }), results, success: Some(true) }
}

#[tokio::test]
async fn rotate_if_needed_enforces_guards_and_cleans_up_failed_setup() -> Result<(), Box<dyn std::error::Error>> {
  let transport = InMemoryTransport::new();
  let client = D1Client::with_in_memory(transport.clone(), 100, 100);
  let rotator = DatabaseRotator::new(client.clone(), "unused.toml", 100, 10);

  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 50 }, 1);
  let mut provider = test_provider("ktmb", "db-ktmb");
  rotator.rotate_if_needed(&mut provider).await?;
  assert_eq!(provider.database_id, "db-ktmb");
  assert!(transport.deleted_databases().is_empty());

  let mut budget_client = client.clone();
  transport.push_query_response(vec![ok_rows_result(vec![serde_json::json!({ "Granted": 5_000 })])]);
  assert!(budget_client.acquire_daily_budget("db-ktmb", 5_000, 100_000).await?);
  let budget_rotator = DatabaseRotator::new(budget_client, "unused.toml", 100, 10);
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 200 }, 1);
  let Err(err) = budget_rotator.rotate_if_needed(&mut provider).await else {
    return Err("budget database must refuse automatic rotation".into());
  };
  assert!(err.to_string().contains("daily-budget"));

  transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 200 }, 10);
  let Err(err) = rotator.rotate_if_needed(&mut provider).await else {
    return Err("max database limit must block rotation".into());
  };
  assert!(err.to_string().contains("Archive and delete"));

  // Unknown provider fails before creating a D1 database.
  transport.set_database_info(D1DatabaseInfo { name: "gtfs-unknown".to_owned(), file_size: 200 }, 1);
  let mut unknown = test_provider("unknown-provider", "db-rot");
  assert!(rotator.rotate_if_needed(&mut unknown).await.is_err());
  assert_eq!(unknown.database_id, "db-rot");
  assert!(transport.deleted_databases().is_empty());

  // Schema setup failure deletes the newly created orphan database.
  let fail_transport = InMemoryTransport::new();
  fail_transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb".to_owned(), file_size: 200 }, 1);
  fail_transport.push_outcome(Err(D1Error::ApiError("injected schema setup failure".to_owned()).into()));
  let fail_client = D1Client::with_in_memory(fail_transport.clone(), 100, 1_000);
  let fail_rotator = DatabaseRotator::new(fail_client, "unused.toml", 100, 10);
  let mut failing_provider = test_provider("ktmb", "db-rot");
  assert!(fail_rotator.rotate_if_needed(&mut failing_provider).await.is_err());
  assert_eq!(failing_provider.database_id, "db-rot");
  assert_eq!(fail_transport.deleted_databases(), vec!["in-memory-uuid-1"]);

  let happy_transport = InMemoryTransport::new();
  let happy_client = D1Client::with_in_memory(happy_transport.clone(), 100, 1_000);
  happy_transport.set_database_info(D1DatabaseInfo { name: "gtfs-ktmb-old".to_owned(), file_size: 200 }, 1);
  let seq = ROTATION_TEST_SEQ.fetch_add(1, Ordering::Relaxed);
  let path = std::env::temp_dir().join(format!("my-gtfs-worker-rotate-providers-test-{}-{seq}.toml", std::process::id()));
  let _cleanup = TempFile(path.clone());
  std::fs::write(
    &path,
    r#"budget_provider = "ktmb"

[[providers]]
name = "ktmb"
is_active = true
static_url = "https://example.invalid/"
static_provider = "ktmb"
database_id = "db-old"
"#,
  )?;
  let happy_rotator = DatabaseRotator::new(happy_client, &path, 100, 10);
  let mut happy_provider = test_provider("ktmb", "db-old");
  happy_rotator.rotate_if_needed(&mut happy_provider).await?;
  assert_eq!(happy_provider.database_id, "in-memory-uuid-1");
  assert!(happy_transport.deleted_databases().is_empty());

  // Verify rotation executed only pre-split, comment-free base schema statements.
  let recorded = happy_transport.recorded_queries();
  let expected = SchemaCatalog::base_schema_statements("ktmb").ok_or("ktmb base schema missing")?;
  assert_eq!(recorded.len(), expected.len());
  assert!(recorded.iter().all(|query| !query.sql.starts_with("--") && !query.sql.starts_with("empty route_id")));

  let updated = std::fs::read_to_string(&path)?;
  assert!(updated.parse::<toml_edit::DocumentMut>().is_ok());
  assert!(updated.contains("database_id = \"in-memory-uuid-1\""));
  assert!(updated.contains("[[retired_databases]]"));
  assert!(updated.contains("provider = \"ktmb\""));
  assert!(updated.contains("name = \"gtfs-ktmb-old\""));
  assert!(updated.contains("database_id = \"db-old\""));
  let reloaded = load_config(&path)?;
  assert_eq!(reloaded.budget_database()?.database_id, "in-memory-uuid-1");
  Ok(())
}
