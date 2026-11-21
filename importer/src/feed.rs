use crate::config::RuntimeConfig;
use crate::d1::BoxFuture;
use crate::error::ProcessorError;
use crate::schema::SchemaCatalog;
use reqwest::{
  Client, StatusCode,
  header::{CONTENT_LENGTH, ETAG, IF_NONE_MATCH},
};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
  Arc,
  atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use zip::ZipArchive;

static TEMP_FEED_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub trait FeedBody: Send {
  fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProcessorError>>;
}

pub struct FeedStream {
  pub etag: String,
  pub content_length: Option<u64>,
  pub body: Box<dyn FeedBody>,
}

pub enum FeedResponse {
  NotModified,
  Stream(FeedStream),
}

pub trait FeedFetcher: Send + Sync {
  fn fetch<'a>(&'a self, url: &'a str, if_none_match: Option<&'a str>) -> BoxFuture<'a, Result<FeedResponse, ProcessorError>>;
}

struct ReqwestFeedBody(reqwest::Response);

impl FeedBody for ReqwestFeedBody {
  fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProcessorError>> {
    Box::pin(async move { Ok(self.0.chunk().await?.map(|chunk| chunk.to_vec())) })
  }
}

#[derive(Clone)]
pub struct HttpFeedFetcher {
  client: Client,
}

impl HttpFeedFetcher {
  pub fn new(runtime: &RuntimeConfig) -> Result<Self, ProcessorError> {
    let client = Client::builder().connect_timeout(runtime.http_connect_timeout).timeout(runtime.http_request_timeout).user_agent(concat!("my-GTFS-worker/", env!("CARGO_PKG_VERSION"))).build()?;
    Ok(Self { client })
  }
}

impl FeedFetcher for HttpFeedFetcher {
  fn fetch<'a>(&'a self, url: &'a str, if_none_match: Option<&'a str>) -> BoxFuture<'a, Result<FeedResponse, ProcessorError>> {
    Box::pin(async move {
      let mut request = self.client.get(url);
      if let Some(etag) = if_none_match.filter(|etag| !etag.is_empty()) {
        request = request.header(IF_NONE_MATCH, etag);
      }
      let response = request.send().await?;
      if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(FeedResponse::NotModified);
      }
      let response = response.error_for_status()?;
      let etag = response.headers().get(ETAG).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
      let content_length = response.headers().get(CONTENT_LENGTH).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok());
      Ok(FeedResponse::Stream(FeedStream { etag, content_length, body: Box::new(ReqwestFeedBody(response)) }))
    })
  }
}

pub(crate) struct TempFeedStorage {
  used_bytes: AtomicU64,
  maximum_bytes: u64,
}

impl TempFeedStorage {
  pub(crate) fn new(maximum_bytes: u64) -> Self {
    Self { used_bytes: AtomicU64::new(0), maximum_bytes }
  }

  pub(crate) fn reserve(&self, bytes: u64) -> Result<(), io::Error> {
    self
      .used_bytes
      .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(bytes).filter(|total| *total <= self.maximum_bytes))
      .map(|_| ())
      .map_err(|current| io::Error::other(format!("temporary feed storage would exceed its {}-byte limit ({current} bytes already reserved, {bytes} requested)", self.maximum_bytes)))
  }

  pub(crate) fn release(&self, bytes: u64) {
    self.used_bytes.fetch_sub(bytes, Ordering::Relaxed);
  }
}

pub(crate) struct TemporaryFeed {
  path: PathBuf,
  storage: Arc<TempFeedStorage>,
  reserved_bytes: u64,
}

impl Drop for TemporaryFeed {
  fn drop(&mut self) {
    match std::fs::remove_file(&self.path) {
      Ok(()) => self.storage.release(self.reserved_bytes),
      Err(error) if error.kind() == io::ErrorKind::NotFound => self.storage.release(self.reserved_bytes),
      Err(error) => {
        eprintln!("Failed to remove temporary feed {}: {error}", self.path.display());
      }
    }
  }
}

impl TemporaryFeed {
  pub(crate) fn path(&self) -> &Path {
    &self.path
  }

  fn reserve(&mut self, bytes: u64) -> Result<(), io::Error> {
    self.storage.reserve(bytes)?;
    match self.reserved_bytes.checked_add(bytes) {
      Some(total) => {
        self.reserved_bytes = total;
        Ok(())
      }
      None => {
        self.storage.release(bytes);
        Err(io::Error::other("temporary feed reservation overflowed u64"))
      }
    }
  }
}

pub(crate) struct DownloadedFeed {
  pub(crate) file: Arc<TemporaryFeed>,
  pub(crate) size: u64,
  pub(crate) remote_etag: String,
}

pub(crate) struct DiscoveredFile {
  pub(crate) csv_file: String,
  pub(crate) crc: String,
  pub(crate) table_name: String,
  pub(crate) db_columns: &'static [&'static str],
}

#[derive(Clone)]
pub(crate) struct FeedStore {
  fetcher: Arc<dyn FeedFetcher>,
  temp_storage: Arc<TempFeedStorage>,
  max_download_bytes: u64,
  max_uncompressed_bytes: u64,
  max_archive_entries: usize,
}

