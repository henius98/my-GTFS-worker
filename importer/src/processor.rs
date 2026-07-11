use crate::config::ProviderConfig;
use crate::d1::{D1Client, D1Error, D1Query};
use bytes::Bytes;

use std::io::Cursor;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::task::JoinSet;
use zip::ZipArchive;

#[derive(Error, Debug)]
pub enum ProcessorError {
    #[error("D1 error: {0}")]
    D1(#[from] D1Error),
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("D1 database full (exceeded maximum size) for provider: {0}")]
    DatabaseFull(String),
}

pub enum BatchMessage {
    InitSql(String),
    Data(Vec<serde_json::Value>, u64),
}

#[derive(Clone)]
struct BatchWorker {
    d1_client: D1Client,
    provider: ProviderConfig,
    csv_file: String,
    insert_sql: String,
    limit: usize,
    d1_semaphore: Arc<tokio::sync::Semaphore>,
}

impl BatchWorker {
    async fn flush_batch(self, json_batch: Vec<serde_json::Value>, last_processed: u64) -> Result<(), ProcessorError> {
        let _permit = self.d1_semaphore.acquire().await.map_err(|e| ProcessorError::D1(D1Error::ApiError(format!("Semaphore error: {}", e))))?;
        let batch_size = json_batch.len() as u64;
        let json_str = serde_json::to_string(&json_batch)?;

        if let Err(e) = self
            .d1_client
            .query(
                &self.provider.database_id,
                D1Query {
                    sql: self.insert_sql,
                    params: vec![serde_json::Value::String(json_str)],
                },
            )
            .await
        {
            self.d1_client
                .update_file_progress(&self.provider.database_id, &self.provider.name, &self.csv_file, last_processed.saturating_sub(batch_size), 1)
                .await?;
            return Err(e.into());
        }

        Ok(())
    }

    fn spawn_flush(&self, batch_tasks: &mut JoinSet<Result<(), ProcessorError>>, json_batch: &mut Vec<serde_json::Value>, last_processed: u64) {
        let batch_to_send = std::mem::take(json_batch);
        let ctx = self.clone();

        batch_tasks.spawn(async move { ctx.flush_batch(batch_to_send, last_processed).await });
    }

    async fn wait_for_slot(&self, batch_tasks: &mut JoinSet<Result<(), ProcessorError>>) -> bool {
        let mut error = false;
        while batch_tasks.len() >= self.limit {
            if let Some(res) = batch_tasks.join_next().await
                && handle_task_result(res, &self.provider.name)
            {
                error = true;
            }
        }
        error
    }
}

fn handle_task_result(res: Result<Result<(), ProcessorError>, tokio::task::JoinError>, provider_name: &str) -> bool {
    match res {
        Ok(Err(e)) => {
            println!("[{}] Batch insert failed: {}", provider_name, e);
            true
        }
        Err(e) => {
            println!("[{}] Task join error: {}", provider_name, e);
            true
        }
        _ => false,
    }
}

pub fn parse_provider_schemas(provider_name: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
    include!(concat!(env!("OUT_DIR"), "/schemas.rs"))
}

pub async fn check_etag(d1_client: &D1Client, provider: &ProviderConfig) -> Result<(String, String, bool), ProcessorError> {
    let target_url = format!("{}{}", provider.static_url, provider.static_provider);
    let mut head_resp = d1_client.client.head(&target_url).send().await?;
    if !head_resp.status().is_success() {
        head_resp = d1_client.client.get(&target_url).send().await?;
    }

    let remote_etag = head_resp.headers().get("ETag").and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
    let db_etag = d1_client.get_dataset_version(&provider.database_id, &provider.name).await?.unwrap_or_default();
    let etag_changed = db_etag != remote_etag && !remote_etag.is_empty();
    Ok((target_url, remote_etag, etag_changed))
}

