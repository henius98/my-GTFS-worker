-- Migration: Recreate eligible primary-key tables with WITHOUT ROWID
CREATE TABLE IF NOT EXISTS "0_import_progress_new" (
  FileName TEXT PRIMARY KEY,
  CRC TEXT,
  LastProcessedLine INTEGER,
  LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0),
  Status TINYINT CHECK (Status IN (0, 1)),
  UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "0_import_progress_new" (FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt)
SELECT FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt
FROM
  "0_import_progress"
WHERE
  FileName IS NOT NULL;

DROP TABLE "0_import_progress";

ALTER TABLE "0_import_progress_new"
RENAME TO "0_import_progress";

CREATE TABLE IF NOT EXISTS "trips_new" (
  route_id TEXT,
  service_id TEXT,
  trip_id VARCHAR(20) PRIMARY KEY,
  trip_headsign TEXT,
  direction_id INTEGER,
  block_id TEXT,
  shape_id TEXT,
  wheelchair_accessible INTEGER
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "trips_new" (route_id, service_id, trip_id, trip_headsign, direction_id, block_id, shape_id, wheelchair_accessible)
SELECT route_id, service_id, trip_id, trip_headsign, direction_id, block_id, shape_id, wheelchair_accessible
FROM
  "trips"
WHERE
  trip_id IS NOT NULL;

DROP TABLE "trips";

ALTER TABLE "trips_new"
RENAME TO "trips";

CREATE TABLE IF NOT EXISTS "calendar_new" (
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

INSERT OR REPLACE INTO
  "calendar_new" (service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date)
SELECT service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date
FROM
  "calendar"
WHERE
  service_id IS NOT NULL
  AND start_date IS NOT NULL;

DROP TABLE "calendar";

ALTER TABLE "calendar_new"
RENAME TO "calendar";

CREATE TABLE IF NOT EXISTS "routes_new" (
  route_id TEXT PRIMARY KEY,
  agency_id TEXT,
  route_short_name TEXT,
  route_long_name TEXT,
  route_desc TEXT,
  route_type INTEGER,
  route_url TEXT,
  route_color TEXT,
  route_text_color TEXT
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "routes_new" (
    route_id,
    agency_id,
    route_short_name,
    route_long_name,
    route_desc,
    route_type,
    route_url,
    route_color,
    route_text_color
  )
SELECT
  route_id,
  agency_id,
  route_short_name,
  route_long_name,
  route_desc,
  route_type,
  route_url,
  route_color,
  route_text_color
FROM
  "routes"
WHERE
  route_id IS NOT NULL;

DROP TABLE "routes";

ALTER TABLE "routes_new"
RENAME TO "routes";

CREATE TABLE IF NOT EXISTS "shapes_new" (
  shape_id TEXT,
  shape_pt_lat REAL,
  shape_pt_lon REAL,
  shape_pt_sequence INTEGER,
  shape_dist_traveled REAL,
  PRIMARY KEY (shape_id, shape_pt_sequence)
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "shapes_new" (shape_id, shape_pt_lat, shape_pt_lon, shape_pt_sequence, shape_dist_traveled)
SELECT shape_id, shape_pt_lat, shape_pt_lon, shape_pt_sequence, shape_dist_traveled
FROM
  "shapes"
WHERE
  shape_id IS NOT NULL
  AND shape_pt_sequence IS NOT NULL;

DROP TABLE "shapes";

ALTER TABLE "shapes_new"
RENAME TO "shapes";

CREATE TABLE IF NOT EXISTS "stops_new" (
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
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "stops_new" (stop_id, stop_code, stop_name, stop_desc, stop_lat, stop_lon, zone_id, stop_url, location_type, parent_station)
SELECT stop_id, stop_code, stop_name, stop_desc, stop_lat, stop_lon, zone_id, stop_url, location_type, parent_station
FROM
  "stops"
WHERE
  stop_id IS NOT NULL;

DROP TABLE "stops";

ALTER TABLE "stops_new"
RENAME TO "stops";

CREATE TABLE IF NOT EXISTS "agency_new" (
  agency_id TEXT PRIMARY KEY,
  agency_name TEXT,
  agency_url TEXT,
  agency_timezone TEXT,
  agency_phone TEXT,
  agency_lang TEXT
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "agency_new" (agency_id, agency_name, agency_url, agency_timezone, agency_phone, agency_lang)
SELECT agency_id, agency_name, agency_url, agency_timezone, agency_phone, agency_lang
FROM
  "agency"
WHERE
  agency_id IS NOT NULL;

DROP TABLE "agency";

ALTER TABLE "agency_new"
RENAME TO "agency";

CREATE TABLE IF NOT EXISTS "areas_new" (area_id TEXT PRIMARY KEY, area_name TEXT) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "areas_new" (area_id, area_name)
SELECT area_id, area_name
FROM
  "areas"
WHERE
  area_id IS NOT NULL;

DROP TABLE "areas";

ALTER TABLE "areas_new"
RENAME TO "areas";

CREATE TABLE IF NOT EXISTS "fare_leg_rules_new" (
  leg_group_id TEXT NOT NULL DEFAULT '',
  from_area_id TEXT NOT NULL DEFAULT '',
  to_area_id TEXT NOT NULL DEFAULT '',
  fare_product_id TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (leg_group_id, from_area_id, to_area_id, fare_product_id)
) WITHOUT ROWID;

DROP INDEX IF EXISTS uq_fare_leg_rules_import_key;

INSERT OR REPLACE INTO
  "fare_leg_rules_new" (leg_group_id, from_area_id, to_area_id, fare_product_id)
SELECT
  COALESCE(leg_group_id, ''),
  COALESCE(from_area_id, ''),
  COALESCE(to_area_id, ''),
  COALESCE(fare_product_id, '')
FROM
  "fare_leg_rules";

DROP TABLE "fare_leg_rules";

ALTER TABLE "fare_leg_rules_new"
RENAME TO "fare_leg_rules";

CREATE TABLE IF NOT EXISTS "fare_media_new" (fare_media_id TEXT PRIMARY KEY, fare_media_name TEXT, fare_media_type INTEGER) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "fare_media_new" (fare_media_id, fare_media_name, fare_media_type)
SELECT fare_media_id, fare_media_name, fare_media_type
FROM
  "fare_media"
WHERE
  fare_media_id IS NOT NULL;

DROP TABLE "fare_media";

ALTER TABLE "fare_media_new"
RENAME TO "fare_media";

CREATE TABLE IF NOT EXISTS "fare_products_new" (
  fare_product_id TEXT PRIMARY KEY,
  fare_product_name TEXT,
  amount REAL,
  currency TEXT,
  fare_media_id TEXT,
  rider_category_id TEXT
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "fare_products_new" (fare_product_id, fare_product_name, amount, currency, fare_media_id, rider_category_id)
SELECT fare_product_id, fare_product_name, amount, currency, fare_media_id, rider_category_id
FROM
  "fare_products"
WHERE
  fare_product_id IS NOT NULL;

DROP TABLE "fare_products";

ALTER TABLE "fare_products_new"
RENAME TO "fare_products";

CREATE TABLE IF NOT EXISTS "rider_categories_new" (rider_category_id TEXT PRIMARY KEY, rider_category_name TEXT, is_default_fare_category INTEGER) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "rider_categories_new" (rider_category_id, rider_category_name, is_default_fare_category)
SELECT rider_category_id, rider_category_name, is_default_fare_category
FROM
  "rider_categories"
WHERE
  rider_category_id IS NOT NULL;

DROP TABLE "rider_categories";

ALTER TABLE "rider_categories_new"
RENAME TO "rider_categories";

CREATE TABLE IF NOT EXISTS "stop_areas_new" (area_id TEXT, stop_id TEXT, PRIMARY KEY (area_id, stop_id)) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "stop_areas_new" (area_id, stop_id)
SELECT area_id, stop_id
FROM
  "stop_areas"
WHERE
  area_id IS NOT NULL
  AND stop_id IS NOT NULL;

DROP TABLE "stop_areas";

ALTER TABLE "stop_areas_new"
RENAME TO "stop_areas";
