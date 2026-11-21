UPDATE "0_import_progress"
SET
  LastProcessedLine = ?,
  LastProcessedByte = ?,
  Status = ?,
  UpdatedAt = CURRENT_TIMESTAMP
WHERE
  FileName = ?
  AND (
    LastProcessedLine IS NOT ?
    OR LastProcessedByte IS NOT ?
    OR Status IS NOT ?
  )
