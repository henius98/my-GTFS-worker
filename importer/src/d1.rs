use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Semaphore;

#[derive(Error, Debug)]
pub enum D1Error {
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("D1 API error: {0}")]
    ApiError(String),
    #[error("D1 query failed: {0:?}")]
    QueryFailed(Option<Vec<serde_json::Value>>),
    #[error("D1 database full (exceeded maximum size): {0}")]
    DatabaseFull(String),
}

#[derive(Serialize)]
pub struct D1Query {
    pub sql: String,
    pub params: Vec<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
pub struct D1Response {
    pub success: bool,
    pub result: Option<Vec<D1Result>>,
    pub errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct D1Result {
    pub results: Vec<serde_json::Value>,
}

#[derive(Clone)]
pub struct D1Client {
    pub client: Client,
    account_id: String,
    api_token: String,
    concurrency_limit: Arc<Semaphore>,
}

impl D1Client {
    pub fn new(account_id: String, api_token: String) -> Self {
        let limit = std::env::var("D1_CONCURRENCY_LIMIT").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(5); //defualt 5

        Self {
            client: Client::new(),
            account_id,
            api_token,
            concurrency_limit: Arc::new(Semaphore::new(limit)),
        }
    }

    pub async fn query(&self, db_id: &str, query: D1Query) -> Result<Vec<D1Result>, D1Error> {
        let _permit = self.concurrency_limit.acquire().await.map_err(|e| D1Error::ApiError(format!("Failed to acquire semaphore: {}", e)))?;

        let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/d1/database/{}/query", self.account_id, db_id);

        let mut retries = 0;
        let max_retries = 3;
        let mut delay = std::time::Duration::from_millis(1000);

        loop {
            match self.client.post(&url).header("Authorization", format!("Bearer {}", self.api_token)).json(&query).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let err_text = resp.text().await.unwrap_or_default();

                        // Check for error code 7500 (exceeded maximum DB size) in HTTP error response
                        if Self::contains_error_code_7500(&err_text) {
                            return Err(D1Error::DatabaseFull(err_text));
                        }

                        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            if retries >= max_retries {
                                return Err(D1Error::ApiError(format!("HTTP {}: {}", status, err_text)));
                            }
                            println!("D1 API error ({}): {}, retrying {}/{} in {:?}", status, err_text, retries + 1, max_retries, delay);
                        } else {
                            return Err(D1Error::ApiError(err_text));
                        }
                    } else {
                        let d1_resp: D1Response = resp.json().await?;

                        if !d1_resp.success {
                            // Check for error code 7500 in D1 response errors
                            if let Some(ref errors) = d1_resp.errors {
                                for err in errors {
                                    if err.get("code").and_then(|c| c.as_u64()) == Some(7500) {
                                        let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("Exceeded maximum DB size");
                                        return Err(D1Error::DatabaseFull(msg.to_string()));
                                    }
                                }
                            }
                            return Err(D1Error::QueryFailed(d1_resp.errors));
                        }

                        return Ok(d1_resp.result.unwrap_or_default());
                    }
                }
                Err(e) => {
                    if retries >= max_retries {
                        return Err(D1Error::Reqwest(e));
                    }
                    println!("Reqwest error: {}, retrying {}/{} in {:?}", e, retries + 1, max_retries, delay);
                }
            }

            retries += 1;
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
    }

    pub async fn get_dataset_version(&self, db_id: &str, provider_name: &str) -> Result<Option<String>, D1Error> {
        let sql = "SELECT ETag FROM dataset_versions WHERE Provider = ?";
        let params = vec![serde_json::Value::String(provider_name.to_string())];
        let res = self.query(db_id, D1Query { sql: sql.to_string(), params }).await?;
        Ok(res
            .first()
            .and_then(|r| r.results.first())
            .and_then(|row| row.get("ETag"))
            .and_then(|v| v.as_str().map(|s| s.to_string())))
    }

    pub async fn set_dataset_version(&self, db_id: &str, provider: &str, etag: &str) -> Result<(), D1Error> {
        let sql = "INSERT OR REPLACE INTO dataset_versions (Provider, ETag, UpdatedAt) VALUES (?, ?, CURRENT_TIMESTAMP)";
        let params = vec![serde_json::Value::String(provider.to_string()), serde_json::Value::String(etag.to_string())];
        self.query(db_id, D1Query { sql: sql.to_string(), params }).await?;
        Ok(())
    }

    pub async fn get_incomplete_files_count(&self, db_id: &str, provider_name: &str) -> Result<u64, D1Error> {
        let sql = "SELECT COUNT(*) as count FROM import_progress WHERE Provider = ? AND Status != 0";
        let params = vec![serde_json::Value::String(provider_name.to_string())];
        let res = self.query(db_id, D1Query { sql: sql.to_string(), params }).await?;
        Ok(res.first().and_then(|r| r.results.first()).and_then(|row| row.get("count")).and_then(|v| v.as_u64()).unwrap_or(0))
    }

    pub async fn get_all_files_progress(&self, db_id: &str, provider_name: &str) -> Result<Vec<serde_json::Value>, D1Error> {
        let sql = "SELECT FileName, CRC, LastProcessedLine, Status FROM import_progress WHERE Provider = ?";
        let params = vec![serde_json::Value::String(provider_name.to_string())];

        let res = self.query(db_id, D1Query { sql: sql.to_string(), params }).await?;
        Ok(res.first().map(|r| r.results.clone()).unwrap_or_default())
    }

    pub async fn init_file_progress(&self, db_id: &str, provider: &str, file: &str, crc: &str, line: u64, status: i64) -> Result<(), D1Error> {
        self.query(
            db_id,
            D1Query {
                sql: "INSERT OR REPLACE INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status, UpdatedAt) VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)".to_string(),
                params: vec![
                    serde_json::Value::String(provider.to_string()),
                    serde_json::Value::String(file.to_string()),
                    serde_json::Value::String(crc.to_string()),
                    serde_json::Value::Number(line.into()),
                    serde_json::Value::Number(status.into()),
                ],
            },
        )
        .await?;
        Ok(())
    }

    pub async fn update_file_progress(&self, db_id: &str, provider: &str, file: &str, line: u64, status: i64) -> Result<(), D1Error> {
        self.query(
            db_id,
            D1Query {
                sql: "UPDATE import_progress SET LastProcessedLine = ?, Status = ?, UpdatedAt = CURRENT_TIMESTAMP WHERE Provider = ? AND FileName = ?".to_string(),
                params: vec![
                    serde_json::Value::Number(line.into()),
                    serde_json::Value::Number(status.into()),
                    serde_json::Value::String(provider.to_string()),
                    serde_json::Value::String(file.to_string()),
                ],
            },
        )
        .await?;
        Ok(())
    }

    /// Check if an error response body contains D1 error code 7500 (exceeded maximum DB size)
    fn contains_error_code_7500(body: &str) -> bool {
        // Parse as JSON and look for error code 7500 in the errors array
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) {
            if let Some(errors) = parsed.get("errors").and_then(|e| e.as_array()) {
                return errors.iter().any(|err| err.get("code").and_then(|c| c.as_u64()) == Some(7500));
            }
        }
        false
    }
}