fn get_resume_state(row: Option<&serde_json::Value>, csv_file: &str, file_crc: &str, provider_name: &str) -> Option<u64> {
    let mut last_processed = 0;
    let mut force_restart_file = false;

    if let Some(row) = row {
        let db_crc = row.get("CRC").and_then(|v| v.as_str()).unwrap_or("");
        if db_crc != file_crc {
            println!("[{}] File {} changed (CRC: {} -> {}). Restarting file.", provider_name, csv_file, db_crc, file_crc);
            force_restart_file = true;
        } else {
            let status = row.get("Status").and_then(|v| v.as_i64()).unwrap_or(-1);
            if status == 0 {
                println!("[{}] Skipping {}, already completed.", provider_name, csv_file);
                return None;
            }
            last_processed = row.get("LastProcessedLine").and_then(|v| v.as_u64()).unwrap_or(0);
        }
    }

    if force_restart_file {
        last_processed = 0;
    }

    Some(last_processed)
}

struct CsvExtractJob {
    bytes: Bytes,
    provider_name: String,
    csv_file: String,
    table_name: String,
    db_columns: &'static [&'static str],
    last_processed: u64,
}

fn extract_and_batch_csv(job: CsvExtractJob, rows_processed_this_run: Arc<AtomicU64>, tx: tokio::sync::mpsc::Sender<BatchMessage>) -> Result<(bool, bool, u64), ProcessorError> {
    let batch_size = std::env::var("QUERY_STATEMENT_BATCH_SIZE").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(500);
    let cursor = Cursor::new(job.bytes);
    let mut archive = ZipArchive::new(cursor)?;
    let file = archive.by_name(&job.csv_file)?;
    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_reader(file);
    let headers = rdr.headers()?.clone();

    let mut csv_indices = Vec::new();
    let mut matched_cols = Vec::new();
    for &db_col in job.db_columns {
        if let Some(pos) = headers.iter().position(|h| h == db_col) {
            csv_indices.push(pos);
            matched_cols.push(db_col.to_string());
        }
    }

    if matched_cols.is_empty() {
        println!("[{}] No matching columns in {}, skip", job.provider_name, job.csv_file);
        return Ok((true, false, job.last_processed));
    }

    let col_list = matched_cols.join(", ");
    let selects: Vec<String> = matched_cols.iter().map(|col| format!("json_extract(value, '$.{}')", col)).collect();
    let insert_sql = format!("INSERT OR REPLACE INTO {} ({}) SELECT {} FROM json_each(?)", job.table_name, col_list, selects.join(", "));

    if tx.blocking_send(BatchMessage::InitSql(insert_sql)).is_err() {
        return Ok((false, true, job.last_processed)); // Receiver dropped
    }

    let mut json_batch: Vec<serde_json::Value> = Vec::with_capacity(batch_size);
    let mut file_done = true;
    let mut local_last_processed = job.last_processed;
    let max_rows = std::env::var("MAX_ROWS_PER_RUN").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(100_000);

    // Fast-forward skip using a single reusable memory buffer
    let mut dummy_rec = csv::ByteRecord::new();
    for _ in 0..job.last_processed {
        let _ = rdr.read_byte_record(&mut dummy_rec);
    }

    for record in rdr.records() {
        if rows_processed_this_run.load(Ordering::Relaxed) >= max_rows {
            file_done = false;
            break;
        }
        let Ok(rec) = record else {
            continue;
        };

        let mut row_obj = serde_json::Map::new();
        for (i, &idx) in csv_indices.iter().enumerate() {
            let val = rec.get(idx).map(|v| serde_json::Value::String(v.to_string())).unwrap_or(serde_json::Value::Null);
            row_obj.insert(matched_cols[i].clone(), val);
        }

        json_batch.push(serde_json::Value::Object(row_obj));
        rows_processed_this_run.fetch_add(1, Ordering::Relaxed);
        local_last_processed += 1;

        if json_batch.len() >= batch_size {
            let batch_to_send = std::mem::take(&mut json_batch);
            if tx.blocking_send(BatchMessage::Data(batch_to_send, local_last_processed)).is_err() {
                return Ok((false, true, local_last_processed));
            }
        }
    }

    if !json_batch.is_empty() && tx.blocking_send(BatchMessage::Data(json_batch, local_last_processed)).is_err() {
        return Ok((false, true, local_last_processed));
    }

    Ok((file_done, false, local_last_processed))
}

#[derive(Clone)]
pub struct ProviderProcessor {
    d1_client: D1Client,
    provider: ProviderConfig,
    csv_semaphore: Arc<tokio::sync::Semaphore>,
    d1_semaphore: Arc<tokio::sync::Semaphore>,
    rows_processed_this_run: Arc<AtomicU64>,
}

