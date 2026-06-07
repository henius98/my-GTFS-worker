-- Migration: Initial schema (Infrastructure + GTFS tables)
-- Provider: mybas-johor
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
-- GTFS Tables (mybas-johor)
-- ============================================================================
CREATE TABLE IF NOT EXISTS trips (
    route_id TEXT,
    service_id TEXT,
    trip_id VARCHAR(20) PRIMARY KEY,
    trip_headsign TEXT,
    direction_id INTEGER,
    block_id TEXT,
    shape_id TEXT,
    wheelchair_accessible INTEGER
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
    route_desc TEXT,
    route_type INTEGER,
    route_url TEXT,
    route_color TEXT,
    route_text_color TEXT
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
    stop_desc TEXT,
    stop_lat REAL,
    stop_lon REAL,
    zone_id TEXT,
    stop_url TEXT,
    location_type INTEGER,
    parent_station TEXT
);

CREATE TABLE IF NOT EXISTS stop_times (
    trip_id TEXT,
    arrival_time TEXT,
    departure_time TEXT,
    stop_id TEXT,
    stop_sequence INTEGER,
    stop_headsign TEXT,
    shape_dist_traveled REAL,
    pickup_type INTEGER,
    drop_off_type INTEGER,
    PRIMARY KEY (trip_id, stop_sequence)
);

CREATE TABLE IF NOT EXISTS agency (
    agency_id TEXT PRIMARY KEY,
    agency_name TEXT,
    agency_url TEXT,
    agency_timezone TEXT,
    agency_phone TEXT,
    agency_lang TEXT
);

CREATE TABLE IF NOT EXISTS areas (area_id TEXT PRIMARY KEY, area_name TEXT);

CREATE TABLE IF NOT EXISTS fare_leg_rules (
    leg_group_id TEXT,
    from_area_id TEXT,
    to_area_id TEXT,
    fare_product_id TEXT
);

CREATE TABLE IF NOT EXISTS fare_media (
    fare_media_id TEXT PRIMARY KEY,
    fare_media_name TEXT,
    fare_media_type INTEGER
);

CREATE TABLE IF NOT EXISTS fare_products (
    fare_product_id TEXT PRIMARY KEY,
    fare_product_name TEXT,
    amount REAL,
    currency TEXT,
    fare_media_id TEXT,
    rider_category_id TEXT
);

CREATE TABLE IF NOT EXISTS rider_categories (
    rider_category_id TEXT PRIMARY KEY,
    rider_category_name TEXT,
    is_default_fare_category INTEGER
);

CREATE TABLE IF NOT EXISTS stop_areas (
    area_id TEXT,
    stop_id TEXT,
    PRIMARY KEY (area_id, stop_id)
);
