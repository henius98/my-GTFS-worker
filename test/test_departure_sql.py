"""Exercise the worker's cache SQL and migrations without compiling Rust."""

import json
from pathlib import Path
import sqlite3
import tomllib
import unittest


ROOT = Path(__file__).resolve().parent.parent
SQL = ROOT / "worker/src/routes/departures"
FEED_STATE = (SQL / "feed_state.sql").read_text()
CACHE_READ = (SQL / "cache_read.sql").read_text()
CACHE_WRITE = (SQL / "cache_write.sql").read_text().replace("__FEED_STATE__", FEED_STATE)
PROVIDERS = tomllib.loads((ROOT / "providers.toml").read_text())["providers"]


class DepartureCacheSQL(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        self.addCleanup(self.db.close)
        self.db.executescript((ROOT / "migrations/ktmb/0_gtfs_schema.sql").read_text())

    def revision(self):
        return self.db.execute(FEED_STATE).fetchone()[0]

    def write(self, payload, revision=None, expires=160, route="", direction=-1):
        self.db.execute(
            CACHE_WRITE,
            ("S", route, direction, revision or self.revision(), expires, json.dumps(payload)),
        )

    def read(self, now=100, route="", direction=-1):
        row = self.db.execute(CACHE_READ, ("S", route, direction, self.revision(), now)).fetchone()
        return json.loads(row[0]) if row else None

    def test_cache_expires_at_the_boundary_and_is_replaced(self):
        self.write({"departures": ["2026-09-24T08:00:00+08:00"]})
        self.assertIsNotNone(self.read(now=159))
        self.assertIsNone(self.read(now=160))
        self.write({"departures": ["2026-09-24T08:05:00+08:00"]}, expires=220)
        self.assertEqual(self.read(now=170)["departures"], ["2026-09-24T08:05:00+08:00"])
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM departure_cache").fetchone()[0], 1)

    def test_route_and_direction_filters_have_distinct_entries(self):
        self.write({"filter": "all"})
        self.write({"filter": "route"}, route="R")
        self.write({"filter": "outbound"}, route="R", direction=0)
        self.write({"filter": "inbound"}, route="R", direction=1)
        self.assertEqual(self.read(), {"filter": "all"})
        self.assertEqual(self.read(route="R"), {"filter": "route"})
        self.assertEqual(self.read(route="R", direction=0), {"filter": "outbound"})
        self.assertEqual(self.read(route="R", direction=1), {"filter": "inbound"})

    def test_import_changes_invalidate_reads_and_reject_racing_writes(self):
        self.db.execute(
            "INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status) "
            "VALUES ('ktmb', 'stop_times.txt', 'old', 10, 0)"
        )
        old_revision = self.revision()
        self.write({"generation": "old"})
        self.db.execute("UPDATE import_progress SET CRC = 'new', Status = 1")
        self.assertIsNone(self.read())
        self.assertEqual(self.db.execute(FEED_STATE).fetchone()[1], 1)
        self.db.execute("DELETE FROM departure_cache")
        self.write({"generation": "racing"}, revision=old_revision)
        self.write({"generation": "partial"})
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM departure_cache").fetchone()[0], 0)
        self.db.execute("UPDATE import_progress SET Status = 0, LastProcessedLine = 20")
        self.write({"generation": "complete"})
        self.assertEqual(self.read(), {"generation": "complete"})

    def test_older_request_cannot_replace_a_newer_entry(self):
        self.write({"request": "new"}, expires=200)
        self.write({"request": "old"}, expires=160)
        self.assertEqual(self.read(), {"request": "new"})

    def test_revision_changes_when_a_checkpoint_moves_without_a_new_crc(self):
        self.db.execute(
            "INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status) "
            "VALUES ('ktmb', 'stop_times.txt', 'same', 10, 0)"
        )
        self.write({"generation": "old checkpoint"})
        self.db.execute("UPDATE import_progress SET LastProcessedByte = 120")
        self.assertIsNone(self.read())

    def test_every_provider_migration_matches_its_base_schema(self):
        for provider in PROVIDERS:
            with self.subTest(provider=provider["name"]):
                folder = ROOT / "migrations" / provider["name"]
                schema = (folder / "0_gtfs_schema.sql").read_text()
                migrations = list(folder.glob("*_add_departure_cache.sql"))
                self.assertEqual(len(migrations), 1)
                fresh = sqlite3.connect(":memory:")
                upgraded = sqlite3.connect(":memory:")
                self.addCleanup(fresh.close)
                self.addCleanup(upgraded.close)
                fresh.executescript(schema)
                upgraded.executescript(schema.split("-- Calculated departures;")[0])
                upgraded.execute("INSERT INTO stops (stop_id, stop_name) VALUES ('S', 'Existing stop')")
                migration = migrations[0].read_text()
                upgraded.executescript(migration)
                upgraded.executescript(migration)
                self.assertEqual(
                    fresh.execute("PRAGMA table_info(departure_cache)").fetchall(),
                    upgraded.execute("PRAGMA table_info(departure_cache)").fetchall(),
                )
                self.assertEqual(upgraded.execute("SELECT stop_name FROM stops WHERE stop_id = 'S'").fetchone()[0], "Existing stop")
                revision = upgraded.execute(FEED_STATE).fetchone()[0]
                upgraded.execute(CACHE_WRITE, ("S", "", -1, revision, 160, '{"departures": []}'))
                self.assertEqual(upgraded.execute(CACHE_READ, ("S", "", -1, revision, 100)).fetchone()[0], '{"departures": []}')


if __name__ == "__main__":
    unittest.main()
