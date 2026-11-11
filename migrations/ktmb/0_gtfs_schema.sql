-- Migration: Initial schema (Infrastructure + GTFS tables)
-- Provider: ktmb
-- ============================================================================
-- Infrastructure Tables
-- ============================================================================
CREATE TABLE IF NOT EXISTS "0_daily_import_budget" (Id INTEGER PRIMARY KEY CHECK (Id = 1), DAY TEXT NOT NULL, Reserved INTEGER NOT NULL CHECK (Reserved >= 0));

INSERT OR IGNORE INTO
  "0_daily_import_budget" (Id, DAY, Reserved)
VALUES
  (1, '', 0);

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
-- GTFS Tables (ktmb)
-- ============================================================================
CREATE TABLE IF NOT EXISTS trips (
  route_id TEXT,
  service_id TEXT,
  trip_id VARCHAR(20) PRIMARY KEY,
  direction_id INTEGER
  -- FOREIGN KEY (route_id) REFERENCES routes(route_id),
  -- FOREIGN KEY (service_id) REFERENCES calendar(service_id)
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

CREATE TABLE IF NOT EXISTS calendar_dates (
  service_id TEXT,
  date INTEGER,
  exception_type INTEGER,
  PRIMARY KEY (service_id, date)
  -- FOREIGN KEY (service_id) REFERENCES calendar(service_id)
);

CREATE TABLE IF NOT EXISTS routes (
  agency_id TEXT,
  route_id TEXT PRIMARY KEY,
  route_type INTEGER,
  route_short_name TEXT,
  route_long_name TEXT,
  route_desc TEXT,
  route_url TEXT,
  route_color TEXT,
  route_text_color TEXT
  -- FOREIGN KEY (agency_id) REFERENCES agency(agency_id)
);

CREATE TABLE IF NOT EXISTS stops (stop_id TEXT PRIMARY KEY, stop_name TEXT, stop_lat REAL, stop_lon REAL);

CREATE TABLE IF NOT EXISTS stop_times (
  trip_id TEXT,
  arrival_time TEXT,
  departure_time TEXT,
  stop_id TEXT,
  stop_sequence INTEGER,
  shape_dist_traveled REAL,
  PRIMARY KEY (stop_id, trip_id, stop_sequence)
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
