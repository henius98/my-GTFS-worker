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
import sys, os
from datetime import datetime

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

old_mappings = {}
if os.path.exists(OUTPUT_FILE):
    try:
        with open(OUTPUT_FILE, "rb") as f:
            old_data = tomllib.load(f)
            for db in old_data.get("d1_databases", []):
                if "database_id" in db and "database_name" in db:
                    old_mappings[db["database_id"]] = db["database_name"]
    except Exception as e:
        print(f"⚠️ Warning: Could not read existing {OUTPUT_FILE}: {e}")

try:
    with open(PROVIDERS_FILE, "rb") as f:
        providers_data = tomllib.load(f)
except Exception as e:
    print(f"❌ Error reading {PROVIDERS_FILE}: {e}")
    sys.exit(1)

header = """# AUTO-GENERATED from providers.toml — do not edit directly.
# Regenerate with: ./scripts/generate-wrangler.sh

name = "my-gtfs-worker"
main = "worker/build/worker/shim.mjs"
compatibility_date = "2024-09-23"

[observability]
enabled = true

[build]
command = "bash ./scripts/build.sh"

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
        
        if db_id and db_id in old_mappings:
            db_name = old_mappings[db_id]
        else:
            today = datetime.utcnow().strftime('%Y%m%d')
            db_name = f"gtfs-{name}-db-{today}"
            
        binding_name = f"DB_{name.upper().replace('-', '_')}"

        env_block = f"""
# ── {name} ─────────────────────────────────────────────────────────────
[[d1_databases]]
binding = "{binding_name}"
database_name = "{db_name}"
database_id = "{db_id}"
migrations_dir = "migrations/{name}"
"""
        out.write(env_block)
        environments_count += 1

print(f"✅ Generated {OUTPUT_FILE} from {PROVIDERS_FILE} ({environments_count} databases).")
EOF
