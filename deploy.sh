#!/bin/bash
# deploy.sh — Full lifecycle deployment for the unified GTFS worker.
set -euo pipefail

PROVIDERS_FILE="providers.toml"

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

# Ensure Python 3 is available for robust TOML parsing
if ! command -v python3 &> /dev/null; then
    echo "❌ Error: 'python3' is required to securely parse providers.toml."
    exit 1
fi

echo "🚀 Starting Unified GTFS Worker Deployment..."

PROVIDERS=$(python3 -c "
import sys, re
try:
    with open('$PROVIDERS_FILE', 'r') as f:
        blocks = f.read().split('[[providers]]')
    for block in blocks[1:]:
        name_match = re.search(r'name\s*=\s*\"([^\"]+)\"', block)
        if name_match:
            # Check is_active flag. Defaults to True if not present or not set to false.
            is_active_match = re.search(r'is_active\s*=\s*(false|False)', block)
            if not is_active_match:
                print(name_match.group(1))
except Exception as e:
    sys.exit(1)
")

# ── Step 1: Provision Missing Databases ──────────────────────────────────────
for PROVIDER in $PROVIDERS; do
    DB_ID=$(python3 -c "
import sys, re
with open('$PROVIDERS_FILE', 'r') as f:
    blocks = f.read().split('[[providers]]')
for block in blocks:
    name_match = re.search(r'name\s*=\s*\"([^\"]+)\"', block)
    if name_match and name_match.group(1) == '$PROVIDER':
        db_match = re.search(r'database_id\s*=\s*\"([^\"]+)\"', block)
        if db_match:
            print(db_match.group(1))
            sys.exit(0)
" || true)

    if [ -z "$DB_ID" ]; then
        # Check for explicit database_name in providers.toml
        DB_NAME=$(python3 -c "
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
        echo "→ database_id is empty for provider '${PROVIDER}'. Checking if '${DB_NAME}' exists..."
        
        if ! wrangler d1 info "$DB_NAME" > .d1_info.tmp 2>/dev/null; then
            echo "→ '${DB_NAME}' info failed. Checking if it already exists in list..."
            # Note: wrangler d1 list may not show un-bound DBs easily without parsing JSON or grep.
            # Let's try parsing it from wrangler d1 list output
            wrangler d1 list > .d1_list.tmp 2>/dev/null
            EXISTING_ID=$(grep -oP "[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12}(?=\s*│\s*$DB_NAME)" .d1_list.tmp || true)
            
            if [ -z "$EXISTING_ID" ]; then
                echo "→ '${DB_NAME}' not found. Creating it now..."
                wrangler d1 create "$DB_NAME" > .d1_info.tmp
            else
                echo "→ '${DB_NAME}' already exists (ID: $EXISTING_ID). Emulating info output..."
                echo "$EXISTING_ID" > .d1_info.tmp
            fi
        else
            echo "→ '${DB_NAME}' info retrieved successfully."
        fi
        
        DB_ID=$(python3 -c "
import sys, re
with open('.d1_info.tmp', 'r') as f:
    text = f.read()
m = re.search(r'([a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12})', text)
if m:
    print(m.group(1))
")
        rm -f .d1_info.tmp .d1_list.tmp

        if [ -z "$DB_ID" ]; then
            echo "❌ Error: Could not parse database_id from wrangler output for ${PROVIDER}."
            exit 1
        fi
        
        echo "→ Found database_id: ${DB_ID}. Updating ${PROVIDERS_FILE}..."
        
        python3 - << EOF
import sys, re
with open('$PROVIDERS_FILE', 'r') as f:
    content = f.read()

blocks = content.split('[[providers]]')
new_blocks = [blocks[0]]

for block in blocks[1:]:
    name_match = re.search(r'name\s*=\s*\"([^\"]+)\"', block)
    if name_match and name_match.group(1) == '$PROVIDER':
        if re.search(r'database_id\s*=\s*\"[^\"]*\"', block):
            block = re.sub(r'database_id\s*=\s*\"[^\"]*\"', 'database_id = "${DB_ID}"', block)
        else:
            block = block.rstrip() + '\ndatabase_id = "${DB_ID}"\n\n'
    new_blocks.append(block)

with open('$PROVIDERS_FILE', 'w') as f:
    f.write('[[providers]]'.join(new_blocks))
EOF
    fi
done

echo ""
# ── Step 2: Regenerate wrangler.toml ─────────────────────────────────────────
echo "→ Regenerating unified wrangler.toml from providers.toml..."
bash ./generate-wrangler.sh
echo ""

# ── Step 3: Apply D1 migrations ──────────────────────────────────────────────
for PROVIDER in $PROVIDERS; do
    if [ ! -d "migrations/${PROVIDER}" ]; then
        echo "→ Migrations folder 'migrations/${PROVIDER}' not found. Creating empty directory..."
        mkdir -p "migrations/${PROVIDER}"
        echo "  Please add your specific migrations to this folder before deploying."
    fi

    # Convert provider name to uppercase and replace dashes with underscores
    BINDING_NAME="DB_$(echo "$PROVIDER" | tr '[:lower:]' '[:upper:]' | tr '-' '_')"
    
    echo "→ Applying D1 migrations for '${PROVIDER}' (Binding: ${BINDING_NAME})..."
    CI=true wrangler d1 migrations apply "${BINDING_NAME}" --remote
    echo ""
done

# ── Step 4: Deploy the unified worker ────────────────────────────────────────
echo "→ Deploying unified worker..."
wrangler deploy
echo ""

echo "═══════════════════════════════════════════════════════════════"
echo "  ✅ Unified Deployment Complete!"
echo "═══════════════════════════════════════════════════════════════"
echo ""
echo "Useful commands:"
echo "  Logs:    wrangler tail"
echo "  Status:  curl https://my-gtfs-worker.<your-subdomain>.workers.dev/<provider>/status"
