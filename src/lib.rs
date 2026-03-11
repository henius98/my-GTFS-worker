use std::io::{Cursor, Read};
use worker::{event, Env, ScheduleContext, ScheduledEvent, Fetch, Request, RequestInit, Method, Result, D1Database};
use console_error_panic_hook;
use zip::ZipArchive;
use csv::StringRecord;

mod logger;
use logger::Logger;

#[event(scheduled)]
pub async fn main(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    console_error_panic_hook::set_once();
    
    let d1_result = env.d1("DB");
    let logger = Logger::new(env.d1("DB").ok());
    logger.info("Scheduled GTFS import started!");

    match d1_result {
        Ok(d1) => {
            if let Err(e) = import_gtfs(&env, &d1, &logger).await {
                logger.error(&format!("Failed GTFS import: {:?}", e));
            }
        }
        Err(e) => {
            logger.error(&format!("Failed to bind DB: {:?}", e));
        }
    }
}

// Add a fetch endpoint just so we can test it manually
#[event(fetch)]
pub async fn fetch_test(_req: Request, env: Env, _ctx: worker::Context) -> Result<worker::Response> {
    console_error_panic_hook::set_once();
    
    let d1_result = env.d1("DB");
    let logger = Logger::new(env.d1("DB").ok());

    let d1 = match d1_result {
        Ok(db) => db,
        Err(e) => {
            logger.error(&format!("Failed to bind DB: {:?}", e));
            return worker::Response::error(format!("Error: {:?}", e), 500);
        }
    };

    if let Err(e) = import_gtfs(&env, &d1, &logger).await {
        logger.error(&format!("Failed GTFS import: {:?}", e));
        return worker::Response::error(format!("Error: {:?}", e), 500);
    }
    worker::Response::ok("GTFS imported successfully via fetch trigger.")
}

async fn import_gtfs(env: &Env, d1: &D1Database, logger: &Logger) -> Result<()> {

    // Read the configuration URLs from the environment variable
    let base_url = env.var("GTFS_STATIC_URL")?.to_string();
    let enum_str = env.var("GTFS_STATIC_ENUM")?.to_string();
    let normalized_enum = enum_str.replace('\n', "").replace('\r', "");

    // Split the comma-separated enums array string
    for item in normalized_enum.split(',') {
        let trimmed_item = item.trim();
        if trimmed_item.is_empty() {
            continue;
        }
        
        let target_url = format!("{}{}", base_url, trimmed_item);
        logger.info(&format!("Downloading GTFS data from: {}", target_url));

        let req = Request::new_with_init(
            &target_url,
            &RequestInit {
                method: Method::Get,
                ..Default::default()
            },
        )?;
        
        let mut resp = Fetch::Request(req).send().await?;
        let bytes = resp.bytes().await?;
        logger.info(&format!("Downloaded ZIP size: {} bytes", bytes.len()));

        let cursor = Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor).map_err(|e| worker::Error::RustError(e.to_string()))?;

        // Process shapes.txt
        process_csv_file(&d1, &logger, &mut archive, "shapes.txt", "shapes",
            &["shape_id", "shape_pt_lat", "shape_pt_lon", "shape_pt_sequence", "shape_dist_traveled"])
            .await?;
            
        // Process routes.txt
        process_csv_file(&d1, &logger, &mut archive, "routes.txt", "routes",
            &["route_id", "agency_id", "route_short_name", "route_long_name", "route_type"])
            .await?;

        // Process stops.txt
        process_csv_file(&d1, &logger, &mut archive, "stops.txt", "stops",
             &["stop_id", "stop_code", "stop_name", "stop_lat", "stop_lon"])
             .await?;

        // Process calendar.txt
        process_csv_file(&d1, &logger, &mut archive, "calendar.txt", "calendar",
             &["service_id", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "start_date", "end_date"])
             .await?;

        // Process trips.txt
        process_csv_file(&d1, &logger, &mut archive, "trips.txt", "trips",
             &["route_id", "service_id", "trip_id", "trip_headsign", "direction_id", "shape_id"])
             .await?;
             
        // Process stop_times.txt
        process_csv_file(&d1, &logger, &mut archive, "stop_times.txt", "stop_times",
             &["trip_id", "arrival_time", "departure_time", "stop_id", "stop_sequence", "stop_headsign", "shape_dist_traveled"])
             .await?;

        logger.info(&format!("Finished dataset import for enum: {}", trimmed_item));
    }

    logger.info("Successfully imported all GTFS data into D1.");
    Ok(())
}

