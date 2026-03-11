# my-GTFS-worker

A high-performance, Rust-based Cloudflare Worker designed to automatically fetch, uncompress, and synchronize General Transit Feed Specification (GTFS) static datasets directly into a Cloudflare D1 Serverless Database.

It handles multiple Malaysian public transport operator datasets dynamically using the [Malaysia Open API](https://developer.data.gov.my/).

## Features

- ⚡ **Lightning Fast:** Written purely in Rust and compiled to WebAssembly (Wasm) for maximum execution speed and minimum memory footprint.
- 📦 **In-Memory ZIP Processing:** Downloads and streams ZIP datasets directly in-memory, extracting internal CSV targets (like `routes.txt`, `stops.txt`) seamlessly without touching a local disk.
- 🔄 **Batched UPSERT Synchronization:** Instead of dangerous `DELETE FROM` statements, it handles data safely via `INSERT OR REPLACE INTO` using 50-item batches, honoring Cloudflare D1 query limits while efficiently merging existing real-world transit data.
- 📖 **Async Fire-and-Forget DB Logging:** Features a custom logger module that routes dual-level logging to both the real-time Cloudflare console _and_ a permanent `/ExecutionLogs` table within the persistent D1 SQLite instance seamlessly in the background!
- ⏱️ **Zero-Maintenance Scheduling:** Relies on Wrangler cron triggers (`0 */4 * * *`) to automatically fire every 4 hours ensuring your data is fresh.

---

## Prerequisites

Ensure your local environment is correctly configured with:

1. **[Rust & Cargo](https://rustup.rs/)** (`rustup default stable`)
2. **[Node.js / npm](https://nodejs.org/en/)**
3. **Wrangler CLI** (`npm install -g wrangler`)
4. **Cloudflare Account**

---

## Initialization & Setup

### 1. Install Dependencies

Clone the respiratory and run the initialization commands:

```bash
# Install Worker WebAssembly bridge tooling and wrangler setup
npm install
cargo update
```

### 2. Configure Your Cloudflare D1 Database

Create the serverless SQLite database in your Cloudflare account to store the GTFS tables:

```bash
npx wrangler d1 create my-gtfs-db
```

_Note: This command will return a success prompt containing your unique `database_id`. Copy this!_

Update `wrangler.toml` modifying the `database_id` binding on line 13 to match the output:

```toml
[[d1_databases]]
binding = "DB"
database_name = "my-gtfs-db"
database_id = "YOUR_NEW_UUID_HERE"
```

### 3. Bootstrap the Database Schema

Execute the predefined `schema.sql` file against your new D1 environment to build the GTFS tables (`routes`, `trips`, `stops`, `stop_times`, `calendar`, `shapes`, and `ExecutionLogs`):

```bash
# Initialize locally for testing
npx wrangler d1 execute my-gtfs-db --file=./schema.sql --local

# Initialize the production database remotely
npx wrangler d1 execute my-gtfs-db --file=./schema.sql --remote
```

### 4. Setting Supported Transport Environments (Enum Array)

In `wrangler.toml`, the static feed behavior is controlled by the `GTFS_STATIC_ENUM` string block. Modify this array to include whichever regions you want the worker to iterate over daily.

```toml
[vars]
GTFS_STATIC_URL = "https://api.data.gov.my/gtfs-static/"
GTFS_STATIC_ENUM = """ktmb,
prasarana?category=rapid-bus-penang,
prasarana?category=rapid-bus-mrtfeeder,
prasarana?category=rapid-rail-kl,
prasarana?category=rapid-bus-kl,
mybas-johor,
mybas-ipoh"""
```

---

## Running & Testing Locally

You can dynamically test the worker's extraction parameters and DB insertion logic on your local machine before pushing it live:

```bash
# Starts a local simulated Cloudflare environment
npm run dev
```

While the environment is running locally, it binds a `fetch` endpoint natively alongside the `cron` trigger so you can test execution manually! Simply visit or `curl` the provided `http` address (typically `http://localhost:8787/`) to force an immediate database migration test.

---

## Deployment

Once everything is testing cleanly and your schemas are live, push your worker securely to the Cloudflare Edge network:

```bash
npm run deploy
```

Once deployed, the `[triggers]` configuration handles the rest! You can actively watch your dataset migrations output logs remotely using:

```bash
npx wrangler tail
```

## Reviewing Logs

Since Cloudflare Workers cannot write static local files, the `Logger` system pipes background output perfectly directly into the D1 `ExecutionLogs` table!
To review how well a worker completed its daily cron job without using the dashboard, just execute:

```bash
npx wrangler d1 execute my-gtfs-db --command="SELECT * FROM ExecutionLogs ORDER BY Timestamp DESC LIMIT 20" --remote
```