impl FeedStore {
  pub(crate) fn new(fetcher: Arc<dyn FeedFetcher>, temp_storage: Arc<TempFeedStorage>, max_download_bytes: u64, max_uncompressed_bytes: u64, max_archive_entries: usize) -> Self {
    Self { fetcher, temp_storage, max_download_bytes, max_uncompressed_bytes, max_archive_entries }
  }

  pub(crate) async fn download(&self, url: &str, if_none_match: Option<&str>) -> Result<Option<DownloadedFeed>, ProcessorError> {
    let response = self.fetcher.fetch(url, if_none_match.filter(|etag| !etag.is_empty())).await?;
    let FeedResponse::Stream(mut stream) = response else {
      return Ok(None);
    };

    if let Some(content_length) = stream.content_length
      && content_length > self.max_download_bytes
    {
      return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed declares {content_length} bytes, exceeding the configured {}-byte download limit", self.max_download_bytes)).into());
    }

    let (mut temporary_feed, mut file) = create_temporary_feed(self.temp_storage.clone()).await?;
    let mut size = 0_u64;
    while let Some(chunk) = stream.body.next_chunk().await? {
      let chunk_len = u64::try_from(chunk.len()).map_err(|_| io::Error::other("feed chunk length does not fit in u64"))?;
      size = size.checked_add(chunk_len).ok_or_else(|| io::Error::other("downloaded feed size overflowed u64"))?;
      if size > self.max_download_bytes {
        drop(file);
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed exceeded the configured {}-byte download limit while streaming", self.max_download_bytes)).into());
      }
      temporary_feed.reserve(chunk_len)?;
      file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    Ok(Some(DownloadedFeed { file: Arc::new(temporary_feed), size, remote_etag: stream.etag }))
  }

  pub(crate) async fn discover_supported_files(
    &self,
    feed: Arc<TemporaryFeed>,
    provider_name: String,
    schemas: &'static [(&'static str, &'static [&'static str])],
  ) -> Result<Vec<DiscoveredFile>, ProcessorError> {
    let maximum_uncompressed_bytes = self.max_uncompressed_bytes;
    let maximum_archive_entries = self.max_archive_entries;
    tokio::task::spawn_blocking(move || discover_supported_files_sync(&feed, &provider_name, schemas, maximum_uncompressed_bytes, maximum_archive_entries)).await?
  }
}

async fn create_temporary_feed(storage: Arc<TempFeedStorage>) -> Result<(TemporaryFeed, tokio::fs::File), ProcessorError> {
  for _ in 0..16 {
    let sequence = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-{}-{timestamp}-{sequence}.zip", std::process::id()));
    match tokio::fs::OpenOptions::new().write(true).create_new(true).open(&path).await {
      Ok(file) => {
        return Ok((TemporaryFeed { path, storage, reserved_bytes: 0 }, file));
      }
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error.into()),
    }
  }
  Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate a unique temporary feed path after 16 attempts").into())
}

fn discover_supported_files_sync(
  feed: &TemporaryFeed,
  provider_name: &str,
  schemas: &'static [(&'static str, &'static [&'static str])],
  maximum_uncompressed_bytes: u64,
  maximum_archive_entries: usize,
) -> Result<Vec<DiscoveredFile>, ProcessorError> {
  let mut archive = ZipArchive::new(File::open(feed.path())?)?;
  if archive.len() > maximum_archive_entries {
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("feed contains {} archive entries, exceeding the configured maximum of {maximum_archive_entries}", archive.len())).into());
  }
  let mut files = Vec::new();
  let mut uncompressed_bytes = 0_u64;
  for index in 0..archive.len() {
    let file = archive.by_index(index)?;
    let csv_file = file.name()?.into_owned();
    let base_name = csv_file.rsplit('/').next().unwrap_or("");
    if csv_file.contains("__MACOSX") || base_name.starts_with("._") {
      continue;
    }
    let Some(table_name) = base_name.strip_suffix(".txt") else {
      continue;
    };
    if SchemaCatalog::is_infrastructure_table(table_name) {
      continue;
    }
    let Some(db_columns) = schemas.iter().find(|(schema_table, _)| *schema_table == table_name).map(|(_, columns)| *columns) else {
      println!("[{provider_name}] Skipping unsupported GTFS file: {table_name} (no schema)");
      continue;
    };
    if db_columns.is_empty() {
      println!("[{provider_name}] Skipping unsupported GTFS file: {table_name} (empty schema)");
      continue;
    }

    uncompressed_bytes = uncompressed_bytes.checked_add(file.size()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "supported CSV sizes overflowed u64"))?;
    if uncompressed_bytes > maximum_uncompressed_bytes {
      return Err(
        io::Error::new(io::ErrorKind::InvalidData, format!("supported CSV files expand to {uncompressed_bytes} bytes, exceeding the configured {maximum_uncompressed_bytes}-byte limit")).into(),
      );
    }

    let table_name = table_name.to_owned();
    files.push(DiscoveredFile { csv_file, crc: format!("{:08x}", file.crc32()), table_name, db_columns });
  }
  Ok(files)
}

#[cfg(test)]
#[path = "../../test/unit/importer/feed.rs"]
pub(crate) mod tests;
