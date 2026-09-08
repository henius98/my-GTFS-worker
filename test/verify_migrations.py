#!/usr/bin/env python3
"""Validate live GTFS headers against every active provider's compile-time schema."""

from __future__ import annotations

import argparse
import csv
import io
from pathlib import Path
import re
import sqlite3
import sys
import tomllib
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parent.parent
INFRASTRUCTURE_TABLES = {
    "logs",
    "dataset_versions",
    "import_progress",
}


def strip_sql_comments(sql: str) -> str:
    sql = re.sub(r"/\*.*?\*/", "", sql, flags=re.DOTALL)
    return re.sub(r"--.*", "", sql)


def split_top_level_commas(value: str) -> list[str]:
    fields: list[str] = []
    start = 0
    depth = 0
    quote: str | None = None
    for index, character in enumerate(value):
        if quote is not None:
            if character == quote:
                quote = None
            continue
        if character in {"'", '"', "`"}:
            quote = character
        elif character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
        elif character == "," and depth == 0:
            fields.append(value[start:index])
            start = index + 1
    fields.append(value[start:])
    return fields


def parse_schema(path: Path) -> dict[str, list[str]]:
    sql = strip_sql_comments(path.read_text(encoding="utf-8"))
    table_pattern = re.compile(
        r'CREATE\s+TABLE\s+IF\s+NOT\s+EXISTS\s+["`]?([\w-]+)["`]?\s*\((.*?)\)\s*;',
        re.IGNORECASE | re.DOTALL,
    )
    tables: dict[str, list[str]] = {}
    for match in table_pattern.finditer(sql):
        table_name = match.group(1)
        if table_name in INFRASTRUCTURE_TABLES:
            continue
        columns: list[str] = []
        for definition in split_top_level_commas(match.group(2)):
            definition = definition.strip()
            if not definition:
                continue
            first_token = definition.split(maxsplit=1)[0].strip('"`[]')
            if first_token.lower() in {
                "primary",
                "foreign",
                "unique",
                "check",
                "constraint",
            }:
                continue
            columns.append(first_token)
        tables[table_name] = columns
    return tables


def active_providers() -> list[dict[str, object]]:
    with (ROOT / "providers.toml").open("rb") as providers_file:
        document = tomllib.load(providers_file)
    return [
        provider
        for provider in document.get("providers", [])
        if provider.get("is_active", False)
    ]


def normalized_header(archive: zipfile.ZipFile, filename: str) -> list[str]:
    with archive.open(filename) as csv_file:
        header_line = csv_file.readline().decode("utf-8-sig")
    if not header_line.strip():
        return []
    return [column.strip() for column in next(csv.reader([header_line]))]


def migration_sort_key(path: Path) -> tuple[int, str]:
    prefix = path.name.split("_", 1)[0]
    sequence = int(prefix) if prefix.isdigit() else sys.maxsize
    return sequence, path.name


def validate_replay_cleanup_migrations() -> tuple[str, int]:
    connection = sqlite3.connect(":memory:")
    try:
        connection.executescript(
            """
            CREATE TABLE fare_leg_rules (
                leg_group_id TEXT,
                from_area_id TEXT,
                to_area_id TEXT,
                fare_product_id TEXT
            );
            CREATE TABLE agency (
                agency_name TEXT,
                agency_url TEXT,
                agency_timezone TEXT,
                agency_phone TEXT,
                agency_lang TEXT
            );
            """
        )
        fare_row = ("leg", "from", "to", "product")
        connection.execute(
            "INSERT INTO fare_leg_rules VALUES (?, ?, ?, ?)",
            fare_row,
        )
        connection.executemany(
            "INSERT INTO agency VALUES (?, ?, ?, ?, ?)",
            [
                ("old", "url", "zone", "phone", "en"),
                ("new", "url", "zone", "phone", "en"),
            ],
        )
        connection.executescript(
            (
                ROOT
                / "migrations"
                / "mybas-johor"
                / "20260826_deduplicate_fare_leg_rules.sql"
            ).read_text(encoding="utf-8")
        )
        connection.executescript(
            (
                ROOT
                / "migrations"
                / "rapid-bus-mrtfeeder"
                / "20260826_enforce_single_agency.sql"
            ).read_text(encoding="utf-8")
        )
        connection.execute(
            "INSERT OR REPLACE INTO fare_leg_rules VALUES (?, ?, ?, ?)", fare_row
        )
        connection.execute(
            "INSERT OR REPLACE INTO agency VALUES (?, ?, ?, ?, ?)",
            ("latest", "url", "zone", "phone", "en"),
        )
        fare_count = connection.execute(
            "SELECT COUNT(*) FROM fare_leg_rules"
        ).fetchone()[0]
        agency_rows = connection.execute("SELECT agency_name FROM agency").fetchall()
        if fare_count != 1 or agency_rows != [("latest",)]:
            return "- ❌ Replay cleanup migrations did not enforce idempotency.", 1
    except (OSError, sqlite3.Error) as error:
        return f"- ❌ Replay cleanup migration validation failed: {error}", 1
    finally:
        connection.close()
    return (
        "- ✅ Replay cleanup migrations enforce future idempotency.",
        0,
    )


