"""Exercise the production ledger SQL locally; no Cloudflare requests."""

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import re
import sqlite3
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
SOURCE = (ROOT / "importer/src/d1.rs").read_text()
ACQUIRE, RELEASE = re.findall(r'sql: "((?:WITH allowance|UPDATE daily_import_budget)[^"]+)"', SOURCE)
LIMIT = int(re.search(r"DAILY_WRITE_LIMIT: u64 = ([\d_]+)", SOURCE)[1].replace("_", ""))
OVERHEAD = sum(
    int(re.search(rf"{name}: u64 = ([\d_]+)", SOURCE)[1].replace("_", ""))
    for name in ("METADATA_WRITE_RESERVE", "LEDGER_WRITE_RESERVE")
)
MIGRATION = (ROOT / "migrations/ktmb/20260912_add_daily_import_budget.sql").read_text()


class DailyBudgetTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = str(Path(self.directory.name) / "budget.db")
        self.day = "2026-09-12"
        self.db = self.connect()
        self.addCleanup(self.db.close)
        self.db.executescript(MIGRATION)

    def connect(self):
        db = sqlite3.connect(self.path, isolation_level=None)
        db.create_function("date", 1, lambda _: self.day)
        return db

    def acquire(self, db=None, amount=LIMIT // 2):
        return (db or self.db).execute(ACQUIRE, (amount, LIMIT, self.day, OVERHEAD)).fetchall()

    def reserved(self):
        return self.db.execute("SELECT Reserved FROM daily_import_budget WHERE Id = 1").fetchone()[0]

    def test_crashed_runs_keep_capacity_and_third_run_is_denied(self):
        self.assertTrue(self.acquire())
        self.assertTrue(self.acquire())
        self.assertFalse(self.acquire())
        self.assertEqual(self.reserved(), LIMIT)

    def test_returning_unused_capacity_preserves_consumed_writes(self):
        self.acquire()
        unused = LIMIT // 2 - 10_000
        self.db.execute(RELEASE, (unused, self.day, unused))
        self.assertEqual(self.reserved(), 10_000)
        self.assertTrue(self.acquire())
        self.assertEqual(self.acquire(), [(LIMIT // 2 - 10_000,)])
        self.assertFalse(self.acquire())
        self.assertEqual(self.reserved(), LIMIT)

    def test_full_allowance_and_later_run_use_remaining_capacity(self):
        self.assertEqual(LIMIT, 100_000)
        self.assertEqual(self.acquire(amount=LIMIT), [(LIMIT,)])
        self.assertFalse(self.acquire(amount=LIMIT))
        unused = LIMIT - 10_000
        self.db.execute(RELEASE, (unused, self.day, unused))
        self.assertEqual(self.acquire(amount=LIMIT), [(unused,)])
        self.assertEqual(self.reserved(), LIMIT)

    def test_remaining_capacity_must_cover_metadata_and_ledger(self):
        self.acquire(amount=LIMIT)
        self.db.execute(RELEASE, (OVERHEAD, self.day, OVERHEAD))
        self.assertFalse(self.acquire(amount=LIMIT))
        self.assertEqual(self.reserved(), LIMIT - OVERHEAD)

    def test_stale_day_cannot_acquire_a_new_lease(self):
        old_day = self.day
        self.day = "2026-09-13"
        self.assertEqual(self.db.execute(ACQUIRE, (LIMIT, LIMIT, old_day, OVERHEAD)).fetchall(), [])
        self.assertEqual(self.reserved(), 0)

    def test_next_day_resets_and_old_release_cannot_refund_new_day(self):
        self.acquire()
        old_day = self.day
        self.day = "2026-09-13"
        self.assertEqual(self.acquire(amount=LIMIT), [(LIMIT,)])
        self.db.execute(RELEASE, (30_000, old_day, 30_000))
        self.assertEqual(self.reserved(), LIMIT)

    def test_concurrent_processes_share_one_allowance(self):
        def run(_):
            db = self.connect()
            try:
                return self.acquire(db, amount=40_000)
            finally:
                db.close()

        with ThreadPoolExecutor(max_workers=4) as executor:
            grants = [row[0] for result in executor.map(run, range(4)) for row in result]
        self.assertEqual(sorted(grants), [20_000, 40_000, 40_000])
        self.assertEqual(self.reserved(), LIMIT)

    def test_reapplying_migration_preserves_ledger(self):
        self.acquire()
        self.db.executescript(MIGRATION)
        self.assertEqual(self.reserved(), LIMIT // 2)


if __name__ == "__main__":
    unittest.main()
