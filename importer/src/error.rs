use crate::config::ConfigError;
use crate::d1::D1Error;
use std::io;
use thiserror::Error;
use tokio::task::JoinError;

#[derive(Error, Debug)]
pub enum ProcessorError {
  #[error("D1 error: {0}")]
  D1(#[from] D1Error),
  #[error("configuration error: {0}")]
  Config(#[from] ConfigError),
  #[error("HTTP error: {0}")]
  Reqwest(#[from] reqwest::Error),
  #[error("Zip error: {0}")]
  Zip(#[from] zip::result::ZipError),
  #[error("CSV error: {0}")]
  Csv(#[from] csv::Error),
  #[error("I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("JSON error: {0}")]
  Json(#[from] serde_json::Error),
  #[error("serialized JSON was unexpectedly not UTF-8: {0}")]
  JsonUtf8(#[from] std::string::FromUtf8Error),
  #[error("task failed to join: {0}")]
  TaskJoin(#[from] JoinError),
  #[error("batch uploader closed before extraction completed")]
  UploaderClosed,
  #[error("database rotation error: {0}")]
  Rotation(String),
  #[error("importer invariant violated: {0}")]
  Invariant(String),
}
