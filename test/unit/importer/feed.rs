use super::*;
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

impl TempFeedStorage {
  pub(crate) fn used_bytes(&self) -> u64 {
    self.used_bytes.load(Ordering::Relaxed)
  }
}

impl TemporaryFeed {
  pub(crate) fn reserved_bytes(&self) -> u64 {
    self.reserved_bytes
  }

  pub(crate) fn from_zip_entries(label: &str, entries: &[(&str, &[u8])]) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
    let archive = zip_bytes(entries)?;
    let seq = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-{label}-{}-{seq}.zip", std::process::id()));
    std::fs::write(&path, archive)?;
    let reserved_bytes = std::fs::metadata(&path)?.len();
    let storage = Arc::new(TempFeedStorage::new(reserved_bytes));
    storage.reserve(reserved_bytes)?;
    Ok(Arc::new(Self { path, storage, reserved_bytes }))
  }

  pub(crate) fn from_zip_entry(label: &str, entry_name: &str, content: &[u8]) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
    Self::from_zip_entries(label, &[(entry_name, content)])
  }

  pub(crate) fn from_zip_entry_with_headroom(label: &str, entry_name: &str, content: &[u8], headroom_bytes: u64) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
    let archive = zip_bytes(&[(entry_name, content)])?;
    let seq = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("my-gtfs-worker-{label}-{}-{seq}.zip", std::process::id()));
    std::fs::write(&path, archive)?;
    let reserved_bytes = std::fs::metadata(&path)?.len();
    let storage = Arc::new(TempFeedStorage::new(reserved_bytes.saturating_add(headroom_bytes)));
    storage.reserve(reserved_bytes)?;
    Ok(Arc::new(Self { path, storage, reserved_bytes }))
  }
}

pub(crate) struct InMemoryFeedBody(VecDeque<Result<Vec<u8>, ProcessorError>>);

impl FeedBody for InMemoryFeedBody {
  fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, ProcessorError>> {
    Box::pin(async move {
      match self.0.pop_front() {
        Some(Ok(chunk)) => Ok(Some(chunk)),
        Some(Err(error)) => Err(error),
        None => Ok(None),
      }
    })
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedFeedRequest {
  pub(crate) url: String,
  pub(crate) if_none_match: Option<String>,
}

#[derive(Default)]
pub(crate) struct InMemoryFeedFetcher {
  responses: Mutex<VecDeque<Result<FeedResponse, ProcessorError>>>,
  requests: Mutex<Vec<RecordedFeedRequest>>,
}

impl InMemoryFeedFetcher {
  pub(crate) fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  pub(crate) fn push_not_modified(&self) {
    self.push_response(Ok(FeedResponse::NotModified));
  }

  pub(crate) fn push_stream(&self, etag: &str, content_length: Option<u64>, chunks: Vec<Vec<u8>>) {
    let body = InMemoryFeedBody(chunks.into_iter().map(Ok).collect());
    self.push_response(Ok(FeedResponse::Stream(FeedStream { etag: etag.to_owned(), content_length, body: Box::new(body) })));
  }

  pub(crate) fn push_response(&self, response: Result<FeedResponse, ProcessorError>) {
    self.responses.lock().unwrap_or_else(PoisonError::into_inner).push_back(response);
  }

  pub(crate) fn recorded_requests(&self) -> Vec<RecordedFeedRequest> {
    self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }
}

impl FeedFetcher for InMemoryFeedFetcher {
  fn fetch<'a>(&'a self, url: &'a str, if_none_match: Option<&'a str>) -> BoxFuture<'a, Result<FeedResponse, ProcessorError>> {
    Box::pin(async move {
      self.requests.lock().unwrap_or_else(PoisonError::into_inner).push(RecordedFeedRequest { url: url.to_owned(), if_none_match: if_none_match.map(str::to_owned) });
      self.responses.lock().unwrap_or_else(PoisonError::into_inner).pop_front().ok_or_else(|| ProcessorError::Invariant(format!("no queued InMemoryFeedFetcher response for {url}")))?
    })
  }
}

pub(crate) fn zip_bytes(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
  use std::io::{Cursor, Write};
  let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
  let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
  for (name, content) in entries {
    writer.start_file(*name, options)?;
    writer.write_all(content)?;
  }
  Ok(writer.finish()?.into_inner())
}

#[test]
fn temporary_feed_storage_is_globally_bounded() {
  let storage = TempFeedStorage::new(10);
  assert!(storage.reserve(6).is_ok());
  assert!(storage.reserve(5).is_err());
  assert_eq!(storage.used_bytes(), 6);
  storage.release(6);
  assert!(storage.reserve(10).is_ok());
  assert_eq!(storage.used_bytes(), 10);
}

