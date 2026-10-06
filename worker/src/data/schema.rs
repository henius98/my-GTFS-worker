use std::{cell::RefCell, collections::HashMap, rc::Rc};

use serde::{Deserialize, Serialize};
use worker::{Cache, D1Database, Error, Response, Result};

const SCHEMA_CACHE_VERSION: &str = env!("SCHEMA_CACHE_VERSION");
const SCHEMA_CACHE_TTL: u32 = 86_400;

#[derive(Deserialize, Serialize)]
pub(super) struct TableSchema {
  pub columns: Vec<String>,
}

impl TableSchema {
  fn valid(&self) -> bool {
    !self.columns.is_empty() && self.columns.iter().all(|column| valid_identifier(column))
  }
}

fn valid_identifier(name: &str) -> bool {
  let mut bytes = name.bytes();
  matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_')) && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn cache_key(version: &str, database: &str, table: &str) -> String {
  format!("https://schema-cache.internal/{version}/{database}/{table}")
}

#[derive(Clone, Default)]
struct SchemaRegistry(Rc<RefCell<SchemaMap>>);

type SchemaMap = HashMap<String, HashMap<String, HashMap<String, Rc<TableSchema>>>>;

impl SchemaRegistry {
  fn get(&self, version: &str, database: &str, table: &str) -> Option<Rc<TableSchema>> {
    self.0.borrow().get(version)?.get(database)?.get(table).cloned()
  }

  fn insert(&self, version: &str, database: &str, table: &str, schema: Rc<TableSchema>) {
    self.0.borrow_mut().entry(version.to_owned()).or_default().entry(database.to_owned()).or_default().insert(table.to_owned(), schema);
  }
}

thread_local! {
  static REGISTRY: SchemaRegistry = SchemaRegistry::default();
}

trait SchemaBackend {
  async fn read_cache(&self, key: &str) -> Result<Option<String>>;
  async fn write_cache(&self, key: &str, body: String) -> Result<()>;
  async fn read_d1(&self, table: &str) -> Result<Option<TableSchema>>;
}

struct WorkerBackend<'a>(&'a D1Database);

impl SchemaBackend for WorkerBackend<'_> {
  async fn read_cache(&self, key: &str) -> Result<Option<String>> {
    match Cache::default().get(key, false).await? {
      Some(mut response) if response.status_code() == 200 => Ok(Some(response.text().await?)),
      Some(_) => Ok(None),
      None => Ok(None),
    }
  }

  async fn write_cache(&self, key: &str, body: String) -> Result<()> {
    let mut response = Response::ok(body)?;
    response.headers_mut().set("Cache-Control", &format!("public, max-age={SCHEMA_CACHE_TTL}"))?;
    Cache::default().put(key, response).await
  }

  async fn read_d1(&self, table: &str) -> Result<Option<TableSchema>> {
    let exists = self.0.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name=?1").bind(&[table.into()])?.first::<String>(Some("name")).await?;
    if exists.is_none() {
      return Ok(None);
    }
    let result = self.0.prepare("SELECT name FROM pragma_table_info(?1)").bind(&[table.into()])?.all().await?;
    #[derive(Deserialize)]
    struct ColumnName {
      name: String,
    }
    let columns = result.results::<ColumnName>()?.into_iter().map(|column| column.name).collect();
    Ok(Some(TableSchema { columns }))
  }
}

async fn resolve<B: SchemaBackend>(registry: &SchemaRegistry, backend: &B, version: &str, database: &str, table: &str) -> Result<Option<Rc<TableSchema>>> {
  if !valid_identifier(table) {
    return Ok(None);
  }
  if let Some(schema) = registry.get(version, database, table) {
    return Ok(Some(schema));
  }
  let key = cache_key(version, database, table);

  if let Ok(Some(body)) = backend.read_cache(&key).await
    && let Ok(schema) = serde_json::from_str::<TableSchema>(&body)
    && schema.valid()
  {
    let schema = Rc::new(schema);
    registry.insert(version, database, table, schema.clone());
    return Ok(Some(schema));
  }

  let Some(schema) = backend.read_d1(table).await? else {
    return Ok(None);
  };
  if !schema.valid() {
    return Err(Error::RustError(format!("Invalid schema for table '{table}'")));
  }
  if let Ok(body) = serde_json::to_string(&schema) {
    let _ = backend.write_cache(&key, body).await;
  }
  let schema = Rc::new(schema);
  registry.insert(version, database, table, schema.clone());
  Ok(Some(schema))
}

