use super::*;
use crate::format::ColumnFormatter;
use std::io::Cursor;

#[test]
fn positional_json_escapes_values_and_represents_missing_fields_as_null() {
  let record = csv::StringRecord::from(vec!["quoted \"value\"", "line\nbreak"]);
  let columns = [
    PlannedColumn { csv_index: 0, db_column: "col_a", formatter: ColumnFormatter::None },
    PlannedColumn { csv_index: 1, db_column: "col_b", formatter: ColumnFormatter::None },
    PlannedColumn { csv_index: 2, db_column: "col_c", formatter: ColumnFormatter::None },
  ];
  let mut json = vec![b'['];
  assert!(append_positional_json_row(&mut json, &record, &columns).is_ok());
  json.push(b']');
  assert_eq!(String::from_utf8(json).ok().as_deref(), Some(r#"[["quoted \"value\"","line\nbreak",null]]"#));
}

#[test]
fn extract_and_batch_csv_formats_unpadded_gtfs_times_for_stop_times_and_frequencies() -> Result<(), Box<dyn std::error::Error>> {
  let stop_times_csv = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,6:00:00,6:01:00,S1,1\nT2,23:00:00,23:01:00,S1,1\nT3,9:59:00,9:59:30,S1,1\n";
  let stop_times_feed = TemporaryFeed::from_zip_entry("format-stop-times", "stop_times.txt", stop_times_csv.as_bytes())?;
  let (plan_tx, _plan_rx) = tokio::sync::oneshot::channel();
  let (tx, mut rx) = tokio::sync::mpsc::channel(4);
  let outcome = extract_and_batch_csv(
    CsvExtractJob {
      feed: stop_times_feed,
      extracted_csv: None,
      cache_extracted_csv: false,
      provider_name: "rapid-rail-kl".to_owned(),
      csv_file: "stop_times.txt".to_owned(),
      table_name: "stop_times".to_owned(),
      db_columns: &["trip_id", "arrival_time", "departure_time", "stop_id", "stop_sequence"],
      checkpoint: Checkpoint::default(),
      batch_size: 10,
      max_rows: 10,
      max_uncompressed_feed_bytes: 1024 * 1024,
      max_record_bytes: 1024,
      max_payload_bytes: 4096,
      d1_client: None,
      max_writes_per_row: 1,
      shutdown: ShutdownSignal::new(),
    },
    Arc::new(AtomicU64::new(0)),
    plan_tx,
    tx,
  )?;
  assert!(outcome.file_done);
  let stop_times_batch = rx.try_recv()?;
  assert_eq!(stop_times_batch.payload, r#"[["T1","06:00:00","06:01:00","S1","1"],["T2","23:00:00","23:01:00","S1","1"],["T3","09:59:00","09:59:30","S1","1"]]"#);

  let frequencies_csv = "trip_id,start_time,end_time,headway_secs\nT1,6:00:00,9:00:00,300\nT1,9:00:00,17:00:00,600\n";
  let frequencies_feed = TemporaryFeed::from_zip_entry("format-frequencies", "frequencies.txt", frequencies_csv.as_bytes())?;
  let (plan_tx, _plan_rx) = tokio::sync::oneshot::channel();
  let (tx, mut rx) = tokio::sync::mpsc::channel(4);
  let outcome = extract_and_batch_csv(
    CsvExtractJob {
      feed: frequencies_feed,
      extracted_csv: None,
      cache_extracted_csv: false,
      provider_name: "rapid-rail-kl".to_owned(),
      csv_file: "frequencies.txt".to_owned(),
      table_name: "frequencies".to_owned(),
      db_columns: &["trip_id", "start_time", "end_time", "headway_secs"],
      checkpoint: Checkpoint::default(),
      batch_size: 10,
      max_rows: 10,
      max_uncompressed_feed_bytes: 1024 * 1024,
      max_record_bytes: 1024,
      max_payload_bytes: 4096,
      d1_client: None,
      max_writes_per_row: 1,
      shutdown: ShutdownSignal::new(),
    },
    Arc::new(AtomicU64::new(0)),
    plan_tx,
    tx,
  )?;
  assert!(outcome.file_done);
  let frequencies_batch = rx.try_recv()?;
  assert_eq!(frequencies_batch.payload, r#"[["T1","06:00:00","09:00:00","300"],["T1","09:00:00","17:00:00","600"]]"#);
  Ok(())
}

#[test]
fn byte_checkpoint_resumes_at_a_quoted_record_boundary() -> Result<(), Box<dyn std::error::Error>> {
  let csv = "stop_id,stop_name\n1,alpha\n2,\"two\nlines\"\n3,gamma\n4,delta\n";
  let feed = TemporaryFeed::from_zip_entry("resume-test", "stops.txt", csv.as_bytes())?;
  let run_extract = |checkpoint: Checkpoint, max_rows: u64, max_payload_bytes: u64| -> Result<(CsvExtractOutcome, Vec<String>), Box<dyn std::error::Error>> {
    let (plan_tx, _plan_rx) = tokio::sync::oneshot::channel();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let outcome = extract_and_batch_csv(
      CsvExtractJob {
        feed: feed.clone(),
        extracted_csv: None,
        cache_extracted_csv: false,
        provider_name: "ktmb".to_owned(),
        csv_file: "stops.txt".to_owned(),
        table_name: "stops".to_owned(),
        db_columns: &["stop_id", "stop_name"],
        checkpoint,
        batch_size: 10,
        max_rows,
        max_uncompressed_feed_bytes: 1024 * 1024,
        max_record_bytes: 1024,
        max_payload_bytes,
        d1_client: None,
        max_writes_per_row: 1,
        shutdown: ShutdownSignal::new(),
      },
      Arc::new(AtomicU64::new(0)),
      plan_tx,
      tx,
    )?;
    let mut payloads = Vec::new();
    while let Ok(batch) = rx.try_recv() {
      payloads.push(batch.payload);
    }
    Ok((outcome, payloads))
  };

  let (first, first_payloads) = run_extract(Checkpoint::default(), 2, 32)?;
  assert!(!first.file_done);
  assert_eq!(first.checkpoint.line, 2);
  assert_eq!(usize::try_from(first.checkpoint.byte).ok(), csv.find("3,gamma"));
  assert_eq!(first_payloads, ["[[\"1\",\"alpha\"]]", "[[\"2\",\"two\\nlines\"]]"]);

  let (second, second_payloads) = run_extract(first.checkpoint, 10, 4096)?;
  assert!(second.file_done);
  assert_eq!(second.checkpoint, Checkpoint { line: 4, byte: u64::try_from(csv.len())? });
  assert_eq!(second_payloads.last().map(String::as_str), Some("[[\"3\",\"gamma\"],[\"4\",\"delta\"]]"));
  Ok(())
}

#[test]
fn row_budget_is_exact_under_contention() {
  let counter = Arc::new(AtomicU64::new(0));
  let claimed = Arc::new(AtomicU64::new(0));
  let mut threads = Vec::new();
  for _ in 0..8 {
    let counter = counter.clone();
    let claimed = claimed.clone();
    threads.push(std::thread::spawn(move || {
      while try_claim_row(&counter, 10_000) {
        claimed.fetch_add(1, Ordering::Relaxed);
      }
    }));
  }
  for thread in threads {
    assert!(thread.join().is_ok());
  }
  assert_eq!(counter.load(Ordering::Relaxed), 10_000);
  assert_eq!(claimed.load(Ordering::Relaxed), 10_000);
}

#[test]
fn oversized_csv_record_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
  let input = format!("id,name\n1,{}\n", "x".repeat(128 * 1024));
  let mut reader = csv::ReaderBuilder::new().has_headers(true).from_reader(RecordSizeLimiter::new(Cursor::new(input), 1024));
  let _ = reader.headers()?;
  let mut record = csv::StringRecord::new();
  let error = match read_bounded_record(&mut reader, &mut record, 1024) {
    Ok(_) => panic!("oversized record must fail"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("CSV record"));
  Ok(())
}

#[test]
fn committed_progress_stops_at_first_gap_or_failure() {
  let statuses = BTreeMap::from([
    (0, (Checkpoint { line: 100, byte: 1_000 }, true)),
    (100, (Checkpoint { line: 200, byte: 2_000 }, true)),
    (200, (Checkpoint { line: 300, byte: 3_000 }, false)),
    (300, (Checkpoint { line: 400, byte: 4_000 }, true)),
  ]);
  assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint { line: 200, byte: 2_000 });

  let statuses = BTreeMap::from([(100, (Checkpoint { line: 200, byte: 2_000 }, true))]);
  assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint::default());
}

#[test]
fn byte_only_producer_progress_does_not_require_a_d1_batch() {
  let initial = Checkpoint::default();
  let header_only = Checkpoint { line: 0, byte: 31 };

  assert!(producer_checkpoint_is_committed(header_only, initial));
  assert!(!producer_checkpoint_is_committed(Checkpoint { line: 1, byte: 31 }, initial));
  assert!(!producer_checkpoint_is_committed(initial, header_only));
}

#[test]
fn write_budget_exhaustion_is_not_reported_as_a_batch_failure() {
  let mut statuses = BTreeMap::new();
  let (completion, error) = record_task_result(
    Ok(BatchTaskResult {
      committed_ranges: vec![(Checkpoint::default(), Checkpoint { line: 50, byte: 500 })],
      uncommitted_ranges: vec![(Checkpoint { line: 50, byte: 500 }, Checkpoint { line: 100, byte: 1_000 })],
      result: Err(ProcessorError::D1(D1Error::WriteBudgetExhausted { requested: 100, remaining: 50 })),
    }),
    "test",
    &mut statuses,
  );
  assert_eq!(completion, BatchCompletion::WriteBudgetExhausted);
  assert!(error.is_none());
  assert_eq!(contiguous_committed_checkpoint(Checkpoint::default(), &statuses), Checkpoint { line: 50, byte: 500 });
}

#[test]
fn largest_fitting_prefix_shrinks_strictly_below_full_group() {
  let mk = |rows| SerializedBatch { payload: "[]".to_owned(), start: Checkpoint::default(), end: Checkpoint { line: rows, byte: rows }, rows };
  let group = vec![mk(2), mk(2), mk(2), mk(2)];
  assert_eq!(largest_fitting_prefix(&group, 3, 2), 0);
  assert_eq!(largest_fitting_prefix(&group, 4, 2), 1);
  assert_eq!(largest_fitting_prefix(&group, 9, 2), 2);
  assert_eq!(largest_fitting_prefix(&group, 100, 2), 3);

  assert_eq!(largest_fitting_prefix(&group, 1, 1), 0);
  assert_eq!(largest_fitting_prefix(&group, 2, 1), 1);
  assert_eq!(largest_fitting_prefix(&group, 5, 1), 2);
  assert_eq!(largest_fitting_prefix(&group, 100, 1), 3);
}

#[test]
fn cached_extracted_csv_resumes_directly_without_reopening_zip() -> Result<(), Box<dyn std::error::Error>> {
  let csv = "stop_id,stop_name\n1,alpha\n2,\"two\nlines\"\n3,gamma\n4,delta\n";
  let feed = TemporaryFeed::from_zip_entry_with_headroom("cached-resume", "stops.txt", csv.as_bytes(), 4096)?;

  let (plan_tx1, _plan_rx1) = tokio::sync::oneshot::channel();
  let (tx1, mut rx1) = tokio::sync::mpsc::channel(4);
  let first = extract_and_batch_csv(
    CsvExtractJob {
      feed: feed.clone(),
      extracted_csv: None,
      cache_extracted_csv: true,
      provider_name: "ktmb".to_owned(),
      csv_file: "stops.txt".to_owned(),
      table_name: "stops".to_owned(),
      db_columns: &["stop_id", "stop_name"],
      checkpoint: Checkpoint::default(),
      batch_size: 10,
      max_rows: 2,
      max_uncompressed_feed_bytes: 1024 * 1024,
      max_record_bytes: 1024,
      max_payload_bytes: 4096,
      d1_client: None,
      max_writes_per_row: 1,
      shutdown: ShutdownSignal::new(),
    },
    Arc::new(AtomicU64::new(0)),
    plan_tx1,
    tx1,
  )?;
  assert!(!first.file_done);
  assert_eq!(first.checkpoint.line, 2);
  let extracted = first.extracted_csv.ok_or("expected cached extracted csv")?;
  assert_eq!(rx1.try_recv()?.payload, "[[\"1\",\"alpha\"],[\"2\",\"two\\nlines\"]]");

  // Delete the original ZIP to prove wave 2 reads strictly via seek() on the extracted plain CSV.
  std::fs::remove_file(feed.path())?;

  let (plan_tx2, _plan_rx2) = tokio::sync::oneshot::channel();
  let (tx2, mut rx2) = tokio::sync::mpsc::channel(4);
  let second = extract_and_batch_csv(
    CsvExtractJob {
      feed,
      extracted_csv: Some(extracted),
      cache_extracted_csv: true,
      provider_name: "ktmb".to_owned(),
      csv_file: "stops.txt".to_owned(),
      table_name: "stops".to_owned(),
      db_columns: &["stop_id", "stop_name"],
      checkpoint: first.checkpoint,
      batch_size: 10,
      max_rows: 10,
      max_uncompressed_feed_bytes: 1024 * 1024,
      max_record_bytes: 1024,
      max_payload_bytes: 4096,
      d1_client: None,
      max_writes_per_row: 1,
      shutdown: ShutdownSignal::new(),
    },
    Arc::new(AtomicU64::new(0)),
    plan_tx2,
    tx2,
  )?;
  assert!(second.file_done);
  assert_eq!(second.checkpoint, Checkpoint { line: 4, byte: u64::try_from(csv.len())? });
  assert_eq!(rx2.try_recv()?.payload, "[[\"3\",\"gamma\"],[\"4\",\"delta\"]]");
  Ok(())
}

#[test]
fn extract_and_batch_csv_stops_immediately_when_shutdown_is_triggered() -> Result<(), Box<dyn std::error::Error>> {
  let feed = TemporaryFeed::from_zip_entry("shutdown-extract", "stops.txt", b"stop_id,stop_name\n1,alpha\n2,beta\n")?;
  let shutdown = ShutdownSignal::new();
  shutdown.trigger();
  let (plan_tx, mut plan_rx) = tokio::sync::oneshot::channel();
  let (tx, mut rx) = tokio::sync::mpsc::channel(4);
  let outcome = extract_and_batch_csv(
    CsvExtractJob {
      feed,
      extracted_csv: None,
      cache_extracted_csv: false,
      provider_name: "ktmb".to_owned(),
      csv_file: "stops.txt".to_owned(),
      table_name: "stops".to_owned(),
      db_columns: &["stop_id", "stop_name"],
      checkpoint: Checkpoint::default(),
      batch_size: 10,
      max_rows: 10,
      max_uncompressed_feed_bytes: 1024 * 1024,
      max_record_bytes: 1024,
      max_payload_bytes: 4096,
      d1_client: None,
      max_writes_per_row: 1,
      shutdown,
    },
    Arc::new(AtomicU64::new(0)),
    plan_tx,
    tx,
  )?;
  assert!(!outcome.file_done);
  assert_eq!(outcome.checkpoint, Checkpoint::default());
  assert!(plan_rx.try_recv()?.is_none());
  assert!(rx.try_recv().is_err());
  Ok(())
}

#[test]
fn effective_batch_size_caps_to_affordable_rows_for_remaining_write_budget() {
  assert_eq!(effective_batch_size(2_000, None, 1), 2_000);
  assert_eq!(effective_batch_size(2_000, Some(10_000), 1), 2_000);
  assert_eq!(effective_batch_size(2_000, Some(1_988), 1), 1_988);
  assert_eq!(effective_batch_size(2_000, Some(1_988), 2), 994);
  assert_eq!(effective_batch_size(2_000, Some(1), 2), 1);
  assert_eq!(effective_batch_size(2_000, Some(0), 1), 1);
}
