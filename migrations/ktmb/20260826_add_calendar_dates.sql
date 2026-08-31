-- Migration number: 0001 	 2026-08-25T16:11:54.877Z

CREATE TABLE IF NOT EXISTS calendar_dates (
    service_id TEXT,
    date INTEGER,
    exception_type INTEGER,
    PRIMARY KEY (service_id, date)
);
