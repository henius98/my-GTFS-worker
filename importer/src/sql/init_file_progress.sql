INSERT INTO "0_import_progress" (
  FileName,
  CRC,
  LastProcessedLine,
  LastProcessedByte,
  Status,
  UpdatedAt
)
VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
ON CONFLICT(FileName) DO UPDATE SET
  CRC = excluded.CRC,
  LastProcessedLine = excluded.LastProcessedLine,
  LastProcessedByte = excluded.LastProcessedByte,
  Status = 1,
  UpdatedAt = CURRENT_TIMESTAMP
WHERE
  "0_import_progress".CRC IS NOT excluded.CRC
  OR "0_import_progress".LastProcessedLine IS NOT excluded.LastProcessedLine
  OR "0_import_progress".LastProcessedByte IS NOT excluded.LastProcessedByte
  OR "0_import_progress".Status IS NOT 1