def validate_legacy_progress_upgrade(migration_directory: Path) -> str | None:
    migrations = list(migration_directory.glob("*_add_import_progress_byte.sql"))
    if len(migrations) != 1:
        return f"expected exactly one progress-byte migration, found {len(migrations)}"

    connection = sqlite3.connect(":memory:")
    try:
        connection.executescript(
            """
            CREATE TABLE import_progress (
                Provider TEXT,
                FileName TEXT,
                CRC TEXT,
                LastProcessedLine INTEGER,
                Status TINYINT CHECK (Status IN (0, 1)),
                UpdatedAt DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (Provider, FileName)
            );
            CREATE TABLE import_progress_offsets (
                Provider TEXT,
                FileName TEXT,
                CRC TEXT,
                LastProcessedLine INTEGER,
                LastProcessedByte INTEGER,
                PRIMARY KEY (Provider, FileName)
            );
            INSERT INTO import_progress (
                Provider, FileName, CRC, LastProcessedLine, Status, UpdatedAt
            ) VALUES (
                'legacy-provider', 'stops.txt', 'legacy-crc', 17, 1,
                '2026-08-30 00:00:00'
            );
            """
        )
        connection.executescript(migrations[0].read_text(encoding="utf-8"))
        row = connection.execute(
            "SELECT Provider, FileName, CRC, LastProcessedLine, "
            "LastProcessedByte, Status, UpdatedAt FROM import_progress"
        ).fetchone()
        expected = (
            "legacy-provider",
            "stops.txt",
            "legacy-crc",
            17,
            0,
            1,
            "2026-08-30 00:00:00",
        )
        if row != expected:
            return f"legacy progress row changed during migration: {row}"
        leftover_tables = connection.execute(
            "SELECT COUNT(*) FROM sqlite_schema "
            "WHERE type = 'table' AND name IN "
            "('import_progress_legacy', 'import_progress_offsets')"
        ).fetchone()[0]
        if leftover_tables != 0:
            return "legacy or sidecar progress table remained after migration"
    except (OSError, sqlite3.Error) as error:
        return f"legacy progress migration failed: {error}"
    finally:
        connection.close()
    return None