#[test]
fn declared_uncompressed_feed_size_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("expansion-test", "stops.txt", b"id,name\n1,a-name-that-exceeds-the-test-limit\n")?;
  let error = match discover_supported_files_sync(&feed, "test", &[("stops", &["id", "name"])], 8, 256) {
    Ok(_) => panic!("expanded feed must fail"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("expand"));
  Ok(())
}

#[test]
fn archive_entry_count_is_bounded_by_configured_limit() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entries("entries-test", &[("stops.txt", b"id,name\n1,A\n"), ("routes.txt", b"id,name\n1,B\n")])?;
  let error = match discover_supported_files_sync(&feed, "test", &[("stops", &["id", "name"])], 1024, 1) {
    Ok(_) => panic!("archive with excess entries must fail"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("archive entries"));
  Ok(())
}

#[tokio::test]
async fn download_returns_none_on_304_and_streams_to_temp_storage_on_200() -> Result<(), Box<dyn std::error::Error>> {
  let archive = zip_bytes(&[("stops.txt", b"stop_id,stop_name\nS1,Alpha\n")])?;
  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_not_modified();
  fetcher.push_stream("\"v1\"", Some(u64::try_from(archive.len())?), vec![archive.clone()]);
  let storage = Arc::new(TempFeedStorage::new(1024 * 1024));
  let store = FeedStore::new(fetcher.clone(), storage.clone(), 1024 * 1024, 1024 * 1024, 256);

  let not_modified = store.download("https://example.invalid/ktmb", Some("\"v1\"")).await?;
  assert!(not_modified.is_none());

  let downloaded = store.download("https://example.invalid/ktmb", None).await?.ok_or("expected downloaded feed")?;
  assert_eq!(downloaded.size, u64::try_from(archive.len())?);
  assert_eq!(downloaded.remote_etag, "\"v1\"");
  assert_eq!(storage.used_bytes(), downloaded.size);
  assert_eq!(
    fetcher.recorded_requests(),
    vec![
      RecordedFeedRequest { url: "https://example.invalid/ktmb".to_owned(), if_none_match: Some("\"v1\"".to_owned()) },
      RecordedFeedRequest { url: "https://example.invalid/ktmb".to_owned(), if_none_match: None },
    ]
  );
  drop(downloaded);
  assert_eq!(storage.used_bytes(), 0);
  Ok(())
}

#[tokio::test]
async fn download_enforces_size_limits_and_reclaims_temp_storage_on_streaming_abort() -> Result<(), Box<dyn std::error::Error>> {
  let fetcher = InMemoryFeedFetcher::new();
  fetcher.push_stream("etag-1", Some(100), vec![vec![0; 10]]);
  fetcher.push_stream("etag-2", None, vec![vec![1; 6], vec![2; 6]]);
  let storage = Arc::new(TempFeedStorage::new(100));
  let store = FeedStore::new(fetcher, storage.clone(), 10, 100, 256);

  let declared_err = store.download("https://example.invalid/ktmb", None).await;
  assert!(declared_err.is_err());
  assert_eq!(storage.used_bytes(), 0);

  let stream_err = store.download("https://example.invalid/ktmb", None).await;
  assert!(stream_err.is_err());
  assert_eq!(storage.used_bytes(), 0);
  Ok(())
}

#[test]
fn try_extract_zip_entry_caches_csv_within_temp_storage_and_falls_back_when_full() -> Result<(), Box<dyn std::error::Error>> {
  let csv_content = b"stop_id,stop_name\nS1,Alpha\nS2,Beta\n";
  let archive = zip_bytes(&[("stops.txt", csv_content)])?;
  let seq = TEMP_FEED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
  let path = std::env::temp_dir().join(format!("my-gtfs-worker-extract-cache-test-{}-{seq}.zip", std::process::id()));
  std::fs::write(&path, &archive)?;
  let zip_size = std::fs::metadata(&path)?.len();
  let storage = Arc::new(TempFeedStorage::new(zip_size + u64::try_from(csv_content.len())?));
  storage.reserve(zip_size)?;
  let feed = Arc::new(TemporaryFeed { path, storage: storage.clone(), reserved_bytes: zip_size });

  let extracted = feed.try_extract_zip_entry("stops.txt", 1024)?.ok_or("expected extracted csv")?;
  assert_eq!(std::fs::read(extracted.path())?, csv_content);
  assert_eq!(storage.used_bytes(), zip_size + u64::try_from(csv_content.len())?);
  // Temp storage is now full; a second extraction gracefully returns None instead of failing.
  assert!(feed.try_extract_zip_entry("stops.txt", 1024)?.is_none());

  let extracted_path = extracted.path().to_path_buf();
  drop(extracted);
  assert!(!extracted_path.exists());
  assert_eq!(storage.used_bytes(), zip_size);
  drop(feed);
  assert_eq!(storage.used_bytes(), 0);
  Ok(())
}
