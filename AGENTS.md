# Agent Configuration: my-GTFS-worker

This document serves as the single source of truth for architectural patterns and MCP tool integration guidelines for the `my-GTFS-worker` project, as mandated by the Universal Vibe Coding (UVC) standard.

## 1. System Architecture

The application is a Cargo Workspace containing three crates:

- **`worker/`**: Cloudflare Worker (Wasm). A unified worker that dynamically routes requests to per-provider D1 databases (`DB_<PROVIDER>`) across `/`, `/<provider>/status`, `/<provider>/data/<table>`, `/<provider>/sql`, and `/<provider>/map`.
- **`importer/`**: GitHub Actions standalone CLI. Connects directly to Cloudflare D1 via HTTP APIs to ingest ZIP/CSV GTFS files concurrently into `<table_name>__staging` tables under a shared daily write ledger (`0_daily_import_budget`), then commits atomic table swaps once all provider files finish staging.
- **`test/`** (`gtfs-checks`): Single crate housing all integration tests, migration/live-feed validators, and benchmarks, alongside out-of-line unit tests under `test/unit/`.

## 2. Eval-Driven Development (EDD) & Test Organization

- To verify logic, all Rust refactoring must pass an LLM-as-a-Judge code-verification stage before merging.
- Any manual compilation validation must be flagged to the human user for out-of-band execution.
- **Single Source of Truth for Tests (`test/`)**: All test code, test doubles, and test helpers MUST live under `test/` (`test/unit/{importer,worker,scripts}/` for out-of-line unit tests, `test/{importer,worker,shared}/` for integration suites, benchmarks, and Wrangler e2e tests). Application files in `importer/src/`, `worker/src/`, and `scripts/` may only contain a 3-line `#[cfg(test)] #[path = "..."] mod tests;` stub (or `pub(crate) mod tests;` when sharing mocks across modules). Never place `#[test]`, `#[tokio::test]`, inline `mod tests { ... }`, or `#[cfg(test)]` helper blocks in application files.

## 3. Multi-Tenant Deployment & Schemas

- Agents deploying or configuring databases must utilize the unified `./scripts/deploy.sh` orchestrator which provisions D1, creates empty migration folders (if missing), and triggers `scripts/generate-wrangler.sh`.
- `providers.toml` is the absolute source of truth.
- Do not edit `wrangler.toml` directly, it is auto-generated.
- **Schema Modifications**: Both `importer/build.rs` and `worker/build.rs` parse `migrations/<provider>/0_gtfs_schema.sql` at **compile time** via `scripts/schema_parser.rs`. To add a new table or column, agents MUST do both:
  1. Update `migrations/<provider>/0_gtfs_schema.sql` so the importer compiles the CSV columns, primary keys, staging table DDL, and base schema, and the worker validates `/data` and `/map` columns.
  2. Create a _new_ D1 migration file (`npx wrangler d1 migrations create DB_<PROVIDER> <name>`) to actually execute the `ALTER TABLE` or table recreation in production, as D1 ignores modifications to already-applied migrations.

## 4. Architectural Patterns & Concurrency

- **Compile-Time Schema Discovery**: Both `importer` and `worker` avoid runtime `PRAGMA table_info()` queries by generating static Rust maps via `build.rs` and the shared `scripts/schema_parser.rs`.
- **Decoupled Async/Blocking Work**: Never run heavy CPU tasks (e.g., ZIP decompression, CSV parsing, or massive JSON serialization) directly on the `tokio` async executor. Always use `tokio::task::spawn_blocking` coupled with an MPSC channel to stream data back to the async thread.
- **Global & Per-Database Concurrency Limits**: Rely on `Arc<Semaphore>` across providers and tasks to bound CPU/memory (CSV producers) and D1 batch traffic via `D1_CONCURRENCY_LIMIT` and `D1_DATABASE_CONCURRENCY_LIMIT` to prevent 429 rate limiting, paired with wave-based row/write budgeting.