async fn process_csv_file<'a>(
    d1: &D1Database,
    logger: &Logger,
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    filename: &str,
    table_name: &str,
    columns: &[&str],
) -> Result<()> {
    let mut file = match archive.by_name(filename) {
        Ok(f) => f,
        Err(_) => {
            logger.warn(&format!("File {} not found in archive, skipping.", filename));
            return Ok(());
        }
    };
    logger.info(&format!("Processing {}...", filename));

    let mut content = String::new();
    file.read_to_string(&mut content).map_err(|e| worker::Error::RustError(e.to_string()))?;
    
    // Parse CSV
    let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_reader(content.as_bytes());
    let mut count = 0;
    
    // Map targeted specific CSV columns into SQL indices so differing headers don't cause bounds mismatch
    let headers = rdr.headers().map_err(|e| worker::Error::RustError(e.to_string()))?.clone();
    let mut indices: Vec<Option<usize>> = Vec::new();
    let mut missing_cols = Vec::new();
    
    for &col in columns {
        if let Some(pos) = headers.iter().position(|h| h == col) {
            indices.push(Some(pos));
        } else {
            indices.push(None);
            missing_cols.push(col);
        }
    }
    
    let mut extra_cols = Vec::new();
    for h in headers.iter() {
        if !columns.contains(&h) {
            extra_cols.push(h);
        }
    }
    
    if !missing_cols.is_empty() || !extra_cols.is_empty() {
        let mut msg = format!("Header mismatch in {}:", filename);
        if !missing_cols.is_empty() {
            msg.push_str(&format!(" Missing DB columns (will be null): {:?}", missing_cols));
        }
        if !extra_cols.is_empty() {
            msg.push_str(&format!(" Extra CSV columns (ignored): {:?}", extra_cols));
        }
        logger.warn(&msg);
    }
    
    let placeholders = (1..=columns.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ");
    let insert_query = format!("INSERT OR REPLACE INTO {} ({}) VALUES ({})", table_name, columns.join(", "), placeholders);
    
    // Create base prepared statement
    let stmt = d1.prepare(&insert_query);
    
    // We batch statements for insertion, to respect D1 limitations
    // Note: D1 allows up to 100 statements in a batch transaction. But we will process 50 statements per batch as a safety net.
    let mut batch = Vec::new();
    
    for result in rdr.records() {
        let record: StringRecord = result.map_err(|e| worker::Error::RustError(e.to_string()))?;
        
        // Dynamically bind ONLY the columns defined by SQL query dynamically!
        let mut params = Vec::new();
        for &idx_opt in &indices {
            if let Some(idx) = idx_opt {
                if let Some(val) = record.get(idx) {
                    params.push(worker::wasm_bindgen::JsValue::from_str(val));
                } else {
                    params.push(worker::wasm_bindgen::JsValue::null());
                }
            } else {
                params.push(worker::wasm_bindgen::JsValue::null());
            }
        }
        
        // Pass the slice to bind()
        let bind_stmt = match stmt.clone().bind(&params) {
            Ok(s) => s,
            Err(e) => return Err(worker::Error::RustError(e.to_string())),
        };
        
        batch.push(bind_stmt);
        count += 1;
        
        if batch.len() >= 50 {
            // Unused but expected to run. d1.batch expects Vec<Statement>
            match d1.batch(batch).await {
                Ok(_) => (),
                Err(e) => return Err(worker::Error::RustError(e.to_string())),
            }
            batch = Vec::new();
        }
    }
    
    // Execute remaining
    if !batch.is_empty() {
        match d1.batch(batch).await {
            Ok(_) => (),
            Err(e) => return Err(worker::Error::RustError(e.to_string())),
        }
    }
    
    logger.info(&format!("Inserted {} rows into table for {}", count, filename));
    Ok(())
}
