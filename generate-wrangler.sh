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
import sys, re

PROVIDERS_FILE = "providers.toml"
OUTPUT_FILE = "wrangler.toml"

try:
    with open(PROVIDERS_FILE, "r") as f:
        content = f.read()
except Exception as e:
    print(f"❌ Error reading {PROVIDERS_FILE}: {e}")
    sys.exit(1)

header = """# AUTO-GENERATED from providers.toml — do not edit directly.
# Regenerate with: ./generate-wrangler.sh

name = "my-gtfs-worker"
main = "worker/build/worker/shim.mjs"
compatibility_date = "2024-09-23"
compatibility_flags = ["nodejs_compat"]

[observability]
enabled = true

[build]
command = "bash ./build.sh"

# ─── Provider D1 Databases ──────────────────────────────────────────────────
# All providers share the same worker, but queries route to the specific D1 database.
#
# Deploy:     ./deploy.sh
# Logs:       wrangler tail
"""

blocks = content.split("[[providers]]")[1:]  # Skip everything before the first block
environments_count = 0

with open(OUTPUT_FILE, "w") as out:
    out.write(header)
    
    for block in blocks:
        def get_val(key):
            match = re.search(fr'{key}\s*=\s*"([^"]+)"', block)
            return match.group(1) if match else None

        name = get_val("name")
        if not name:
            continue
            
        db_id = get_val("database_id") or ""
        db_name = f"gtfs-{name}-db"
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
