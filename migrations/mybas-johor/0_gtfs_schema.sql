-- Migration: Initial schema (Infrastructure + GTFS tables)
-- Provider: mybas-johor
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
  route_url TEXT,
  route_color TEXT,
  route_text_color TEXT
  -- FOREIGN KEY (agency_id) REFERENCES agency(agency_id)
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
  -- FOREIGN KEY (parent_station) REFERENCES stops(stop_id)
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

CREATE TABLE IF NOT EXISTS areas (area_id TEXT PRIMARY KEY, area_name TEXT);

CREATE TABLE IF NOT EXISTS fare_leg_rules (
  leg_group_id TEXT,
  from_area_id TEXT,
  to_area_id TEXT,
  fare_product_id TEXT
  -- FOREIGN KEY (from_area_id) REFERENCES areas(area_id),
  -- FOREIGN KEY (to_area_id) REFERENCES areas(area_id),
  -- FOREIGN KEY (fare_product_id) REFERENCES fare_products(fare_product_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_fare_leg_rules_import_key ON fare_leg_rules (leg_group_id, from_area_id, to_area_id, fare_product_id);

CREATE TABLE IF NOT EXISTS fare_media (fare_media_id TEXT PRIMARY KEY, fare_media_name TEXT, fare_media_type INTEGER);

CREATE TABLE IF NOT EXISTS fare_products (
  fare_product_id TEXT PRIMARY KEY,
  fare_product_name TEXT,
  amount REAL,
  currency TEXT,
  fare_media_id TEXT,
  rider_category_id TEXT
  -- FOREIGN KEY (fare_media_id) REFERENCES fare_media(fare_media_id),
  -- FOREIGN KEY (rider_category_id) REFERENCES rider_categories(rider_category_id)
);

CREATE TABLE IF NOT EXISTS rider_categories (rider_category_id TEXT PRIMARY KEY, rider_category_name TEXT, is_default_fare_category INTEGER);

CREATE TABLE IF NOT EXISTS stop_areas (
  area_id TEXT,
  stop_id TEXT,
  PRIMARY KEY (area_id, stop_id)
  -- FOREIGN KEY (area_id) REFERENCES areas(area_id),
  -- FOREIGN KEY (stop_id) REFERENCES stops(stop_id)
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
