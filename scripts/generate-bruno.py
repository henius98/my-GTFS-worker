#!/usr/bin/env python3
"""Generate Bruno status and original GTFS requests from providers.toml (Python 3.11+)."""

from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parents[1]
STATUS_OUTPUT = ROOT / "bruno" / "Status"
ORIGINAL_OUTPUT = ROOT / "bruno" / "Original GTFS"
MARKER = "Generated from providers.toml by scripts/generate-bruno.py."

def make_unzip_post_response_script(provider_name: str) -> str:
  return f"""script:post-response {{
  if (typeof res.getDataBuffer !== 'function') {{
    console.log(" Enable Bruno Developer Mode (click the shield icon at top-right -> Developer Mode) to unzip binary GTFS archives.");
    return;
  }}

  const zlib = require('zlib');
  const fs = require('fs');
  const path = require('path');

  const raw = res.getDataBuffer();
  const buf = Buffer.isBuffer(raw) ? raw : Buffer.from(raw);
  if (!buf || buf.length < 30 || buf.readUInt32LE(0) !== 0x04034b50) {{
    console.log("No valid ZIP archive received (status: " + res.status + ")");
    return;
  }}

  let eocdOffset = -1;
  for (let i = buf.length - 22; i >= 0; i--) {{
    if (buf.readUInt32LE(i) === 0x06054b50) {{
      eocdOffset = i;
      break;
    }}
  }}

  if (eocdOffset === -1) {{
    console.log("Invalid ZIP archive: End of Central Directory not found");
    return;
  }}

  const outDir = path.join(bru.cwd(), 'Original GTFS', 'extracted', '{provider_name}');
  fs.mkdirSync(outDir, {{ recursive: true }});

  const cdOffset = buf.readUInt32LE(eocdOffset + 16);
  const totalEntries = buf.readUInt16LE(eocdOffset + 10);
  let offset = cdOffset;
  let extractedCount = 0;

  for (let i = 0; i < totalEntries && offset + 46 <= buf.length; i++) {{
    if (buf.readUInt32LE(offset) !== 0x02014b50) break;
    const method = buf.readUInt16LE(offset + 10);
    const compSize = buf.readUInt32LE(offset + 20);
    const nameLen = buf.readUInt16LE(offset + 28);
    const extraLen = buf.readUInt16LE(offset + 30);
    const commentLen = buf.readUInt16LE(offset + 32);
    const localOffset = buf.readUInt32LE(offset + 42);
    const name = buf.toString('utf8', offset + 46, offset + 46 + nameLen);

    if (localOffset + 30 <= buf.length && buf.readUInt32LE(localOffset) === 0x04034b50) {{
      const localNameLen = buf.readUInt16LE(localOffset + 26);
      const localExtraLen = buf.readUInt16LE(localOffset + 28);
      const dataStart = localOffset + 30 + localNameLen + localExtraLen;
      const compData = buf.subarray(dataStart, dataStart + compSize);
      if ((name.endsWith('.txt') || name.endsWith('.csv')) && !name.startsWith('__MACOSX')) {{
        let content;
        if (method === 0) {{
          content = compData;
        }} else if (method === 8) {{
          content = zlib.inflateRawSync(compData);
        }}
        if (content !== undefined) {{
          const safeName = path.basename(name).replace(/\\.txt$/i, '.csv');
          fs.writeFileSync(path.join(outDir, safeName), content);
          extractedCount++;
        }}
      }}
    }}
    offset += 46 + nameLen + extraLen + commentLen;
  }}

  console.log("Extracted " + extractedCount + " files to " + outDir);
}}
"""


def main():
  with (ROOT / "providers.toml").open("rb") as source:
    providers = tomllib.load(source)["providers"]

  STATUS_OUTPUT.mkdir(parents=True, exist_ok=True)
  ORIGINAL_OUTPUT.mkdir(parents=True, exist_ok=True)

  status_folder = STATUS_OUTPUT / "folder.bru"
  if not status_folder.exists():
    status_folder.write_text("meta {\n  name: Status\n}\n", encoding="utf-8")

  original_folder = ORIGINAL_OUTPUT / "folder.bru"
  if not original_folder.exists():
    original_folder.write_text("meta {\n  name: Original GTFS\n}\n", encoding="utf-8")

  generated_status = set()
  generated_original = set()

  for provider in providers:
    name = provider.get("name")
    if not name or provider.get("is_active") is False:
      continue

    # Status request
    status_request = STATUS_OUTPUT / f"{name}.bru"
    generated_status.add(status_request)
    status_request.write_text(
      f"""meta {{
  name: {name} status
  type: http
  seq: {len(generated_status)}
}}

get {{
  url: {{{{baseUrl}}}}/{name}/status
  body: none
  auth: none
}}

headers {{
  Accept: application/json
}}

docs {{
  {MARKER}
  Regenerate with: python3 scripts/generate-bruno.py
  Returns an array of import progress records; an empty array is valid.
  Fields: FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt.
  Successful responses are publicly cached for 60 seconds.
  Errors: 404 for an unbound provider; 500 for a database or response decoding error.
}}
""",
      encoding="utf-8",
    )

    # Original GTFS request
    static_provider = provider.get("static_provider")
    if static_provider:
      if "?" in static_provider:
        endpoint, query_str = static_provider.split("?", 1)
        query_params = dict(param.split("=", 1) for param in query_str.split("&") if "=" in param)
      else:
        endpoint = static_provider
        query_params = {}

      if query_params:
        params_lines = "\n".join(f"  {k}: {v}" for k, v in query_params.items())
        query_section = f"\nparams:query {{\n{params_lines}\n}}\n"
      else:
        query_section = ""

      original_request = ORIGINAL_OUTPUT / f"{name}.bru"
      generated_original.add(original_request)
      original_request.write_text(
        f"""meta {{
  name: {name} original GTFS
  type: http
  seq: {len(generated_original)}
}}

get {{
  url: {{{{staticBaseUrl}}}}/{endpoint}
  body: none
  auth: none
}}
{query_section}
headers {{
  ~If-None-Match: 
}}

{make_unzip_post_response_script(name)}
docs {{
  {MARKER}
  Regenerate with: python3 scripts/generate-bruno.py
  Downloads the original GTFS static ZIP archive from data.gov.my.
  Requires Bruno Developer Mode (click the shield icon at top-right -> Developer Mode).
  The post-response script extracts all .csv files to bruno/Original GTFS/extracted/{name}/.
  Set optional If-None-Match header with an ETag to check for feed updates (returns 304 if unchanged).
}}
""",
        encoding="utf-8",
      )

  for request in STATUS_OUTPUT.glob("*.bru"):
    if request.name == "folder.bru":
      continue
    if request not in generated_status and MARKER in request.read_text(encoding="utf-8"):
      request.unlink()
  print(f"Generated {len(generated_status)} Bruno status requests in {STATUS_OUTPUT}")

  for request in ORIGINAL_OUTPUT.glob("*.bru"):
    if request.name == "folder.bru":
      continue
    if request not in generated_original and MARKER in request.read_text(encoding="utf-8"):
      request.unlink()
  print(f"Generated {len(generated_original)} Bruno original GTFS requests in {ORIGINAL_OUTPUT}")


if __name__ == "__main__":
  main()
