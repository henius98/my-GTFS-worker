#!/usr/bin/env python3
"""Benchmark replay write amplification for the importer's UPSERT strategy.

SQLite JSON1 exercises the same SQL shape used by D1. This is a local CPU and
changed-row proxy; it does not measure Cloudflare latency or billed D1 usage.
"""

from __future__ import annotations

import argparse
import gc
import json
import sqlite3
import statistics
import time


REPLACE_SQL = (
    "INSERT OR REPLACE INTO stop_times (trip_id, stop_sequence, arrival_time) "
    "SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]'), "
    "json_extract(value, '$[2]') FROM json_each(?)"
)

CONDITIONAL_SQL = (
    "INSERT INTO stop_times (trip_id, stop_sequence, arrival_time) "
    "SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]'), "
    "json_extract(value, '$[2]') FROM json_each(?) WHERE TRUE "
    "ON CONFLICT DO UPDATE SET trip_id = excluded.trip_id, "
    "stop_sequence = excluded.stop_sequence, "
    "arrival_time = excluded.arrival_time "
    "WHERE stop_times.trip_id IS NOT excluded.trip_id "
    "OR stop_times.stop_sequence IS NOT excluded.stop_sequence "
    "OR stop_times.arrival_time IS NOT excluded.arrival_time"
)


def make_payload(row_count: int) -> str:
    return json.dumps(
        [
            [f"trip-{index // 40}", str(index % 40), f"{index % 24:02}:00:00"]
            for index in range(row_count)
        ],
        separators=(",", ":"),
    )


def benchmark_replay(
    sql: str, payload: str, iterations: int
) -> tuple[list[float], list[int], int]:
    connection = sqlite3.connect(":memory:")
    connection.execute("PRAGMA journal_mode=OFF")
    connection.execute("PRAGMA synchronous=OFF")
    connection.execute(
        "CREATE TABLE stop_times ("
        "trip_id TEXT, stop_sequence INTEGER, arrival_time TEXT, "
        "PRIMARY KEY (trip_id, stop_sequence))"
    )
    connection.execute(REPLACE_SQL, (payload,))

    for _ in range(3):
        connection.execute(sql, (payload,))
    connection.commit()

    samples = []
    changed_rows = []
    for _ in range(iterations):
        gc.collect()
        changes_before = connection.total_changes
        started = time.perf_counter_ns()
        connection.execute(sql, (payload,))
        connection.commit()
        samples.append((time.perf_counter_ns() - started) / 1_000_000)
        changed_rows.append(connection.total_changes - changes_before)

    stored_rows = connection.execute("SELECT COUNT(*) FROM stop_times").fetchone()[0]
    connection.close()
    return samples, changed_rows, stored_rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=50_000)
    parser.add_argument("--iterations", type=int, default=9)
    args = parser.parse_args()
    if args.rows <= 0 or args.iterations <= 0:
        parser.error("--rows and --iterations must be positive")

    payload = make_payload(args.rows)
    replace_samples, replace_changes, replace_rows = benchmark_replay(
        REPLACE_SQL, payload, args.iterations
    )
    conditional_samples, conditional_changes, conditional_rows = benchmark_replay(
        CONDITIONAL_SQL, payload, args.iterations
    )
    replace_median = statistics.median(replace_samples)
    conditional_median = statistics.median(conditional_samples)

    print(
        f"rows={args.rows} iterations={args.iterations}\n"
        f"replace_replay_median_ms={replace_median:.4f}\n"
        f"conditional_replay_median_ms={conditional_median:.4f}\n"
        f"replay_median_reduction_pct="
        f"{(1 - conditional_median / replace_median) * 100:.2f}\n"
        f"replace_changed_rows_per_replay={replace_changes}\n"
        f"conditional_changed_rows_per_replay={conditional_changes}"
    )

    correct = (
        replace_rows == args.rows
        and conditional_rows == args.rows
        and all(changes == args.rows for changes in replace_changes)
        and all(changes == 0 for changes in conditional_changes)
    )
    return int(not correct)


if __name__ == "__main__":
    raise SystemExit(main())
