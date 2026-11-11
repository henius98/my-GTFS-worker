-- Migration: Prefix infrastructure tables with 0_, drop Provider column, and remove unused logs table.
DROP TABLE IF EXISTS logs;

CREATE TABLE IF NOT EXISTS import_progress (
  Provider TEXT,
  FileName TEXT,
  CRC TEXT,
  LastProcessedLine INTEGER,
  LastProcessedByte INTEGER NOT NULL DEFAULT 0,
  Status TINYINT,
  UpdatedAt DATETIME,
  PRIMARY KEY (Provider, FileName)
);
CREATE TABLE IF NOT EXISTS "0_import_progress" (
  FileName TEXT PRIMARY KEY,
  CRC TEXT,
  LastProcessedLine INTEGER,
  LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0),
  Status TINYINT CHECK (Status IN (0, 1)),
  UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
);
INSERT OR REPLACE INTO "0_import_progress" (FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt)
SELECT FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt FROM import_progress;
DROP TABLE import_progress;

CREATE TABLE IF NOT EXISTS dataset_versions (
  Provider TEXT PRIMARY KEY,
  ETag TEXT,
  UpdatedAt DATETIME
);
CREATE TABLE IF NOT EXISTS "0_dataset_versions" (
  ETag TEXT,
  UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO "0_dataset_versions" (ETag, UpdatedAt)
SELECT ETag, UpdatedAt FROM dataset_versions;
DROP TABLE dataset_versions;
