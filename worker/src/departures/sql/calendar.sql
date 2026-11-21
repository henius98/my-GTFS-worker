SELECT *
FROM calendar
WHERE service_id IN (SELECT value FROM json_each(?1))
