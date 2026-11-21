-- Migration: Allow Status = 2 (STAGED) in 0_import_progress and drop legacy in-place Status = 1 rows
CREATE TABLE IF NOT EXISTS "0_import_progress_new" (
  FileName TEXT PRIMARY KEY,
  CRC TEXT,
  LastProcessedLine INTEGER,
  LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0),
  Status TINYINT CHECK (Status IN (0, 1, 2)), -- 0 = COMPLETED, 1 = IN_PROGRESS, 2 = STAGED
  UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP
) WITHOUT ROWID;

INSERT OR REPLACE INTO
  "0_import_progress_new" (FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt)
SELECT FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt
FROM
  "0_import_progress"
WHERE
  FileName IS NOT NULL
  AND Status = 0;

DROP TABLE "0_import_progress";

ALTER TABLE "0_import_progress_new"
RENAME TO "0_import_progress";
