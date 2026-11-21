SELECT *
FROM calendar_dates
WHERE service_id IN (SELECT value FROM json_each(?1))
