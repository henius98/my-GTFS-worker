UPDATE "0_import_progress"
SET
  Status = 0,
  UpdatedAt = CURRENT_TIMESTAMP
WHERE
  FileName = ?
  AND Status IS NOT 0
