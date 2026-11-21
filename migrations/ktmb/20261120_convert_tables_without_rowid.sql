-- Migration: Recreate eligible primary-key tables with WITHOUT ROWID
CREATE TABLE IF NOT EXISTS "0_daily_import_budget_new" (Id INTEGER PRIMARY KEY CHECK (Id = 1), DAY TEXT NOT NULL, Reserved INTEGER NOT NULL CHECK (Reserved >= 0)) WITHOUT ROWID,
STRICT;

INSERT OR REPLACE INTO
  "0_daily_import_budget_new" (Id, DAY, Reserved)
SELECT Id, DAY, Reserved
FROM
  "0_daily_import_budget"
WHERE
  Id IS NOT NULL;

INSERT OR IGNORE INTO
  "0_daily_import_budget_new" (Id, DAY, Reserved)
VALUES
  (1, '', 0);

DROP TABLE "0_daily_import_budget";

ALTER TABLE "0_daily_import_budget_new"
RENAME TO "0_daily_import_budget";

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

CREATE TABLE IF NOT EXISTS "trips_new" (route_id TEXT, service_id TEXT, trip_id VARCHAR(20) PRIMARY KEY, direction_id INTEGER) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "trips_new" (route_id, service_id, trip_id, direction_id)
SELECT route_id, service_id, trip_id, direction_id
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

CREATE TABLE IF NOT EXISTS "calendar_dates_new" (service_id TEXT, date INTEGER, exception_type INTEGER, PRIMARY KEY (service_id, date)) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "calendar_dates_new" (service_id, date, exception_type)
SELECT service_id, date, exception_type
FROM
  "calendar_dates"
WHERE
  service_id IS NOT NULL
  AND date IS NOT NULL;

DROP TABLE "calendar_dates";

ALTER TABLE "calendar_dates_new"
RENAME TO "calendar_dates";

CREATE TABLE IF NOT EXISTS "routes_new" (
  agency_id TEXT,
  route_id TEXT PRIMARY KEY,
  route_type INTEGER,
  route_short_name TEXT,
  route_long_name TEXT,
  route_desc TEXT,
  route_url TEXT,
  route_color TEXT,
  route_text_color TEXT
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "routes_new" (
    agency_id,
    route_id,
    route_type,
    route_short_name,
    route_long_name,
    route_desc,
    route_url,
    route_color,
    route_text_color
  )
SELECT
  agency_id,
  route_id,
  route_type,
  route_short_name,
  route_long_name,
  route_desc,
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

CREATE TABLE IF NOT EXISTS "stops_new" (stop_id TEXT PRIMARY KEY, stop_name TEXT, stop_lat REAL, stop_lon REAL) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "stops_new" (stop_id, stop_name, stop_lat, stop_lon)
SELECT stop_id, stop_name, stop_lat, stop_lon
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
