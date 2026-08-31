use std::hint::black_box;
use std::io::{Cursor, Read, Write};
use std::time::{Duration, Instant};
use zip::ZipArchive;

const TOTAL_ROWS: usize = 250_000;
const RESUME_ROWS: usize = 225_000;
const TARGET_ROWS: usize = 1_000;
const MEASURED_RUNS: usize = 9;

fn build_archive() -> Result<(Vec<u8>, u64), Box<dyn std::error::Error>> {
  let mut csv = Vec::with_capacity(TOTAL_ROWS * 48);
  csv.extend_from_slice(b"id,name,description\n");
  let mut resume_byte = 0_u64;
  for row in 0..TOTAL_ROWS {
    if row == RESUME_ROWS {
      resume_byte = u64::try_from(csv.len())?;
    }
    if row % 1_000 == 0 {
      writeln!(csv, "{row},stop-{row},\"quoted row {row}\nsecond line\"")?;
    } else {
      writeln!(csv, "{row},stop-{row},description-{row}")?;
    }
  }

  let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
  let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
  writer.start_file("stops.txt", options)?;
  writer.write_all(&csv)?;
  Ok((writer.finish()?.into_inner(), resume_byte))
}

fn legacy_line_resume(archive_bytes: &[u8]) -> Result<usize, Box<dyn std::error::Error>> {
  let mut archive = ZipArchive::new(Cursor::new(archive_bytes))?;
  let file = archive.by_name("stops.txt")?;
  let mut reader = csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(file);
  let _ = reader.headers()?;
  let mut record = csv::StringRecord::new();
  for _ in 0..RESUME_ROWS {
    if !reader.read_record(&mut record)? {
      return Err("legacy scan reached EOF before its checkpoint".into());
    }
  }

  let mut checksum = 0_usize;
  for _ in 0..TARGET_ROWS {
    if !reader.read_record(&mut record)? {
      return Err("legacy scan reached EOF in its target range".into());
    }
    checksum = checksum.saturating_add(record.as_slice().len());
  }
  Ok(checksum)
}

fn byte_offset_resume(archive_bytes: &[u8], resume_byte: u64) -> Result<usize, Box<dyn std::error::Error>> {
  let mut archive = ZipArchive::new(Cursor::new(archive_bytes))?;
  {
    let file = archive.by_name("stops.txt")?;
    let mut header_reader = csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(file);
    let _ = header_reader.headers()?;
  }

  let file = archive.by_name("stops.txt")?;
  let mut prefix = file.take(resume_byte);
  let skipped = std::io::copy(&mut prefix, &mut std::io::sink())?;
  if skipped != resume_byte {
    return Err("byte resume reached EOF before its checkpoint".into());
  }
  let file = prefix.into_inner();
  let mut reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_reader(file);
  let mut record = csv::StringRecord::new();
  let mut checksum = 0_usize;
  for _ in 0..TARGET_ROWS {
    if !reader.read_record(&mut record)? {
      return Err("byte resume reached EOF in its target range".into());
    }
    checksum = checksum.saturating_add(record.as_slice().len());
  }
  Ok(checksum)
}

fn measure<F>(mut operation: F) -> Result<Vec<Duration>, Box<dyn std::error::Error>>
where
  F: FnMut() -> Result<usize, Box<dyn std::error::Error>>,
{
  black_box(operation()?);
  let mut samples = Vec::with_capacity(MEASURED_RUNS);
  for _ in 0..MEASURED_RUNS {
    let started = Instant::now();
    black_box(operation()?);
    samples.push(started.elapsed());
  }
  samples.sort_unstable();
  Ok(samples)
}

fn median(samples: &[Duration]) -> Duration {
  samples[samples.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let (archive, resume_byte) = build_archive()?;
  let legacy = measure(|| legacy_line_resume(&archive))?;
  let byte_offset = measure(|| byte_offset_resume(&archive, resume_byte))?;
  let before = median(&legacy);
  let after = median(&byte_offset);
  let change = (after.as_secs_f64() / before.as_secs_f64() - 1.0) * 100.0;

  println!("resume checkpoint benchmark: {TOTAL_ROWS} rows, resume after {RESUME_ROWS}, parse {TARGET_ROWS}, {MEASURED_RUNS} measured runs");
  println!("legacy line replay median: {:.3} ms", before.as_secs_f64() * 1_000.0);
  println!("byte-offset replay median: {:.3} ms", after.as_secs_f64() * 1_000.0);
  println!("elapsed-time change: {change:.2}%");
  if after.as_secs_f64() >= before.as_secs_f64() * 0.85 {
    return Err(format!("byte-offset resume regression: expected at least 15% lower median latency, observed {change:.2}%").into());
  }
  Ok(())
}
