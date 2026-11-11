-- Migration: Change calendar primary key from (service_id, start_date, end_date) to (service_id, start_date) and deduplicate records.
CREATE TABLE IF NOT EXISTS "calendar_new" (
  service_id TEXT,
  monday BOOLEAN,
  tuesday BOOLEAN,
  wednesday BOOLEAN,
  thursday BOOLEAN,
  friday BOOLEAN,
  saturday BOOLEAN,
  sunday BOOLEAN,
  start_date INTEGER,
  end_date INTEGER,
  PRIMARY KEY (service_id, start_date)
);

INSERT OR REPLACE INTO "calendar_new" (
  service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date
)
SELECT
  service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, MAX(end_date)
FROM "calendar"
GROUP BY service_id, start_date;

DROP TABLE "calendar";

ALTER TABLE "calendar_new" RENAME TO "calendar";