impl ProviderProcessor {
    pub fn new(d1_client: D1Client, provider: ProviderConfig, csv_semaphore: Arc<tokio::sync::Semaphore>, d1_semaphore: Arc<tokio::sync::Semaphore>) -> Self {
        Self {
            d1_client,
            provider,
            csv_semaphore,
            d1_semaphore,
            rows_processed_this_run: Arc::new(AtomicU64::new(0)),
        }
    }

    async fn upload_batches(&self, csv_file: String, mut rx: tokio::sync::mpsc::Receiver<BatchMessage>) -> Result<bool, ProcessorError> {
        let mut worker_opt: Option<BatchWorker> = None;
        let mut file_error = false;
        let mut batch_tasks = JoinSet::new();

        while let Some(msg) = rx.recv().await {
            match msg {
                BatchMessage::InitSql(sql) => {
                    if worker_opt.is_none() {
                        worker_opt = Some(BatchWorker {
                            d1_client: self.d1_client.clone(),
                            provider: self.provider.clone(),
                            csv_file: csv_file.clone(),
                            insert_sql: sql,
                            limit: std::env::var("D1_CONCURRENCY_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(5),
                            d1_semaphore: self.d1_semaphore.clone(),
                        });
                    }
                }
                BatchMessage::Data(mut batch, processed_count) => {
                    if !batch.is_empty()
                        && let Some(worker) = worker_opt.as_ref()
                    {
                        if worker.wait_for_slot(&mut batch_tasks).await {
                            file_error = true;
                            break;
                        }
                        worker.spawn_flush(&mut batch_tasks, &mut batch, processed_count);
                    }
                }
            }
        }

        while let Some(res) = batch_tasks.join_next().await {
            if handle_task_result(res, &self.provider.name) {
                file_error = true;
            }
        }

        Ok(file_error)
    }

    pub async fn process_csv_file(&self, bytes: Bytes, csv_file: &str, file_crc: &str, table_name: &str, db_columns: &'static [&'static str], last_processed: u64) -> Result<bool, ProcessorError> {
        println!("[{}] Importing {} (resuming from {})", self.provider.name, csv_file, last_processed);
        self.d1_client
            .init_file_progress(&self.provider.database_id, &self.provider.name, csv_file, file_crc, last_processed, 1)
            .await?;

        let _permit = self
            .csv_semaphore
            .acquire()
            .await
            .map_err(|e| ProcessorError::D1(D1Error::ApiError(format!("Semaphore error: {}", e))))?;
        let (tx, rx) = tokio::sync::mpsc::channel(2);

        let job = CsvExtractJob {
            bytes,
            provider_name: self.provider.name.clone(),
            csv_file: csv_file.to_string(),
            table_name: table_name.to_string(),
            db_columns,
            last_processed,
        };

        let rows_processed_clone = self.rows_processed_this_run.clone();
        let blocking_handle = tokio::task::spawn_blocking(move || extract_and_batch_csv(job, rows_processed_clone, tx));

        let file_error_from_uploader = self.upload_batches(csv_file.to_string(), rx).await?;

        let blocking_res = blocking_handle.await.unwrap_or(Ok((false, true, last_processed)));
        let (file_done, blocking_error, final_last_processed) = blocking_res?;

        if file_error_from_uploader || blocking_error {
            return Err(ProcessorError::D1(D1Error::ApiError("Batch inserts failed".to_string())));
        }

        let status = if file_done { 0 } else { 1 };
        self.d1_client
            .update_file_progress(&self.provider.database_id, &self.provider.name, csv_file, final_last_processed, status)
            .await?;

        Ok(file_done)
    }
}

pub async fn process_provider(d1_client: &D1Client, provider: &ProviderConfig, csv_semaphore: Arc<tokio::sync::Semaphore>, d1_semaphore: Arc<tokio::sync::Semaphore>) -> Result<(), ProcessorError> {
    match process_provider_inner(d1_client, provider, csv_semaphore, d1_semaphore).await {
        Err(ProcessorError::D1(D1Error::DatabaseFull(_))) => {
            Err(ProcessorError::DatabaseFull(provider.name.clone()))
        }
        other => other,
    }
}

