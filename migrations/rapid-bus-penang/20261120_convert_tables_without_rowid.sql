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
  shape_id TEXT
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "trips_new" (route_id, service_id, trip_id, trip_headsign, direction_id, shape_id)
SELECT route_id, service_id, trip_id, trip_headsign, direction_id, shape_id
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

CREATE TABLE IF NOT EXISTS "routes_new" (route_id TEXT PRIMARY KEY, agency_id TEXT, route_short_name TEXT, route_long_name TEXT, route_type INTEGER) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "routes_new" (route_id, agency_id, route_short_name, route_long_name, route_type)
SELECT route_id, agency_id, route_short_name, route_long_name, route_type
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

CREATE TABLE IF NOT EXISTS "stops_new" (stop_id TEXT PRIMARY KEY, stop_code TEXT, stop_name TEXT, stop_lat REAL, stop_lon REAL) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "stops_new" (stop_id, stop_code, stop_name, stop_lat, stop_lon)
SELECT stop_id, stop_code, stop_name, stop_lat, stop_lon
FROM
  "stops"
WHERE
  stop_id IS NOT NULL;

DROP TABLE "stops";

ALTER TABLE "stops_new"
RENAME TO "stops";

CREATE TABLE IF NOT EXISTS "agency_new" (
  agency_name TEXT,
  agency_url TEXT,
  agency_timezone TEXT,
  agency_phone TEXT,
  agency_lang TEXT,
  agency_id TEXT PRIMARY KEY
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "agency_new" (agency_name, agency_url, agency_timezone, agency_phone, agency_lang, agency_id)
SELECT agency_name, agency_url, agency_timezone, agency_phone, agency_lang, agency_id
FROM
  "agency"
WHERE
  agency_id IS NOT NULL;

DROP TABLE "agency";

ALTER TABLE "agency_new"
RENAME TO "agency";
