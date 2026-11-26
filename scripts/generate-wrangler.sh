#!/bin/bash
# generate-wrangler.sh — Generates wrangler.toml from providers.toml
set -euo pipefail

PROVIDERS_FILE="providers.toml"
OUTPUT_FILE="wrangler.toml"

# ── Pre-flight Checks ────────────────────────────────────────────────────────
if [ ! -f "$PROVIDERS_FILE" ]; then
    echo "❌ Error: $PROVIDERS_FILE not found in current directory."
    exit 1
fi

if ! command -v python3 &> /dev/null; then
    echo "❌ Error: 'python3' is required to securely parse $PROVIDERS_FILE."
    exit 1
fi

# ── Generate wrangler.toml using Python ──────────────────────────────────────
echo "→ Parsing $PROVIDERS_FILE and generating $OUTPUT_FILE..."

python3 - << 'EOF'
import sys

try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        print("❌ Error: Python 3.11+ or 'tomli' package is required.")
        sys.exit(1)

PROVIDERS_FILE = "providers.toml"
OUTPUT_FILE = "wrangler.toml"

try:
    with open(PROVIDERS_FILE, "rb") as f:
        providers_data = tomllib.load(f)
except Exception as e:
    print(f"❌ Error reading {PROVIDERS_FILE}: {e}")
    sys.exit(1)

header = """# AUTO-GENERATED from providers.toml — do not edit directly.
# Regenerate with: ./scripts/generate-wrangler.sh

name = "my-gtfs-worker"
main = "worker/build/index.js"
compatibility_date = "2026-09-08"

[placement]
mode = "smart"

[observability.logs]
enabled = true
head_sampling_rate = 1
invocation_logs = true
persist = true

[observability.traces]
enabled = false
head_sampling_rate = 1
persist = true

[build]
command = "bash ./scripts/build.sh"

[vars]
WORKER_CORS_ALLOW_ORIGIN = "*"                    # Worker SQL max rows; this is the maximum number of rows that can be returned in a single query to the worker.
WORKER_SQL_MAX_ROWS = "1000"                      # Short edge Cache-Control max-age lifetime in seconds for /status and /data (non-negative integer).
WORKER_API_CACHE_TTL_SECONDS = "60"               # Feed-level cache lifetime in seconds for /map edge caching and /departures D1 departure_cache (positive integer).
WORKER_FEED_CACHE_TTL_SECONDS = "86400"           # Max distance in seconds from `now` for /departures requests to populate departure_cache (0 disables writes).
WORKER_MAP_MAX_ROUTES = "20"                      # Maximum distinct route_id values accepted in one /map request (positive integer).
WORKER_MAP_CACHE_EXPIRE_TIME = "05:00"            # Daily local time (HH:MM in WORKER_DEFAULT_TIMEZONE) when /map edge cache expires.
WORKER_DEPARTURE_CACHE_WINDOW_SECONDS = "300"     # Maximum and default `limit` values for /departures (1 <= default <= max).
WORKER_DEPARTURES_MAX_LIMIT = "100"
WORKER_DEPARTURES_DEFAULT_LIMIT = "5"             # Forward search horizon in days for /departures (positive integer).
WORKER_DEPARTURES_SEARCH_DAYS = "7"               # Fallback IANA timezone when a GTFS feed omits agency_timezone.
WORKER_DEFAULT_TIMEZONE = "Asia/Kuala_Lumpur"

# ─── Provider D1 Databases ──────────────────────────────────────────────────
# All providers share the same worker, but queries route to the specific D1 database.
#
# Deploy:     ./scripts/deploy.sh
# Logs:       wrangler tail
"""

environments_count = 0

with open(OUTPUT_FILE, "w") as out:
    out.write(header)

    for provider in providers_data.get("providers", []):
        name = provider.get("name")
        if not name or provider.get("is_active") is False:
            continue

        db_id = provider.get("database_id", "")
        binding_name = f"DB_{name.upper().replace('-', '_')}"

        env_block = f"""
# ── {name} ─────────────────────────────────────────────────────────────
[[d1_databases]]
binding = "{binding_name}"
database_id = "{db_id}"
migrations_dir = "migrations/{name}"
"""
        out.write(env_block)
        environments_count += 1

print(f"✅ Generated {OUTPUT_FILE} from {PROVIDERS_FILE} ({environments_count} databases).")
EOF