def validate_local_migrations(
    migration_directory: Path,
    compile_time_tables: dict[str, list[str]],
) -> tuple[str, int]:
    if progress_error := validate_legacy_progress_upgrade(migration_directory):
        return f"- ❌ {progress_error}", 1

    connection = sqlite3.connect(":memory:")
    try:
        for migration in sorted(
            migration_directory.glob("*.sql"), key=migration_sort_key
        ):
            connection.executescript(migration.read_text(encoding="utf-8"))
        database_tables = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_schema WHERE type = 'table' "
                "AND name NOT LIKE 'sqlite_%'"
            )
        } - INFRASTRUCTURE_TABLES
        progress_columns = [
            row[1] for row in connection.execute('PRAGMA table_info("import_progress")')
        ]
        expected_progress_columns = [
            "Provider",
            "FileName",
            "CRC",
            "LastProcessedLine",
            "LastProcessedByte",
            "Status",
            "UpdatedAt",
        ]
        if progress_columns != expected_progress_columns:
            return (
                "- ❌ Runtime progress schema differs from the expected "
                f"single-table schema: {progress_columns}",
                1,
            )
        connection.execute(
            "INSERT INTO import_progress "
            "(Provider, FileName, CRC, LastProcessedLine, Status) "
            "VALUES ('checkpoint-probe', 'stops.txt', 'current-crc', 7, 1)"
        )
        checkpoint = connection.execute(
            "SELECT LastProcessedLine, LastProcessedByte FROM import_progress "
            "WHERE Provider = 'checkpoint-probe'"
        ).fetchone()
        if checkpoint != (7, 0):
            return (
                f"- ❌ Byte checkpoint default returned {checkpoint} instead of (7, 0).",
                1,
            )
        connection.execute(
            "UPDATE import_progress SET LastProcessedLine = 8, "
            "LastProcessedByte = 321 "
            "WHERE Provider = 'checkpoint-probe'"
        )
        updated_checkpoint = connection.execute(
            "SELECT LastProcessedLine, LastProcessedByte FROM import_progress "
            "WHERE Provider = 'checkpoint-probe'"
        ).fetchone()
        if updated_checkpoint != (8, 321):
            return (
                "- ❌ Single-table byte checkpoint update returned "
                f"{updated_checkpoint} instead of (8, 321).",
                1,
            )
        connection.execute(
            "DELETE FROM import_progress WHERE Provider = 'checkpoint-probe'"
        )
        if database_tables != set(compile_time_tables):
            return (
                "- ❌ Runtime migration tables differ from the compile-time schema: "
                f"runtime_only={sorted(database_tables - set(compile_time_tables))}, "
                f"compile_time_only={sorted(set(compile_time_tables) - database_tables)}",
                1,
            )
        for table_name, expected_columns in compile_time_tables.items():
            quoted_table = table_name.replace('"', '""')
            table_info = list(
                connection.execute(f'PRAGMA table_info("{quoted_table}")')
            )
            actual_columns = [row[1] for row in table_info]
            if actual_columns != expected_columns:
                return (
                    f"- ❌ Runtime columns for `{table_name}` differ from the "
                    f"compile-time schema: runtime={actual_columns}, "
                    f"compile_time={expected_columns}",
                    1,
                )
            has_primary_key = any(row[5] for row in table_info)
            indexes = list(connection.execute(f'PRAGMA index_list("{quoted_table}")'))
            has_unique_index = any(row[2] for row in indexes)
            if not has_primary_key and not has_unique_index:
                return (
                    f"- ❌ `{table_name}` has no uniqueness constraint; "
                    "checkpoint replay would append duplicate rows.",
                    1,
                )
            if len(indexes) > 1:
                return (
                    f"- ❌ `{table_name}` has {len(indexes)} indexes; the importer "
                    "write-budget reservation assumes at most one index write per row.",
                    1,
                )
            quoted_columns = ", ".join(
                f'"{column.replace(chr(34), chr(34) * 2)}"' for column in actual_columns
            )
            placeholders = ", ".join("?" for _ in actual_columns)
            assignments = ", ".join(
                f'"{column.replace(chr(34), chr(34) * 2)}" = '
                f'excluded."{column.replace(chr(34), chr(34) * 2)}"'
                for column in actual_columns
            )
            changed = " OR ".join(
                f'"{quoted_table}"."{column.replace(chr(34), chr(34) * 2)}" '
                f'IS NOT excluded."{column.replace(chr(34), chr(34) * 2)}"'
                for column in actual_columns
            )
            probe = tuple(
                f"replay-probe-{index}" for index in range(len(actual_columns))
            )
            replay_sql = (
                f'INSERT INTO "{quoted_table}" ({quoted_columns}) '
                f"VALUES ({placeholders}) ON CONFLICT DO UPDATE SET "
                f"{assignments} WHERE {changed}"
            )
            connection.execute(replay_sql, probe)
            changes_before_replay = connection.total_changes
            connection.execute(replay_sql, probe)
            replay_writes = connection.total_changes - changes_before_replay
            replay_count = connection.execute(
                f'SELECT COUNT(*) FROM "{quoted_table}"'
            ).fetchone()[0]
            connection.execute(f'DELETE FROM "{quoted_table}"')
            if replay_count != 1 or replay_writes != 0:
                return (
                    f"- ❌ `{table_name}` retained {replay_count} rows after "
                    "replaying an identical UPSERT and reported "
                    f"{replay_writes} unnecessary writes.",
                    1,
                )
    except sqlite3.Error as error:
        return f"- ❌ Local migration chain failed: {error}", 1
    finally:
        connection.close()
    return (
        "- ✅ Local migration chain matches the compile-time schema and "
        "uses one progress table; identical UPSERT replay is idempotent with "
        "zero changed rows and every table satisfies the two-write reservation "
        "bound.",
        0,
    )


