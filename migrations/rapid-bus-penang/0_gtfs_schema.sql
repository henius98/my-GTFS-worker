-- Migration: Initial schema (Infrastructure + GTFS tables)
-- Provider: rapid-bus-penang
-- ============================================================================
-- Infrastructure Tables
-- ============================================================================
CREATE TABLE IF NOT EXISTS "0_import_progress" (
  FileName TEXT PRIMARY KEY,
  CRC TEXT,
  LastProcessedLine INTEGER,
  LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0),
  Status TINYINT CHECK (Status IN (0, 1, 2)), -- 0 = COMPLETED, 1 = IN_PROGRESS, 2 = STAGED
  UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS "0_dataset_versions" (ETag TEXT, UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP);

-- ============================================================================
-- GTFS Tables (rapid-bus-penang)
-- ============================================================================
CREATE TABLE IF NOT EXISTS trips (
  route_id TEXT,
  service_id TEXT,
  trip_id VARCHAR(20),
  trip_headsign TEXT,
  direction_id INTEGER, -- 0 = from Jetty or 1 = end Jetty
  shape_id TEXT,
  PRIMARY KEY (route_id, service_id, trip_id)
  -- FOREIGN KEY (route_id) REFERENCES routes(route_id),
  -- FOREIGN KEY (service_id) REFERENCES calendar(service_id),
  -- FOREIGN KEY (shape_id) REFERENCES shapes(shape_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS calendar (
  service_id TEXT PRIMARY KEY,
  monday BOOLEAN,
  tuesday BOOLEAN,
  wednesday BOOLEAN,
  thursday BOOLEAN,
  friday BOOLEAN,
  saturday BOOLEAN,
  sunday BOOLEAN,
  start_date INTEGER,
  end_date INTEGER
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS routes (
  route_id TEXT PRIMARY KEY,
  agency_id TEXT,
  route_short_name TEXT,
  route_long_name TEXT,
  route_type INTEGER
  -- FOREIGN KEY (agency_id) REFERENCES agency(agency_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS shapes (
  shape_id TEXT,
  shape_pt_lat REAL,
  shape_pt_lon REAL,
  shape_pt_sequence INTEGER,
  shape_dist_traveled REAL,
  PRIMARY KEY (shape_id, shape_pt_sequence)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS stops (stop_id TEXT PRIMARY KEY, stop_code TEXT, stop_name TEXT, stop_lat REAL, stop_lon REAL) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS stop_times (
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  stop_headsign TEXT,
  shape_dist_traveled REAL,
  PRIMARY KEY (trip_id, stop_id, stop_sequence)
  -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
  -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agency (
  agency_name TEXT,
  agency_url TEXT,
  agency_timezone TEXT,
  agency_phone TEXT,
  agency_lang TEXT,
  agency_id TEXT PRIMARY KEY
) WITHOUT ROWID;
