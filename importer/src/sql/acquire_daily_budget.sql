WITH allowance AS MATERIALIZED (
  SELECT MIN(?, ? - (CASE WHEN Day = date('now') THEN Reserved ELSE 0 END)) AS Granted
  FROM "0_daily_import_budget"
  WHERE Id = 1
)
UPDATE "0_daily_import_budget"
SET
  Day = date('now'),
  Reserved = (CASE WHEN Day = date('now') THEN Reserved ELSE 0 END) + (SELECT Granted FROM allowance)
WHERE
  Id = 1
  AND date('now') = ?
  AND (SELECT Granted FROM allowance) > ?
RETURNING (SELECT Granted FROM allowance) AS Granted