async fn process_provider_inner(d1_client: &D1Client, provider: &ProviderConfig, csv_semaphore: Arc<tokio::sync::Semaphore>, d1_semaphore: Arc<tokio::sync::Semaphore>) -> Result<(), ProcessorError> {
    let (target_url, remote_etag, etag_changed) = check_etag(d1_client, provider).await?;

    if !etag_changed {
        let incomplete = d1_client.get_incomplete_files_count(&provider.database_id, &provider.name).await?;
        if incomplete == 0 {
            println!("[{}] Zip unchanged and all files processed. Skipping.", provider.name);
            return Ok(());
        } else {
            println!("[{}] Zip unchanged, but {} files are incomplete. Resuming.", provider.name, incomplete);
        }
    } else {
        println!("[{}] Zip ETag changed (new: {}). Starting new import.", provider.name, remote_etag);
        d1_client.set_dataset_version(&provider.database_id, &provider.name, &remote_etag).await?;
    }

    println!("[{}] Downloading GTFS zip from {}", provider.name, target_url);
    let response = d1_client.client.get(&target_url).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    println!("[{}] Downloaded {} bytes", provider.name, bytes.len());

    let mut file_names_and_crcs = Vec::new();
    let schemas = parse_provider_schemas(&provider.name).unwrap_or(&[]);
    {
        let cursor = Cursor::new(&bytes);
        let mut archive = ZipArchive::new(cursor)?;
        for i in 0..archive.len() {
            if let Ok(file) = archive.by_index(i) {
                let csv_file = file.name().to_string();
                if !csv_file.ends_with(".txt") || csv_file.contains("__MACOSX") || csv_file.split('/').next_back().unwrap_or("").starts_with("._") {
                    continue;
                }

                let Some(table_name) = csv_file.strip_suffix(".txt") else {
                    continue;
                };

                let db_columns = schemas.iter().find(|(t, _)| *t == table_name).map(|(_, cols)| *cols);
                let Some(db_columns) = db_columns else {
                    println!("[{}] Skipping unsupported GTFS file: {} (no schema)", provider.name, table_name);
                    continue;
                };
                if db_columns.is_empty() {
                    println!("[{}] Skipping unsupported GTFS file: {} (empty schema)", provider.name, table_name);
                    continue;
                }

                let t_name = table_name.to_string();
                file_names_and_crcs.push((csv_file, format!("{:08x}", file.crc32()), t_name, db_columns));
            }
        }
    }

    let all_progress_rows = d1_client.get_all_files_progress(&provider.database_id, &provider.name).await?;
    let processor = Arc::new(ProviderProcessor::new(d1_client.clone(), provider.clone(), csv_semaphore.clone(), d1_semaphore.clone()));
    let mut csv_tasks = vec![];

    for (csv_file, crc_str, table_name, db_columns) in file_names_and_crcs {
        let row = all_progress_rows.iter().find(|r| r.get("FileName").and_then(|v| v.as_str()) == Some(csv_file.as_str()));
        let Some(last_processed) = get_resume_state(row, &csv_file, &crc_str, &provider.name) else {
            continue;
        };

        let processor_clone = Arc::clone(&processor);
        let bytes_clone = bytes.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = processor_clone.process_csv_file(bytes_clone, &csv_file, &crc_str, &table_name, db_columns, last_processed).await {
                println!("[{}] Error processing {}: {}", processor_clone.provider.name, csv_file, e);
                return Err(e);
            }
            Ok(())
        });
        csv_tasks.push(handle);
    }

    let mut has_csv_error = false;
    let mut has_db_full = false;
    for handle in csv_tasks {
        match handle.await {
            Ok(Err(ProcessorError::D1(D1Error::DatabaseFull(_)))) => {
                has_db_full = true;
            }
            Ok(Err(_e)) => has_csv_error = true,
            Err(e) => {
                println!("[{}] CSV task join error: {}", provider.name, e);
                has_csv_error = true;
            }
            _ => {}
        }
    }

    if has_db_full {
        return Err(ProcessorError::DatabaseFull(provider.name.clone()));
    }

    if has_csv_error {
        return Err(ProcessorError::D1(D1Error::ApiError("One or more CSV processing tasks failed".into())));
    }

    println!("[{}] Completed run. Total rows: {}", provider.name, processor.rows_processed_this_run.load(Ordering::Relaxed));
    Ok(())
}
