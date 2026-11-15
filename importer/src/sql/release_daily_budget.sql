UPDATE "0_daily_import_budget"
SET Reserved = Reserved - ?
WHERE Id = 1 AND Day = ? AND Reserved >= ?
