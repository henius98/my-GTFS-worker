# my-GTFS-worker Importer

Ingests static GTFS ZIP feeds from transit providers into per-provider Cloudflare D1 SQLite databases via staged tables and atomic swaps within bounded daily write and memory budgets.

## Language

**Provider**:
A transit agency whose static GTFS ZIP feed is ingested into a dedicated D1 database configured in `providers.toml`.
_Avoid_: Tenant, agency service, source

**Feed**:
A remote GTFS ZIP archive containing comma-separated `.txt` schedule tables identified by an HTTP ETag and per-file CRC32 checksums.
_Avoid_: Dataset archive, bundle, package

**SchemaCatalog**:
The compile-time mapping of each provider's `0_gtfs_schema.sql` and migration files to table columns, primary keys, staging table DDL, per-row staging write estimates, base schema SQL, and positional upsert statements.
_Avoid_: Schema registry, metadata service, table mapper

**StagingTable**:
A temporary per-table copy named `<table_name>__staging` created from the live table's compiled DDL to receive new or changed CSV rows before replacing the live table.
_Avoid_: Temp table, shadow table, buffer table

**AtomicSwap**:
The single D1 batch committed after all of a provider's files reach `Staged` (`Status = 2`) that drops old live tables, renames each `StagingTable` to its live name, marks files `Completed` (`Status = 0`), and saves the feed ETag in `0_dataset_versions`.
_Avoid_: Cutover, table flip, blue-green switch

**Checkpoint**:
The contiguous `(line, byte)` offset in an uncompressed GTFS `.txt` entry through which all records have been durably committed to its `StagingTable` in D1.
_Avoid_: Cursor, offset marker, progress state

**DailyWriteLease**:
An atomic reservation of D1 row writes acquired from the `0_daily_import_budget` ledger table for a single workflow run on a UTC day and refunded with provably unused capacity on completion or error.
_Avoid_: Quota lock, daily token, write allowance

**WriteReservation**:
An in-process claim against the workflow's remaining data or metadata write budget that settles against actual D1 `rows_written` metadata or consumes its full allocation on ambiguous failure.
_Avoid_: Write permit, budget ticket

**StatementGroup**:
A bounded batch of positional JSON `INSERT` upsert statements targeting a `StagingTable`, dispatched in a single D1 REST batch request under global and per-database concurrency limits.
_Avoid_: Query chunk, request bundle

**Wave**:
One round of fair logical-row allocation across all providers with pending CSV work (bounded by remaining workflow rows and daily data writes), redistributing unused row capacity to remaining providers in subsequent rounds.
_Avoid_: Pass, epoch, iteration

**DatabaseRotation**:
The replacement of a provider's D1 database when its file size reaches the configured threshold, provisioning a fresh database initialized with `0_gtfs_schema.sql` and recording the retired database under `[[retired_databases]]` in `providers.toml`.
_Avoid_: Failover, database shard, rollover
