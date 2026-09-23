#!/usr/bin/env python3
"""Benchmark the D1-facing JSON representations used by the importer.

This exercises SQLite JSON1, the same JSON functions used by D1. It measures
payload bytes and local SQL execution only; it intentionally makes no claim
about Cloudflare network latency.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import statistics
import time


COLUMNS = [
    "route_id",
    "direction_id",
    "trip_id",
    "arrival_time",
    "departure_time",
    "stop_id",
    "stop_sequence",
]


def sample_rows(count: int) -> list[list[str]]:
    rows = []
    for index in range(count):
        hour = (index // 3600) % 30
        minute = (index // 60) % 60
        second = index % 60
        timestamp = f"{hour:02d}:{minute:02d}:{second:02d}"
        rows.append(
            [
                f"route-{index % 50}",
                str(index % 2),
                f"trip-{index // 40}",
                timestamp,
                timestamp,
                f"stop-{index % 2000}",
                str(index % 100),
            ]
        )
    return rows


def benchmark(
    connection: sqlite3.Connection,
    sql: str,
    payload: str,
    iterations: int,
) -> list[float]:
    for _ in range(10):
        connection.execute("DELETE FROM stop_times")
        connection.execute(sql, (payload,))

    samples = []
    for _ in range(iterations):
        connection.execute("DELETE FROM stop_times")
        started = time.perf_counter_ns()
        connection.execute(sql, (payload,))
        samples.append((time.perf_counter_ns() - started) / 1_000_000)
    return samples


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=500)
    parser.add_argument("--iterations", type=int, default=100)
    args = parser.parse_args()
    if args.rows <= 0 or args.iterations <= 0:
        parser.error("--rows and --iterations must be positive")

    rows = sample_rows(args.rows)
    object_payload = json.dumps(
        [dict(zip(COLUMNS, row, strict=True)) for row in rows],
        ensure_ascii=False,
        separators=(",", ":"),
    )
    positional_payload = json.dumps(
        rows,
        ensure_ascii=False,
        separators=(",", ":"),
    )
    quoted_columns = ", ".join(f'"{column}"' for column in COLUMNS)
    object_selects = ", ".join(
        f"json_extract(value, '$.{column}')" for column in COLUMNS
    )
    positional_selects = ", ".join(
        f"json_extract(value, '$[{index}]')" for index in range(len(COLUMNS))
    )
    assignments = ", ".join(
        f'"{column}" = excluded."{column}"' for column in COLUMNS
    )
    changed = " OR ".join(
        f'stop_times."{column}" IS NOT excluded."{column}"'
        for column in COLUMNS
    )
    upsert = f" ON CONFLICT DO UPDATE SET {assignments} WHERE {changed}"
    object_sql = (
        f"INSERT INTO stop_times ({quoted_columns}) "
        f"SELECT {object_selects} FROM json_each(?) WHERE TRUE{upsert}"
    )
    positional_sql = (
        f"INSERT INTO stop_times ({quoted_columns}) "
        f"SELECT {positional_selects} FROM json_each(?) WHERE TRUE{upsert}"
    )

    connection = sqlite3.connect(":memory:")
    connection.execute("PRAGMA journal_mode=OFF")
    connection.execute("PRAGMA synchronous=OFF")
    connection.execute(
        "CREATE TABLE stop_times ("
        "route_id TEXT, direction_id INTEGER, trip_id TEXT, arrival_time TEXT, "
        "departure_time TEXT, stop_id TEXT, stop_sequence INTEGER, "
        "PRIMARY KEY (trip_id, stop_sequence))"
    )

    connection.execute(object_sql, (object_payload,))
    object_result = connection.execute(
        f"SELECT {quoted_columns} FROM stop_times"
    ).fetchall()
    connection.execute("DELETE FROM stop_times")
    connection.execute(positional_sql, (positional_payload,))
    positional_result = connection.execute(
        f"SELECT {quoted_columns} FROM stop_times"
    ).fetchall()
    if object_result != positional_result:
        print("ERROR: positional payload changed inserted values")
        return 1

    object_samples = benchmark(
        connection,
        object_sql,
        object_payload,
        args.iterations,
    )
    positional_samples = benchmark(
        connection,
        positional_sql,
        positional_payload,
        args.iterations,
    )
    connection.close()

    object_bytes = len(object_payload.encode())
    positional_bytes = len(positional_payload.encode())
    object_median = statistics.median(object_samples)
    positional_median = statistics.median(positional_samples)
    print(
        f"rows={args.rows} columns={len(COLUMNS)} iterations={args.iterations}\n"
        f"object_payload_bytes={object_bytes}\n"
        f"positional_payload_bytes={positional_bytes}\n"
        f"payload_reduction_pct={(1 - positional_bytes / object_bytes) * 100:.2f}\n"
        f"object_sql_median_ms={object_median:.4f}\n"
        f"positional_sql_median_ms={positional_median:.4f}\n"
        f"sql_median_change_pct={(positional_median / object_median - 1) * 100:.2f}"
    )
    return int(positional_bytes >= object_bytes)


if __name__ == "__main__":
    raise SystemExit(main())
