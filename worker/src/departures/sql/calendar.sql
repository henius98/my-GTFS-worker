SELECT
  service_id,
  monday,
  tuesday,
  wednesday,
  thursday,
  friday,
  saturday,
  sunday,
  start_date,
  end_date
FROM calendar
WHERE service_id IN (SELECT value FROM json_each(?1))