def validate_provider(provider: dict[str, object]) -> tuple[list[str], int]:
    name = str(provider["name"])
    url = f"{provider['static_url']}{provider['static_provider']}"
    migration_path = ROOT / "migrations" / name / "0_gtfs_schema.sql"
    lines = [f"\n## Provider: `{name}`"]
    if not migration_path.is_file():
        return [*lines, f"- ❌ Migration file not found: `{migration_path}`"], 1

    tables = parse_schema(migration_path)
    local_result, local_errors = validate_local_migrations(
        migration_path.parent,
        tables,
    )
    lines.append(local_result)
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "my-GTFS-worker-schema-validator/1.0"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = response.read()
    except Exception as error:  # noqa: BLE001 - network failures must enter the report.
        return [*lines, f"- ❌ Failed to download feed: {error}"], local_errors + 1

    errors = local_errors
    matched_tables: set[str] = set()
    with zipfile.ZipFile(io.BytesIO(payload)) as archive:
        filenames = sorted(
            filename
            for filename in archive.namelist()
            if filename.endswith(".txt")
            and "__MACOSX" not in filename
            and not filename.rsplit("/", 1)[-1].startswith("._")
        )
        for filename in filenames:
            table_name = filename.rsplit("/", 1)[-1][:-4]
            if table_name not in tables:
                lines.append(
                    f"- ❌ Table `{table_name}` from the feed is absent from the migration."
                )
                errors += 1
                continue

            matched_tables.add(table_name)
            csv_columns = normalized_header(archive, filename)
            db_columns = tables[table_name]
            if db_columns == csv_columns:
                lines.append(f"- ✅ `{table_name}` matches exactly.")
                continue

            missing = sorted(set(csv_columns) - set(db_columns))
            extra = sorted(set(db_columns) - set(csv_columns))
            if not missing and not extra:
                lines.append(
                    f"- ❌ `{table_name}` has a column-order mismatch: "
                    f"migration={db_columns}, feed={csv_columns}"
                )
            else:
                lines.append(
                    f"- ❌ `{table_name}` differs: "
                    f"missing_in_db={missing}, extra_in_db={extra}"
                )
            errors += 1

    for table_name in sorted(set(tables) - matched_tables):
        lines.append(
            f"- ⚠️ Table `{table_name}` exists in the migration but not in the feed."
        )
    return lines, errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--report",
        type=Path,
        default=ROOT / "test" / "migrations_validation_report.md",
        help="Markdown report destination",
    )
    parser.add_argument(
        "--no-report",
        action="store_true",
        help="Validate without writing a report file",
    )
    args = parser.parse_args()

    replay_result, error_count = validate_replay_cleanup_migrations()
    report_lines = [
        "# Migrations Validation Report",
        "\n## Replay-safety migrations",
        replay_result,
    ]
    for provider in active_providers():
        provider_lines, provider_errors = validate_provider(provider)
        report_lines.extend(provider_lines)
        error_count += provider_errors
    report = "\n".join(report_lines) + "\n"

    if not args.no_report:
        args.report.write_text(report, encoding="utf-8")
    sys.stdout.write(report)
    if error_count:
        print(f"Validation failed with {error_count} schema error(s).", file=sys.stderr)
        return 1
    print("All active provider schemas match their live GTFS feeds.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
