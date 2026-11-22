SELECT service_id, date, exception_type
FROM calendar_dates
WHERE service_id IN (SELECT value FROM json_each(?1))
