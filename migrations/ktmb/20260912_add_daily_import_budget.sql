CREATE TABLE IF NOT EXISTS daily_import_budget (Id INTEGER PRIMARY KEY CHECK (Id = 1), DAY TEXT NOT NULL, Reserved INTEGER NOT NULL CHECK (Reserved >= 0));

INSERT
OR IGNORE INTO daily_import_budget (Id, DAY, Reserved)
VALUES (1, '', 0);
