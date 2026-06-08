-- Migration: Initial schema (Infrastructure + GTFS tables)
-- Provider: rapid-bus-mrtfeeder
-- ============================================================================
-- Infrastructure Tables
-- ============================================================================
CREATE TABLE IF NOT EXISTS logs (
    Id INTEGER PRIMARY KEY,
    Level TINYINT NOT NULL CHECK (Level IN (0, 1, 2, 3, 4, 5)),
    Message TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS import_progress (
    Provider TEXT,
    FileName TEXT,
    CRC TEXT,
    LastProcessedLine INTEGER,
    Status TINYINT CHECK (Status IN (0, 1)), -- 0 = COMPLETED, 1 = IN_PROGRESS
    UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (Provider, FileName)
);

CREATE TABLE IF NOT EXISTS dataset_versions (
    Provider TEXT PRIMARY KEY,
    ETag TEXT,
    UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- ============================================================================
-- GTFS Tables (rapid-bus-mrtfeeder)
-- ============================================================================
CREATE TABLE IF NOT EXISTS trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER,
    shape_id TEXT
);

CREATE TABLE IF NOT EXISTS calendar (
    service_id TEXT,
    monday BOOLEAN,
    tuesday BOOLEAN,
    wednesday BOOLEAN,
    thursday BOOLEAN,
    friday BOOLEAN,
    saturday BOOLEAN,
    sunday BOOLEAN,
    start_date INTEGER,
    end_date INTEGER,
    PRIMARY KEY (service_id, start_date, end_date)
);

CREATE TABLE IF NOT EXISTS routes (
    route_id TEXT PRIMARY KEY,
    agency_id TEXT,
    route_short_name TEXT,
    route_long_name TEXT,
    route_type INTEGER
);

CREATE TABLE IF NOT EXISTS shapes (
    shape_id TEXT,
    shape_pt_lat REAL,
    shape_pt_lon REAL,
    shape_pt_sequence INTEGER,
    shape_dist_traveled REAL,
    PRIMARY KEY (shape_id, shape_pt_sequence)
);

CREATE TABLE IF NOT EXISTS stops (
    stop_id TEXT PRIMARY KEY,
    stop_code TEXT,
    stop_name TEXT,
    stop_lat REAL,
    stop_lon REAL
);

CREATE TABLE IF NOT EXISTS stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    PRIMARY KEY (trip_id, stop_sequence)
);

CREATE TABLE IF NOT EXISTS agency (
    agency_name TEXT,
    agency_url TEXT,
    agency_timezone TEXT,
    agency_phone TEXT,
    agency_lang TEXT
);

CREATE TABLE IF NOT EXISTS calendar_dates (
    service_id TEXT,
    date INTEGER,
    exception_type INTEGER,
    PRIMARY KEY (service_id, date)
);
