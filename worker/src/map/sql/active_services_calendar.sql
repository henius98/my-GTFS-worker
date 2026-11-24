  active_services AS (
    SELECT service_id
    FROM calendar
    WHERE __WEEKDAY__ = 1
      AND start_date <= __DATE__
      AND end_date >= __DATE__
  )
