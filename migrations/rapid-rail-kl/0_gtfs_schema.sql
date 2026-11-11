-- Migration: Initial schema (Infrastructure + GTFS tables)
-- Provider: rapid-rail-kl
-- ============================================================================
-- Infrastructure Tables
-- ============================================================================
CREATE TABLE IF NOT EXISTS "0_import_progress" (
  FileName TEXT PRIMARY KEY,
  CRC TEXT,
  LastProcessedLine INTEGER,
  LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0),
  Status TINYINT CHECK (Status IN (0, 1)), -- 0 = COMPLETED, 1 = IN_PROGRESS
  UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS "0_dataset_versions" (ETag TEXT, UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP);

-- ============================================================================
-- GTFS Tables (rapid-rail-kl)
-- ============================================================================
CREATE TABLE IF NOT EXISTS trips (
  route_id TEXT,
  service_id TEXT,
  trip_id VARCHAR(20) PRIMARY KEY,
  trip_headsign TEXT,
  direction_id INTEGER,
  shape_id TEXT
  -- FOREIGN KEY (route_id) REFERENCES routes(route_id),
  -- FOREIGN KEY (service_id) REFERENCES calendar(service_id),
  -- FOREIGN KEY (shape_id) REFERENCES shapes(shape_id)
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
  PRIMARY KEY (service_id, start_date)
);

CREATE TABLE IF NOT EXISTS routes (
  route_id TEXT PRIMARY KEY,
  agency_id TEXT,
  route_short_name TEXT,
  route_long_name TEXT,
  route_desc TEXT,
  route_type INTEGER,
  route_color TEXT,
  route_text_color TEXT,
  category TEXT,
  status TEXT
  -- FOREIGN KEY (agency_id) REFERENCES agency(agency_id)
);

CREATE TABLE IF NOT EXISTS shapes (
  shape_id TEXT,
  shape_pt_lon REAL,
  shape_pt_lat REAL,
  shape_pt_sequence INTEGER,
  PRIMARY KEY (shape_id, shape_pt_sequence)
);

CREATE TABLE IF NOT EXISTS stops (
  stop_id TEXT PRIMARY KEY,
  stop_name TEXT,
  stop_lat REAL,
  stop_lon REAL,
  category TEXT,
  route_id TEXT,
  geometry TEXT,
  isOKU INTEGER,
  status TEXT,
  search TEXT
  -- FOREIGN KEY (route_id) REFERENCES routes(route_id)
);

CREATE TABLE IF NOT EXISTS stop_times (
  route_id TEXT,
  direction_id INTEGER,
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  PRIMARY KEY (stop_id, trip_id, stop_sequence)
  -- FOREIGN KEY (route_id) REFERENCES routes(route_id),
  -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id),
  -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);

CREATE TABLE IF NOT EXISTS agency (
  agency_id TEXT PRIMARY KEY,
  agency_name TEXT,
  agency_url TEXT,
  agency_timezone TEXT,
  agency_phone TEXT,
  agency_lang TEXT
);

CREATE TABLE IF NOT EXISTS frequencies (
  trip_id TEXT,
  start_time TEXT,
  end_time TEXT,
  headway_secs INTEGER,
  PRIMARY KEY (trip_id, start_time)
  -- FOREIGN KEY (trip_id) REFERENCES trips(trip_id)
);

-- Calculated departures; empty route_id and direction_id -1 mean no filter.
CREATE TABLE IF NOT EXISTS departure_cache (
  stop_id TEXT NOT NULL,
  route_id TEXT NOT NULL DEFAULT '',
  direction_id INTEGER NOT NULL DEFAULT -1 CHECK (direction_id IN (-1, 0, 1)),
  feed_revision TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY (stop_id, route_id, direction_id)
  -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
);
