#!/bin/bash
# recreate-db.sh — Recreates a D1 database with a versioned name when it exceeds the 500MB limit.
#
# Usage: ./recreate-db.sh [recreate-providers.txt]
#
# Reads provider names from the file (one per line), and for each:
#   1. Determines the next version suffix (e.g., gtfs-mybas-johor-db -> gtfs-mybas-johor-db-v1)
#   2. Creates a new D1 database with the versioned name
#   3. Updates providers.toml with the new database_id and database_name
#   4. Regenerates wrangler.toml
#   5. Applies D1 schema migrations
#   6. Redeploys the worker
#
# The old database is left untouched as an archive.
set -euo pipefail

PROVIDERS_FILE="providers.toml"
RECREATE_FILE="${1:-recreate-providers.txt}"

# ── Pre-flight Checks ────────────────────────────────────────────────────────
if [ -f ".env" ]; then
    set -a
    source .env
    set +a
fi

if ! command -v wrangler &> /dev/null; then
    echo "❌ Error: 'wrangler' CLI is not installed."
    echo "Please install it via: npm install -g wrangler"
    exit 1
fi

if ! command -v python3 &> /dev/null; then
    echo "❌ Error: 'python3' is required to parse providers.toml."
    exit 1
fi

if [ ! -f "$RECREATE_FILE" ]; then
    echo "❌ Error: $RECREATE_FILE not found."
    exit 1
fi

echo "🔄 Starting D1 database recreation..."
echo ""

# ── Process each provider ────────────────────────────────────────────────────
while IFS= read -r PROVIDER || [ -n "$PROVIDER" ]; do
    # Skip empty lines
    [ -z "$PROVIDER" ] && continue

    echo "═══════════════════════════════════════════════════════════════"
    echo "  Processing provider: ${PROVIDER}"
    echo "═══════════════════════════════════════════════════════════════"

    # Determine the current database_name from providers.toml (if set)
    CURRENT_DB_NAME=$(python3 -c "
import sys, re
with open('$PROVIDERS_FILE', 'r') as f:
    blocks = f.read().split('[[providers]]')
for block in blocks:
    name_match = re.search(r'name\s*=\s*\"([^\"]+)\"', block)
    if name_match and name_match.group(1) == '$PROVIDER':
        db_name_match = re.search(r'database_name\s*=\s*\"([^\"]+)\"', block)
        if db_name_match:
            print(db_name_match.group(1))
        else:
            print('gtfs-${PROVIDER}-db')
        sys.exit(0)
" || echo "gtfs-${PROVIDER}-db")

    echo "→ Current database name: ${CURRENT_DB_NAME}"

    # Determine the next version suffix
    NEW_DB_NAME=$(python3 -c "
import re
current = '$CURRENT_DB_NAME'
# Check if it already has a version suffix like -1, -v1, -v2, etc.
match = re.search(r'-(?:v)?(\d+)$', current)
if match:
    version = int(match.group(1)) + 1
    new_name = re.sub(r'-(?:v)?\d+$', f'-v{version}', current)
else:
    new_name = current + '-v1'
print(new_name)
")

    echo "→ New database name: ${NEW_DB_NAME}"

    # Create the new D1 database
    echo "→ Creating new D1 database '${NEW_DB_NAME}'..."
    wrangler d1 create "$NEW_DB_NAME" > .d1_create.tmp

    NEW_DB_ID=$(python3 -c "
import sys, re
with open('.d1_create.tmp', 'r') as f:
    text = f.read()
m = re.search(r'([a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12})', text)
if m:
    print(m.group(1))
else:
    print('', end='')
")
    rm -f .d1_create.tmp

    if [ -z "$NEW_DB_ID" ]; then
        echo "❌ Error: Could not parse database_id from wrangler output for ${PROVIDER}."
        exit 1
    fi

    echo "→ New database_id: ${NEW_DB_ID}"

    # Update providers.toml with new database_id and database_name
    echo "→ Updating ${PROVIDERS_FILE}..."
    python3 - <<EOF
import sys, re

with open('$PROVIDERS_FILE', 'r') as f:
    content = f.read()

blocks = content.split('[[providers]]')
new_blocks = [blocks[0]]

for block in blocks[1:]:
    name_match = re.search(r'name\s*=\s*"([^"]+)"', block)
    if name_match and name_match.group(1) == '$PROVIDER':
        # Update database_id
        if re.search(r'database_id\s*=\s*"[^"]*"', block):
            block = re.sub(r'database_id\s*=\s*"[^"]*"', 'database_id = "${NEW_DB_ID}"', block)
        else:
            block = block.rstrip() + '\ndatabase_id = "${NEW_DB_ID}"\n\n'

        # Update or add database_name
        if re.search(r'database_name\s*=\s*"[^"]*"', block):
            block = re.sub(r'database_name\s*=\s*"[^"]*"', 'database_name = "${NEW_DB_NAME}"', block)
        else:
            # Add database_name after database_id
            block = re.sub(
                r'(database_id\s*=\s*"[^"]*")',
                r'\1\ndatabase_name = "${NEW_DB_NAME}"',
                block
            )
    new_blocks.append(block)

with open('$PROVIDERS_FILE', 'w') as f:
    f.write('[[providers]]'.join(new_blocks))
EOF

    echo "→ Updated ${PROVIDERS_FILE} with new database_id and database_name."

    # Regenerate wrangler.toml
    echo "→ Regenerating wrangler.toml..."
    bash ./generate-wrangler.sh

    # Apply D1 migrations to the new database
    if [ ! -d "migrations/${PROVIDER}" ]; then
        echo "→ Migrations folder 'migrations/${PROVIDER}' not found. Creating empty directory..."
        mkdir -p "migrations/${PROVIDER}"
        echo "  Please add your specific migrations to this folder before deploying."
    fi

    BINDING_NAME="DB_$(echo "$PROVIDER" | tr '[:lower:]' '[:upper:]' | tr '-' '_')"

    echo "→ Applying D1 migrations for '${PROVIDER}' (Binding: ${BINDING_NAME})..."
    CI=true wrangler d1 migrations apply "${BINDING_NAME}" --remote

    echo ""
    echo "✅ Successfully recreated database for provider '${PROVIDER}'"
    echo "   Old DB: ${CURRENT_DB_NAME} (kept as archive)"
    echo "   New DB: ${NEW_DB_NAME} (${NEW_DB_ID})"
    echo ""

done < "$RECREATE_FILE"

# ── Redeploy the worker (once, after all providers are processed) ────────────
echo "→ Redeploying unified worker with updated bindings..."
wrangler deploy
echo ""

echo "═══════════════════════════════════════════════════════════════"
echo "  ✅ Database recreation complete!"
echo "═══════════════════════════════════════════════════════════════"
