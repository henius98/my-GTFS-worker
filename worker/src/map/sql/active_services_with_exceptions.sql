  active_services AS (
    SELECT calendar.service_id
    FROM calendar
    WHERE calendar.__WEEKDAY__ = 1
      AND calendar.start_date <= __DATE__
      AND calendar.end_date >= __DATE__
      AND NOT EXISTS (
        SELECT 1
        FROM calendar_dates
        WHERE calendar_dates.service_id = calendar.service_id
          AND calendar_dates.date = __DATE__
          AND calendar_dates.exception_type = 2
      )
    UNION
    SELECT calendar_dates.service_id
    FROM calendar_dates
    WHERE calendar_dates.date = __DATE__
      AND calendar_dates.exception_type = 1
  )
