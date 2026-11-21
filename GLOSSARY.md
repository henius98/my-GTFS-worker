# my-GTFS-worker Importer

Ingests static GTFS ZIP feeds from transit providers into per-provider Cloudflare D1 SQLite databases within bounded daily write and memory budgets.

## Language

**Provider**:
A transit agency whose static GTFS ZIP feed is ingested into a dedicated D1 database configured in `providers.toml`.
_Avoid_: Tenant, agency service, source

**Feed**:
A remote GTFS ZIP archive containing comma-separated `.txt` schedule tables identified by an HTTP ETag and per-file CRC32 checksums.
_Avoid_: Dataset archive, bundle, package

**SchemaCatalog**:
The compile-time mapping of each provider's SQL migrations to table columns, primary keys, staging table DDL, and positional upsert statements.
_Avoid_: Schema registry, metadata service, table mapper

**Checkpoint**:
The contiguous `(line, byte)` offset in an uncompressed GTFS `.txt` entry through which all records have been durably committed to D1.
_Avoid_: Cursor, offset marker, progress state

**DailyWriteLease**:
An atomic reservation of D1 row writes acquired from the `0_daily_import_budget` ledger table for a single workflow run on a UTC day and refunded with unused capacity on clean completion.
_Avoid_: Quota lock, daily token, write allowance

**WriteReservation**:
An in-process claim against the workflow's remaining data or metadata write budget that settles against actual D1 `rows_written` metadata or consumes its full allocation on ambiguous failure.
_Avoid_: Write permit, budget ticket

**StatementGroup**:
A bounded batch of positional JSON `INSERT` or `DELETE` SQL statements dispatched in a single D1 REST batch request under per-database concurrency limits.
_Avoid_: Query chunk, request bundle

**Wave**:
One round of fair logical-row allocation across all providers with pending CSV work, redistributing unused row capacity to remaining providers in subsequent rounds.
_Avoid_: Pass, epoch, iteration

**DatabaseRotation**:
The replacement of a provider's D1 database when its file size reaches the configured threshold, provisioning a fresh schema-migrated database and recording the retired database in `providers.toml`.
_Avoid_: Failover, database shard, rollover
