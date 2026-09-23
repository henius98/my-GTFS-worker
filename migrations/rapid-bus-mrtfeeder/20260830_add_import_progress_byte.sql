-- Rebuild the small progress table so this migration is safe both for
-- existing databases and fresh databases whose base schema already includes
-- LastProcessedByte.
ALTER TABLE import_progress RENAME TO import_progress_legacy;

CREATE TABLE import_progress (
    Provider TEXT,
    FileName TEXT,
    CRC TEXT,
    LastProcessedLine INTEGER,
    LastProcessedByte INTEGER NOT NULL DEFAULT 0 CHECK (LastProcessedByte >= 0),
    Status TINYINT CHECK (Status IN (0, 1)),
    UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (Provider, FileName)
);

INSERT INTO import_progress (
    Provider,
    FileName,
    CRC,
    LastProcessedLine,
    LastProcessedByte,
    Status,
    UpdatedAt
)
SELECT
    Provider,
    FileName,
    CRC,
    LastProcessedLine,
    0,
    Status,
    UpdatedAt
FROM import_progress_legacy;

DROP TABLE import_progress_legacy;

-- Clean up the abandoned sidecar if an earlier preview migration created it.
DROP TABLE IF EXISTS import_progress_offsets;
