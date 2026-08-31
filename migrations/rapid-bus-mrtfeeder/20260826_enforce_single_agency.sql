-- Migration number: 0001 	 2026-08-26T11:47:42.866Z

-- Retain the most recently imported agency before enforcing snapshot cardinality.
DELETE FROM agency
WHERE rowid NOT IN (SELECT MAX(rowid) FROM agency);

CREATE UNIQUE INDEX IF NOT EXISTS uq_agency_single_row ON agency ((1));
