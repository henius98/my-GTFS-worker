# my-GTFS-worker

A high-performance, Rust-based Cloudflare Worker designed to automatically fetch, decompress, and synchronize General Transit Feed Specification (GTFS) static datasets directly into a Cloudflare D1 Serverless Database.

It handles multiple Malaysian public transport operator datasets dynamically using the [Malaysia Open API](https://developer.data.gov.my/).

## Features

- ⚡ **Lightning Fast** — Written purely in Rust, compiled to WebAssembly (Wasm) for maximum execution speed and minimum memory footprint.
- 📦 **In-Memory ZIP Processing** — Downloads and streams ZIP datasets directly in-memory, extracting CSV files (`routes.txt`, `stops.txt`, etc.) without touching disk.
- 🚀 **Multi-Row Bulk Inserts** — Dynamically calculates optimal batch sizing based on D1's 100-parameter limit. For a 5-column table, this means 20 rows per query × 100 queries per batch = **2,000 rows per D1 batch call**.
- 🧠 **Dynamic Schema Discovery** — `schema.sql` is the source of truth for each operator. Columns are discovered at runtime via `PRAGMA table_info()` — the worker automatically adapts to heterogeneous structures across different datasets.
- 🔄 **Safe UPSERT Sync** — Uses `INSERT OR REPLACE INTO` instead of destructive `DELETE FROM`, safely merging new and updated records.
- 📖 **Dual Logging** — Custom logger routes to both the Cloudflare console and a persistent `logs` D1 table via async fire-and-forget writes.
- ⏱️ **Zero-Maintenance Scheduling** — Wrangler cron triggers (`0 */4 * * *`) fire every 4 hours automatically.

---

## Project Structure

```
my-GTFS-worker/
├── src/
│   ├── lib.rs          # Entry points (scheduled + fetch) & environment setup
│   ├── config.rs       # Constants, types (TableSchema, ColumnInfo), shared helpers
│   ├── schema.rs       # DB schema introspection via PRAGMA table_info
│   ├── import.rs       # Import orchestration: ZIP download + dataset iteration
│   ├── processor.rs    # CSV parsing, column resolution, SQL building, bulk INSERT
│   └── logger.rs       # Console + D1 dual logging
├── schema.sql          # D1 database schema (single source of truth)
├── wrangler.toml       # Cloudflare Worker configuration
├── Cargo.toml          # Rust dependencies
└── package.json        # Node.js scripts (dev, deploy)
```

### Module Responsibilities

| Module | Purpose |
|---|---|
| `lib.rs` | Worker entry points (`scheduled`, `fetch`) and `setup_env()` helper |
| `config.rs` | `GTFS_TABLES` mapping, D1 limits, `TableSchema` struct, `to_worker_err()` |
| `schema.rs` | `get_table_columns()` (PRAGMA), `prefetch_table_schemas()` (runs per operator prefix) |
| `import.rs` | `import_gtfs()` orchestrator, `fetch_zip_bytes()` HTTP helper |
| `processor.rs` | `process_csv_file()`, `build_multi_row_sql()`, column matching & diagnostics |
| `logger.rs` | `Logger` struct with `info()`, `warn()`, `error()` — logs to console + D1 |

### Data Flow

```
Cron / HTTP trigger
      │
      ▼
  lib.rs (entry point)
      │
      ▼
  import.rs 
      │
      │  for each GTFS dataset enum:
      │    ├── prefetch_table_schemas()   ──► schema.rs (PRAGMA × N tables, per operator)
      │    ├── fetch_zip_bytes()          ──► Download ZIP
      │    └── for each pre-fetched schema:
      │          └── process_csv_file()   ──► processor.rs
      │                ├── Extract CSV from ZIP
      │                ├── Match CSV headers ↔ DB columns
      │                ├── Build multi-row INSERT SQL
      │                └── Batch INSERT into D1
      ▼
  Done ✅
```

### Performance Optimizations

- **Per-operator schema discovery** — PRAGMA queries run once per operator to adapt to different static response structures (e.g., Fares V2 for Johor vs standard GTFS for KTMB).
- **Pre-built SQL templates** — The full multi-row INSERT SQL is built once during prefetch for each table. Reused directly when all DB columns match CSV headers (the common case).
- **Dynamic batch sizing** — `rows_per_insert = 100 ÷ column_count`, maximizing throughput within D1's 100 bound parameter limit.
- **D1 batching** — Up to 100 INSERT statements per `d1.batch()` call, sending thousands of rows in a single round-trip.
- **Vector pre-allocation** — `Vec::with_capacity()` avoids reallocations for parameter buffers and batch statement lists.

---

## Prerequisites

Ensure your local environment is correctly configured with:

1. **[Rust & Cargo](https://rustup.rs/)** (`rustup default stable`)
2. **[Node.js / npm](https://nodejs.org/en/)**
3. **Wrangler CLI** (`npm install -g wrangler`)
4. **Cloudflare Account**

---

## Setup

### 1. Install Dependencies

```bash
npm install
npm run build
```

### 2. Create the D1 Database

```bash
npx wrangler d1 create my-gtfs-db
```

Copy the returned `database_id` and update `wrangler.toml`:

```toml
[[d1_databases]]
binding = "DB"
database_name = "my-gtfs-db"
database_id = "YOUR_NEW_UUID_HERE"
```

### 3. Bootstrap the Schema

Execute `schema.sql` to create the GTFS tables and the `logs` table. The schema supports heterogeneous structures across operators (Core GTFS, Fares V2, Frequencies, etc.):

```bash
# Local development
npx wrangler d1 execute my-gtfs-db --file=./schema.sql --local

# Production
npx wrangler d1 execute my-gtfs-db --file=./schema.sql --remote
```

### 4. Configure GTFS Datasets

In `wrangler.toml`, modify the `GTFS_STATIC_ENUM` variable to control which transport operator datasets are imported:

```toml
[vars]
GTFS_STATIC_URL = "https://api.data.gov.my/gtfs-static/"
GTFS_STATIC_ENUM = """mybas-johor,
ktmb,
prasarana?category=rapid-bus-mrtfeeder,
prasarana?category=rapid-rail-kl,
prasarana?category=rapid-bus-kl,
prasarana?category=rapid-bus-penang"""
```

> Available dataset options are listed at the [Malaysia GTFS Static API docs](https://developer.data.gov.my/realtime-api/gtfs-static).

---

## Development

```bash
# Start local development server
npm run dev
```

While running, the worker binds a `fetch` endpoint alongside the cron trigger. Visit `http://localhost:8787/` or run:

```bash
curl http://localhost:8787/
```

This triggers an immediate import cycle for testing.

To test the scheduled trigger specifically:

```bash
curl http://localhost:8787/cdn-cgi/handler/scheduled
```

---

## Deployment

```bash
npm run deploy
```

Once deployed, the `[triggers]` cron configuration handles automatic execution. Monitor logs in real-time:

```bash
npx wrangler tail
```

---

## Reviewing Logs

The `Logger` module writes all output to the `logs` D1 table. Query recent logs:

```bash
npx wrangler d1 execute my-gtfs-db \
  --command="SELECT * FROM logs ORDER BY Timestamp DESC LIMIT 20" \
  --remote
```

---

## Adding a New GTFS Table

1. **Add the table definition** in `schema.sql`
2. **Register the mapping** in `config.rs` → `GTFS_TABLES`:
   ```rust
   ("new_file.txt", "new_table"),
   ```
3. **Re-run the schema** against D1
4. **Deploy** — the worker automatically discovers columns from the new table at runtime

No other code changes needed. The schema is the single source of truth.

---

## Dependencies

| Crate | Purpose |
|---|---|
| `worker` | Cloudflare Workers Rust SDK (with `d1` feature) |
| `serde` | Deserialize `PRAGMA table_info()` results |
| `zip` | Read ZIP archives in-memory |
| `csv` | Parse CSV files |
| `console_error_panic_hook` | Better panic messages in Wasm |
