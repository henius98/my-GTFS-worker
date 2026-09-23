#!/usr/bin/env python3
"""Generate Bruno status requests from providers.toml (Python 3.11+)."""

from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "bruno" / "Status"
MARKER = "Generated from providers.toml by scripts/generate-bruno.py."


def main():
  with (ROOT / "providers.toml").open("rb") as source:
    providers = tomllib.load(source)["providers"]

  OUTPUT.mkdir(parents=True, exist_ok=True)
  generated = set()
  for provider in providers:
    name = provider.get("name")
    if not name or provider.get("is_active") is False:
      continue
    request = OUTPUT / f"{name}.bru"
    generated.add(request)
    request.write_text(
      f"""meta {{
  name: {name} status
  type: http
  seq: {len(generated)}
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
  Fields: Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt.
  Successful responses are publicly cached for 60 seconds.
  Errors: 404 for an unbound provider; 500 for a database or response decoding error.
}}
""",
      encoding="utf-8",
    )

  for request in OUTPUT.glob("*.bru"):
    if request not in generated and MARKER in request.read_text(encoding="utf-8"):
      request.unlink()
  print(f"Generated {len(generated)} Bruno status requests in {OUTPUT}")


if __name__ == "__main__":
  main()
