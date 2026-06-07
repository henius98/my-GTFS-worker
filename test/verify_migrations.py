import re
import urllib.request
import zipfile
import io
import ssl
import os
import glob
import csv

import os

providers_file = os.path.join(os.path.dirname(__file__), '..', 'providers.toml')
providers = []
with open(providers_file, 'r') as f:
    content = f.read()

blocks = content.split('[[providers]]')[1:]
for block in blocks:
    name_match = re.search(r'name\s*=\s*"([^"]+)"', block)
    url_match = re.search(r'static_url\s*=\s*"([^"]+)"', block)
    provider_match = re.search(r'static_provider\s*=\s*"([^"]+)"', block)
    if name_match and url_match and provider_match:
        is_active_match = re.search(r'is_active\s*=\s*(false|False)', block)
        if not is_active_match:
            providers.append({
                "name": name_match.group(1),
                "url": url_match.group(1) + provider_match.group(1)
            })

ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE

with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "migrations_validation_report.md"), "w") as out:
    out.write("# Migrations Validation Report\n")

    for p in providers:
        name = p['name']
        url = p['url']
        out.write(f"\n## Provider: `{name}`\n")

        script_dir = os.path.dirname(os.path.abspath(__file__))
        migration_dir = os.path.join(script_dir, "..", "migrations", name)
        schema_files = glob.glob(f"{migration_dir}/*gtfs_schema.sql")

        if not schema_files:
            out.write(f"- ❌ Migration file not found in: `{migration_dir}`\n")
            continue

        migration_path = schema_files[-1]  # pick the latest if multiple

        with open(migration_path, 'r') as f:
            migration_sql = f.read()

        # Remove multi-line comments /* ... */ and single-line comments -- ...
        migration_sql_no_comments = re.sub(
            r'/\*.*?\*/', '', migration_sql, flags=re.DOTALL)
        migration_sql_no_comments = re.sub(
            r'--.*', '', migration_sql_no_comments)

        tables = {}
        create_table_pattern = re.compile(
            r'CREATE TABLE IF NOT EXISTS\s+(\w+)\s*\((.*?)\);', re.IGNORECASE | re.DOTALL)
        for match in create_table_pattern.finditer(migration_sql_no_comments):
            table_name = match.group(1).lower()
            # Ignore infrastructure tables
            if table_name in ('trip', 'vehiclepositions', 'logs', 'dataset_versions', 'import_progress'):
                continue

            columns_str = match.group(2)
            columns = []

            # Remove everything inside parentheses to avoid splitting commas in CHECK, DECIMAL, etc.
            while '(' in columns_str:
                columns_str = re.sub(r'\([^)]*\)', '', columns_str)

            lines = columns_str.split(',')
            for line in lines:
                line = line.strip()
                if not line:
                    continue
                first_word = line.split()[0].lower()

                # Skip table-level constraints
                if first_word in ('primary', 'foreign', 'unique', 'check', 'constraint'):
                    continue

                col_name = ''.join(
                    c for c in first_word if c.isalnum() or c == '_')
                if col_name:
                    columns.append(col_name)
            tables[table_name] = columns

        try:
            req = urllib.request.Request(
                url, headers={'User-Agent': 'Mozilla/5.0'})
            with urllib.request.urlopen(req, context=ctx) as response:
                with zipfile.ZipFile(io.BytesIO(response.read())) as z:
                    txt_files = [f for f in z.namelist() if f.endswith('.txt') and not f.startswith(
                        '__MACOSX') and not f.split('/')[-1].startswith('._')]

                    matched_tables = set()

                    for txt_file in txt_files:
                        base_name = txt_file.split('/')[-1][:-4].lower()
                        table_name = base_name

                        if table_name not in tables:
                            out.write(
                                f"- ⚠️ Table `{table_name}` from CSV is NOT in migration.\n")
                            continue

                        matched_tables.add(table_name)

                        with z.open(txt_file) as csv_file:
                            header_line = csv_file.readline().decode('utf-8-sig')
                            if not header_line.strip():
                                continue

                            csv_reader = csv.reader([header_line.strip()])
                            csv_columns_raw = next(csv_reader)

                            csv_columns = [col.strip().lower()
                                           for col in csv_columns_raw]
                            csv_columns = [
                                ''.join(c for c in col if c.isalnum() or c == '_') for col in csv_columns]

                            db_columns = tables[table_name]

                            if db_columns == csv_columns:
                                out.write(
                                    f"- ✅ `{table_name}` matches exactly.\n")
                            else:
                                out.write(f"- ❌ Mismatch in `{table_name}`:\n")
                                missing = set(csv_columns) - set(db_columns)
                                extra = set(db_columns) - set(csv_columns)
                                if missing:
                                    out.write(
                                        f"  - Missing in DB (Extra in CSV): {missing}\n")
                                if extra:
                                    out.write(
                                        f"  - Extra in DB (Missing in CSV): {extra}\n")
                                if not missing and not extra:
                                    out.write(
                                        f"  - Order mismatch!\n  - DB : {db_columns}\n  - CSV: {csv_columns}\n")

                    for t_name in tables:
                        if t_name not in matched_tables:
                            out.write(
                                f"- ⚠️ Table `{t_name}` in migration but no CSV.\n")

        except Exception as e:
            out.write(f"- Error: {e}\n")

print("Done generating migrations validation report.")