pub(super) async fn load(db: &D1Database, database: &str, table: &str) -> Result<Option<Rc<TableSchema>>> {
  let registry = REGISTRY.with(Clone::clone);
  resolve(&registry, &WorkerBackend(db), SCHEMA_CACHE_VERSION, database, table).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::Cell;

  struct Backend {
    cached: RefCell<Option<Result<Option<String>>>>,
    writes_fail: bool,
    cache_reads: Cell<usize>,
    cache_writes: Cell<usize>,
    d1_reads: Cell<usize>,
  }

  impl Backend {
    fn new(cached: Result<Option<String>>, writes_fail: bool) -> Self {
      Self { cached: RefCell::new(Some(cached)), writes_fail, cache_reads: Cell::new(0), cache_writes: Cell::new(0), d1_reads: Cell::new(0) }
    }
  }

  impl SchemaBackend for Backend {
    async fn read_cache(&self, _: &str) -> Result<Option<String>> {
      self.cache_reads.set(self.cache_reads.get() + 1);
      self.cached.borrow_mut().take().unwrap_or(Ok(None))
    }

    async fn write_cache(&self, _: &str, _: String) -> Result<()> {
      self.cache_writes.set(self.cache_writes.get() + 1);
      if self.writes_fail { Err(Error::RustError("cache write failed".into())) } else { Ok(()) }
    }

    async fn read_d1(&self, _: &str) -> Result<Option<TableSchema>> {
      self.d1_reads.set(self.d1_reads.get() + 1);
      Ok(Some(TableSchema { columns: vec!["stop_id".into()] }))
    }
  }

  #[tokio::test]
  async fn l1_hit_skips_cache_and_d1() -> Result<()> {
    let registry = SchemaRegistry::default();
    let backend = Backend::new(Ok(None), false);
    let first = resolve(&registry, &backend, "7da91b", "DB_KTMB", "stops").await?.ok_or_else(|| Error::RustError("missing schema".into()))?;
    let second = resolve(&registry, &backend, "7da91b", "DB_KTMB", "stops").await?.ok_or_else(|| Error::RustError("missing schema".into()))?;
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!((backend.cache_reads.get(), backend.d1_reads.get()), (1, 1));
    Ok(())
  }

  #[tokio::test]
  async fn l2_hit_populates_l1() -> Result<()> {
    let registry = SchemaRegistry::default();
    let backend = Backend::new(Ok(Some(r#"{"columns":["stop_id"]}"#.into())), false);
    assert!(resolve(&registry, &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert!(resolve(&registry, &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert_eq!((backend.cache_reads.get(), backend.d1_reads.get(), backend.cache_writes.get()), (1, 0, 0));
    Ok(())
  }

  #[tokio::test]
  async fn l2_miss_loads_d1_and_writes_cache() -> Result<()> {
    let backend = Backend::new(Ok(None), false);
    assert!(resolve(&SchemaRegistry::default(), &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert_eq!((backend.d1_reads.get(), backend.cache_writes.get()), (1, 1));
    Ok(())
  }

  #[tokio::test]
  async fn invalid_l2_data_loads_d1() -> Result<()> {
    let backend = Backend::new(Ok(Some(r#"{"columns":[]}"#.into())), false);
    assert!(resolve(&SchemaRegistry::default(), &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert_eq!(backend.d1_reads.get(), 1);

    let backend = Backend::new(Ok(Some("{broken json".into())), false);
    assert!(resolve(&SchemaRegistry::default(), &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert_eq!(backend.d1_reads.get(), 1);
    Ok(())
  }

  #[test]
  fn version_and_database_change_cache_key() {
    let original = cache_key("7da91b", "DB_KTMB", "stops");
    assert_eq!(original, "https://schema-cache.internal/7da91b/DB_KTMB/stops");
    assert_ne!(original, cache_key("a84c12", "DB_KTMB", "stops"));
    assert_ne!(original, cache_key("7da91b", "DB_OTHER", "stops"));
  }

  #[tokio::test]
  async fn cache_read_failure_loads_d1() -> Result<()> {
    let backend = Backend::new(Err(Error::RustError("cache read failed".into())), false);
    assert!(resolve(&SchemaRegistry::default(), &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert_eq!(backend.d1_reads.get(), 1);
    Ok(())
  }

  #[tokio::test]
  async fn cache_write_failure_still_succeeds() -> Result<()> {
    let backend = Backend::new(Ok(None), true);
    assert!(resolve(&SchemaRegistry::default(), &backend, "7da91b", "DB_KTMB", "stops").await?.is_some());
    assert_eq!((backend.d1_reads.get(), backend.cache_writes.get()), (1, 1));
    Ok(())
  }

  #[tokio::test]
  async fn invalid_table_skips_all_backends() -> Result<()> {
    let backend = Backend::new(Ok(None), false);
    assert!(resolve(&SchemaRegistry::default(), &backend, "7da91b", "DB_KTMB", "stops;DROP").await?.is_none());
    assert_eq!((backend.cache_reads.get(), backend.d1_reads.get()), (0, 0));
    Ok(())
  }
}
