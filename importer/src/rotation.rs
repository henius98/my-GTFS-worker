use crate::config::{ConfigError, ProviderConfig};
use crate::d1::D1Client;
use crate::error::ProcessorError;
use crate::schema::SchemaCatalog;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct DatabaseRotationRecord {
  provider_name: String,
  retired_database_id: String,
  retired_database_name: String,
  new_database_id: String,
  retired_at: String,
}

#[derive(Clone)]
pub(crate) struct DatabaseRotator {
  d1_client: D1Client,
  providers_path: PathBuf,
  database_rotation_lock: Arc<tokio::sync::Mutex<()>>,
  threshold_bytes: u64,
  max_databases: usize,
}

impl DatabaseRotator {
  pub(crate) fn new(d1_client: D1Client, providers_path: impl Into<PathBuf>, threshold_bytes: u64, max_databases: usize) -> Self {
    Self { d1_client, providers_path: providers_path.into(), database_rotation_lock: Arc::new(tokio::sync::Mutex::new(())), threshold_bytes, max_databases }
  }

  pub(crate) async fn rotate_if_needed(&self, provider: &mut ProviderConfig) -> Result<(), ProcessorError> {
    let database = self.d1_client.get_database_info(&provider.database_id).await?;
    if database.file_size < self.threshold_bytes {
      return Ok(());
    }
    if self.d1_client.is_budget_database(&provider.database_id) {
      return Err(ProcessorError::Rotation("The daily-budget database cannot rotate automatically; preserve its ledger when moving it".to_owned()));
    }
    let Some(base_statements) = SchemaCatalog::base_schema_statements(&provider.name) else {
      return Err(ProcessorError::Rotation(format!("No schema SQL found for provider {}", provider.name)));
    };

    let _rotation_guard = self.database_rotation_lock.lock().await;
    let database_count = self.d1_client.get_database_count().await?;
    let database_limit = u64::try_from(self.max_databases).unwrap_or(u64::MAX);
    if database_count >= database_limit {
      return Err(ProcessorError::Rotation(format!(
        "Database rotation requires a new D1 database, but the account already has {database_count}/{}. Archive and delete an obsolete database before retrying.",
        self.max_databases
      )));
    }

    println!("[{}] Database size {} exceeds threshold {}. Rotating...", provider.name, database.file_size, self.threshold_bytes);
    let now = chrono::Utc::now();
    let new_db_name = format!("gtfs-{}-db-{}", provider.name, now.format("%Y%m%d%H%M%S"));
    let new_uuid = self.d1_client.create_database(&new_db_name).await?;
    println!("[{}] Created new database {} with UUID {}", provider.name, new_db_name, new_uuid);

    let setup = async {
      println!("[{}] Applying base schema to new database...", provider.name);
      self.d1_client.execute_schema(&new_uuid, base_statements).await?;

      let path = self.providers_path.clone();
      let record = DatabaseRotationRecord {
        provider_name: provider.name.clone(),
        retired_database_id: provider.database_id.clone(),
        retired_database_name: database.name,
        new_database_id: new_uuid.clone(),
        retired_at: now.to_rfc3339(),
      };
      tokio::task::spawn_blocking(move || persist_database_rotation(&path, record)).await??;
      Ok(())
    }
    .await;

    if let Err(error) = setup {
      println!("[{}] Failed to set up new database. Deleting orphaned DB {}: {}", provider.name, new_uuid, error);
      let _ = self.d1_client.delete_database(&new_uuid).await;
      return Err(error);
    }

    provider.database_id = new_uuid;
    Ok(())
  }
}

fn persist_database_rotation(path: &Path, record: DatabaseRotationRecord) -> Result<(), ConfigError> {
  let content = fs::read_to_string(path)?;
  let mut document = content.parse::<toml_edit::DocumentMut>()?;
  let mut updated = false;
  if let Some(providers) = document.get_mut("providers").and_then(toml_edit::Item::as_array_of_tables_mut) {
    for provider in providers.iter_mut() {
      if provider.get("name").and_then(toml_edit::Item::as_str) == Some(record.provider_name.as_str()) {
        provider["database_id"] = toml_edit::value(record.new_database_id.as_str());
        updated = true;
        break;
      }
    }
  }
  if !updated {
    return Err(ConfigError::InvalidProviders(format!("Provider {} was not found in {}", record.provider_name, path.display())));
  }

  let retired_databases = document
    .entry("retired_databases")
    .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
    .as_array_of_tables_mut()
    .ok_or_else(|| ConfigError::InvalidProviders(format!("retired_databases in {} is not an array of tables", path.display())))?;
  let already_recorded = retired_databases.iter().any(|database| database.get("database_id").and_then(toml_edit::Item::as_str) == Some(record.retired_database_id.as_str()));
  if !already_recorded {
    let mut retired = toml_edit::Table::new();
    retired["provider"] = toml_edit::value(record.provider_name);
    retired["name"] = toml_edit::value(record.retired_database_name);
    retired["database_id"] = toml_edit::value(record.retired_database_id);
    retired["retired_at"] = toml_edit::value(record.retired_at);
    retired_databases.push(retired);
  }

  let mut temporary_os = path.as_os_str().to_os_string();
  temporary_os.push(".tmp");
  let temporary_path = PathBuf::from(temporary_os);
  if let Err(error) = fs::write(&temporary_path, document.to_string()) {
    let _ = fs::remove_file(&temporary_path);
    return Err(ConfigError::Io(error));
  }
  if let Err(error) = fs::rename(&temporary_path, path) {
    let _ = fs::remove_file(&temporary_path);
    return Err(ConfigError::Io(error));
  }
  Ok(())
}

#[cfg(test)]
#[path = "../../test/unit/importer/rotation.rs"]
mod tests;
