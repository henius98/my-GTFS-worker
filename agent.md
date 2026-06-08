# Agent Configuration: my-GTFS-worker

This document serves as the single source of truth for architectural patterns and MCP tool integration guidelines for the `my-GTFS-worker` project, as mandated by the Universal Vibe Coding (UVC) standard.

## 1. System Architecture
The application is a Cargo Workspace containing two distinct environments:
- **`worker/`**: Cloudflare Worker (Wasm). A unified worker that dynamically routes to specific databases via the `/<provider>/status` API endpoints.
- **`importer/`**: GitHub Actions standalone CLI. Connects directly to Cloudflare D1 via HTTP APIs to ingest ZIP/CSV GTFS files concurrently.

## 2. MCP Guidelines
All subsequent Agentic tasks must use specific Model Context Protocol (MCP) servers:
- **Local File System MCP**: For writing and evaluating Rust code (using strict `cargo check` and `clippy` checks).
- **Chrome DevTools MCP**: For debugging Worker response metrics (if a development server is active on `localhost:8787`).
- **Linter Database MCP**: Ensure strict formatting rules are preserved across both crates.

## 3. Eval-Driven Development (EDD)
- Direct bash testing is restricted due to strict global rules ("Never run terminal commands for compiling...").
- To verify logic, all Rust refactoring must pass an LLM-as-a-Judge code-verification stage before merging.
- Any manual compilation validation must be flagged to the human user for out-of-band execution.

## 4. Multi-Tenant Deployment & Schemas
- Agents deploying or configuring databases must utilize the unified `./deploy.sh` orchestrator which provisions D1, creates empty migration folders (if missing), and triggers `generate-wrangler.sh`. 
- `providers.toml` is the absolute source of truth.
- Do not edit `wrangler.toml` directly, it is auto-generated.
- **Schema Modifications**: The `importer/build.rs` script parses `0_gtfs_schema.sql` at **compile time**. To add a new table or column, agents MUST do both:
  1. Update `0_gtfs_schema.sql` so the Rust compiler statically maps the new CSV columns.
  2. Create a *new* D1 migration file (`npx wrangler d1 migrations create DB_<PROVIDER> <name>`) to actually execute the `ALTER TABLE` in production, as D1 ignores modifications to already-applied migrations.

## 5. Architectural Patterns & Concurrency
- **Compile-Time Schema Discovery**: The importer avoids runtime `PRAGMA table_info()` queries by generating static Rust arrays via `build.rs` to map CSV headers to database columns safely.
- **Decoupled Async/Blocking Work**: Never run heavy CPU tasks (e.g., ZIP decompression, CSV parsing, or massive JSON serialization) directly on the `tokio` async executor. Always use `tokio::task::spawn_blocking` coupled with an MPSC channel to stream data back to the async thread.
- **Global Concurrency Limits**: Rely on `Arc<Semaphore>` across providers and tasks to bound memory (e.g., parsing large ZIP files). D1 batch queries are similarly bounded via config limits (`D1_CONCURRENCY_LIMIT`) to prevent 429 rate limiting.
