#!/usr/bin/env python3
"""Generate openapi.yaml for the Cloudflare Worker from repo sources of truth (Python 3.11+)."""

from pathlib import Path
import re
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
OUTPUT_PATH = ROOT / "openapi.yaml"

YAML_RESERVED = {
  "true",
  "false",
  "null",
  "~",
  "yes",
  "no",
  "on",
  "off",
  "y",
  "n",
}
YAML_NUMBER_RE = re.compile(r"^[+-]?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$|^0x[0-9a-fA-F]+$|^0o[0-7]+$|^[+-]?\.(?:inf|Inf|INF)$|^\.(?:nan|NaN|NAN)$")
YAML_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}(?:[Tt\s].*)?$")
YAML_PLAIN_SAFE_RE = re.compile(r"^[A-Za-z0-9_./+@-][A-Za-z0-9_./+@ -]*$")


def strip_sql_comments(sql: str) -> str:
  """Remove SQL -- and /* */ comments while preserving single-quoted string literals."""
  out: list[str] = []
  in_string = False
  i = 0
  n = len(sql)
  while i < n:
    ch = sql[i]
    if ch == "'":
      if not in_string:
        in_string = True
        out.append(ch)
        i += 1
      elif i + 1 < n and sql[i + 1] == "'":
        out.append("''")
        i += 2
      else:
        in_string = False
        out.append(ch)
        i += 1
    elif not in_string and ch == "-" and i + 1 < n and sql[i + 1] == "-":
      i += 2
      while i < n and sql[i] != "\n":
        i += 1
    elif not in_string and ch == "/" and i + 1 < n and sql[i + 1] == "*":
      i += 2
      while i + 1 < n and not (sql[i] == "*" and sql[i + 1] == "/"):
        i += 1
      i = min(i + 2, n)
      out.append(" ")
    else:
      out.append(ch)
      i += 1
  return "".join(out)


def parse_schema_tables(schema_path: Path) -> list[str]:
  """Extract CREATE TABLE names from a provider's 0_gtfs_schema.sql (matching scripts/schema_parser.rs)."""
  clean_sql = strip_sql_comments(schema_path.read_text(encoding="utf-8"))
  pattern = re.compile(
    r"\bCREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:\"([^\"]+)\"|`([^`]+)`|'([^']+)'|\[([^\]]+)\]|([^\s(,;]+))\s*\(",
    re.IGNORECASE,
  )
  tables: list[str] = []
  seen: set[str] = set()
  for match in pattern.finditer(clean_sql):
    name = next(group for group in match.groups() if group is not None).lower()
    if name and name not in seen:
      seen.add(name)
      tables.append(name)
  return sorted(tables)


def load_active_providers() -> list[str]:
  with (ROOT / "providers.toml").open("rb") as f:
    data = tomllib.load(f)
  providers: list[str] = []
  for entry in data.get("providers", []):
    name = entry.get("name")
    if name and entry.get("is_active") is not False:
      providers.append(name)
  return providers


def load_workspace_version() -> str:
  with (ROOT / "Cargo.toml").open("rb") as f:
    data = tomllib.load(f)
  return str(data.get("workspace", {}).get("package", {}).get("version", "0.1.0"))


def load_worker_vars() -> dict[str, str]:
  defaults = {
    "WORKER_SQL_MAX_ROWS": "1000",
    "WORKER_API_CACHE_TTL_SECONDS": "60",
    "WORKER_MAP_MAX_ROUTES": "20",
    "WORKER_MAP_CACHE_EXPIRE_TIME": "05:00",
    "WORKER_DEFAULT_TIMEZONE": "Asia/Kuala_Lumpur",
  }
  wrangler_path = ROOT / "wrangler.toml"
  if wrangler_path.exists():
    with wrangler_path.open("rb") as f:
      wrangler = tomllib.load(f)
    for key, value in wrangler.get("vars", {}).items():
      defaults[key] = str(value)
  return defaults


def load_bruno_base_url(env_file: Path, fallback: str) -> str:
  if not env_file.exists():
    return fallback
  match = re.search(r"^\s*baseUrl:\s*(\S+)\s*$", env_file.read_text(encoding="utf-8"), re.MULTILINE)
  return match.group(1) if match else fallback


def load_provider_tables(providers: list[str]) -> dict[str, list[str]]:
  mapping: dict[str, list[str]] = {}
  for provider in providers:
    schema_path = ROOT / "migrations" / provider / "0_gtfs_schema.sql"
    if schema_path.exists():
      mapping[provider] = parse_schema_tables(schema_path)
  return mapping


def build_openapi_spec() -> dict[str, Any]:
  version = load_workspace_version()
  providers = load_active_providers()
  provider_tables = load_provider_tables(providers)
  all_tables = sorted({table for tables in provider_tables.values() for table in tables})
  worker_vars = load_worker_vars()

  sql_max_rows = int(worker_vars["WORKER_SQL_MAX_ROWS"])
  api_cache_ttl = int(worker_vars["WORKER_API_CACHE_TTL_SECONDS"])
  map_max_routes = int(worker_vars["WORKER_MAP_MAX_ROUTES"])
  map_cache_expire_time = worker_vars["WORKER_MAP_CACHE_EXPIRE_TIME"]
  default_timezone = worker_vars["WORKER_DEFAULT_TIMEZONE"]

  prod_url = load_bruno_base_url(
    ROOT / "bruno" / "environments" / "Production.bru",
    "https://my-gtfs-worker.98heniyschen.workers.dev",
  )
  local_url = load_bruno_base_url(
    ROOT / "bruno" / "environments" / "Local.bru",
    "http://localhost:8787",
  )

  table_availability_lines = "\n".join(
    f"- `{provider}`: {', '.join(f'`{t}`' for t in tables)}"
    for provider, tables in provider_tables.items()
  )
  table_description = (
    "Table compiled into the provider's D1 schema (matched case-insensitively).\n"
    "Available tables per provider:\n"
    f"{table_availability_lines}"
  )

  cors_header_ref = {"$ref": "#/components/headers/CorsAllowOrigin"}
  allow_header_ref = {"$ref": "#/components/headers/Allow"}
  cache_control_ref = {"$ref": "#/components/headers/CacheControl"}

  return {
    "openapi": "3.1.0",
    "info": {
      "title": "my-GTFS-worker API",
      "version": version,
      "summary": "Unified Cloudflare Worker API for Malaysian GTFS static transit datasets.",
      "description": (
        "Serves per-provider GTFS static datasets stored in isolated Cloudflare D1 databases.\n\n"
        "**CORS**: Every response includes `Access-Control-Allow-Origin` from `WORKER_CORS_ALLOW_ORIGIN`. "
        "`OPTIONS` preflight requests return `204 No Content` with `Access-Control-Allow-Methods: GET, POST, OPTIONS` "
        "and `Access-Control-Allow-Headers: Content-Type, Authorization, X-Admin-Key`.\n\n"
        "**Rate limits**: Exceeding the Cloudflare D1 daily row read limit returns `429 Too Many Requests` "
        "with a JSON `ErrorResponse` body."
      ),
    },
    "servers": [
      {"url": prod_url, "description": "Production Cloudflare Worker"},
      {"url": local_url, "description": "Local Wrangler dev server"},
    ],
    "tags": [
      {"name": "Health", "description": "Worker liveness endpoint."},
      {"name": "Status", "description": "GTFS import progress per provider."},
      {"name": "Data", "description": "Paginated, filterable GTFS table queries."},
      {"name": "SQL", "description": "Admin-protected read-only SQL queries."},
      {"name": "Map", "description": "GeoJSON transit network routes and stops."},
    ],
    "paths": {
      "/": {
        "get": {
          "tags": ["Health"],
          "operationId": "getHealth",
          "summary": "Worker liveness check",
          "description": "Returns plain text confirming that the Worker is running. Does not query D1.",
          "responses": {
            "200": {
              "description": "Worker is running.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "text/plain": {
                  "schema": {"type": "string"},
                  "example": "Worker is running. Use /<provider>/status to check GTFS import progress.",
                }
              },
            },
            "500": {"$ref": "#/components/responses/InternalServerError"},
          },
        }
      },
      "/{provider}/status": {
        "parameters": [{"$ref": "#/components/parameters/Provider"}],
        "get": {
          "tags": ["Status"],
          "operationId": "getProviderStatus",
          "summary": "Get GTFS import progress for a provider",
          "description": (
            "Returns all rows from `0_import_progress` for the provider. "
            f"Canonicalizes cache keys and edge-caches successful responses for `WORKER_API_CACHE_TTL_SECONDS` (currently `{api_cache_ttl}`s)."
          ),
          "responses": {
            "200": {
              "description": "Import progress records (may be an empty array).",
              "headers": {
                "Access-Control-Allow-Origin": cors_header_ref,
                "Cache-Control": cache_control_ref,
              },
              "content": {
                "application/json": {
                  "schema": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ImportProgress"},
                  }
                }
              },
            },
            "404": {"$ref": "#/components/responses/ProviderNotFound"},
            "405": {"$ref": "#/components/responses/MethodNotAllowedText"},
            "429": {"$ref": "#/components/responses/RateLimitExceeded"},
            "500": {"$ref": "#/components/responses/InternalServerError"},
          },
        },
      },
      "/{provider}/data/{table}": {
        "parameters": [
          {"$ref": "#/components/parameters/Provider"},
          {"$ref": "#/components/parameters/Table"},
        ],
        "get": {
          "tags": ["Data"],
          "operationId": "listTableRows",
          "summary": "Query rows from a provider table",
          "description": (
            "Returns paginated rows for any table compiled into the provider's D1 schema. "
            "Duplicate query parameter keys return `400`. Column names are resolved case-insensitively.\n\n"
            f"Responses set `Cache-Control: public, max-age={api_cache_ttl}` (from `WORKER_API_CACHE_TTL_SECONDS`)."
          ),
          "parameters": [
            {
              "name": "limit",
              "in": "query",
              "required": False,
              "description": f"Maximum rows to return. Values above `WORKER_SQL_MAX_ROWS` ({sql_max_rows}) are clamped to {sql_max_rows}.",
              "schema": {
                "type": "integer",
                "minimum": 0,
                "default": sql_max_rows,
              },
              "example": 10,
            },
            {
              "name": "offset",
              "in": "query",
              "required": False,
              "description": "Number of rows to skip before returning results.",
              "schema": {
                "type": "integer",
                "minimum": 0,
                "default": 0,
              },
              "example": 0,
            },
            {
              "name": "include",
              "in": "query",
              "required": False,
              "description": "Comma-separated list of columns to return. Mutually exclusive with `exclude`; must not be empty.",
              "schema": {"type": "string", "minLength": 1},
              "example": "stop_id,stop_name",
            },
            {
              "name": "exclude",
              "in": "query",
              "required": False,
              "description": "Comma-separated list of columns to omit. Mutually exclusive with `include`; cannot exclude all columns.",
              "schema": {"type": "string", "minLength": 1},
              "example": "stop_lat,stop_lon",
            },
            {
              "name": "filter",
              "in": "query",
              "required": False,
              "description": "Exact case-sensitive match (`=`). Comma-separated `val@col` items; escape literal commas in values with `\\,`.",
              "schema": {"type": "string"},
              "example": "KJ10@stop_id",
            },
            {
              "name": "ifilter",
              "in": "query",
              "required": False,
              "description": "Exact case-insensitive match (`COLLATE NOCASE =`). Comma-separated `val@col` items; escape literal commas with `\\,`.",
              "schema": {"type": "string"},
              "example": "kj10@stop_id",
            },
            {
              "name": "contains",
              "in": "query",
              "required": False,
              "description": "Case-sensitive substring match (`INSTR(col, val) > 0`). Comma-separated `val@col` items; escape literal commas with `\\,`.",
              "schema": {"type": "string"},
              "example": "KJ@stop_id",
            },
            {
              "name": "icontains",
              "in": "query",
              "required": False,
              "description": "Case-insensitive substring match (`LIKE '%val%'`). Comma-separated `val@col` items; escape literal commas with `\\,`.",
              "schema": {"type": "string"},
              "example": "sentral@stop_name",
            },
            {
              "name": "range",
              "in": "query",
              "required": False,
              "description": (
                "Inclusive range filter (`>=` / `<=`). Comma-separated `col[begin:end]` or `col[begin..end]` items. "
                "Supports open bounds (`col[:end]`, `col[begin:]`) and `HH:MM[:SS]` time values."
              ),
              "schema": {"type": "string"},
              "example": "stop_lat[1:7]",
            },
            {
              "name": "sort",
              "in": "query",
              "required": False,
              "description": "Comma-separated columns to sort by. Prefix a column with `-` for descending order.",
              "schema": {"type": "string"},
              "example": "stop_name,-stop_id",
            },
          ],
          "responses": {
            "200": {
              "description": "Paginated table rows.",
              "headers": {
                "Access-Control-Allow-Origin": cors_header_ref,
                "Cache-Control": cache_control_ref,
              },
              "content": {
                "application/json": {
                  "schema": {"$ref": "#/components/schemas/DataResponse"}
                }
              },
            },
            "400": {"$ref": "#/components/responses/BadRequestText"},
            "404": {
              "description": "Unknown provider or table not compiled into the provider's schema.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "text/plain": {
                  "schema": {"type": "string"},
                  "example": "Table 'unknown' not found",
                }
              },
            },
            "405": {"$ref": "#/components/responses/MethodNotAllowedText"},
            "429": {"$ref": "#/components/responses/RateLimitExceeded"},
            "500": {"$ref": "#/components/responses/InternalServerError"},
          },
        },
      },
      "/{provider}/sql": {
        "parameters": [{"$ref": "#/components/parameters/Provider"}],
        "post": {
          "tags": ["SQL"],
          "operationId": "executeSqlQuery",
          "summary": "Run a read-only SQL query",
          "description": (
            "Executes one read-only `SELECT` or `WITH ... SELECT` statement wrapped in a subquery "
            f"capped at `WORKER_SQL_MAX_ROWS` ({sql_max_rows}). SQL comments (`--`, `/*`) and multiple statements are rejected; "
            "a single trailing semicolon is allowed.\n\n"
            "Requires `WORKER_ADMIN_KEY` via `Authorization: Bearer <key>` or `X-Admin-Key: <key>` "
            "(unless `WORKER_ALLOW_ANONYMOUS_SQL=true` or `WORKER_TEST_NOW` is set for local/test environments)."
          ),
          "security": [
            {"BearerAuth": []},
            {"AdminKeyHeader": []},
          ],
          "requestBody": {
            "required": True,
            "description": "Single read-only SQL `SELECT` or `WITH ... SELECT` statement.",
            "content": {
              "text/plain": {
                "schema": {"type": "string", "minLength": 1},
                "example": "SELECT stop_id, stop_name FROM stops LIMIT 10",
              }
            },
          },
          "responses": {
            "200": {
              "description": "Query result rows.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "application/json": {
                  "schema": {"$ref": "#/components/schemas/SqlResponse"}
                }
              },
            },
            "400": {"$ref": "#/components/responses/BadRequestText"},
            "401": {
              "description": "Missing or invalid admin key.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "text/plain": {
                  "schema": {"type": "string"},
                  "example": "Unauthorized",
                }
              },
            },
            "403": {
              "description": "`WORKER_ADMIN_KEY` is not configured and anonymous SQL is disabled.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "text/plain": {
                  "schema": {"type": "string"},
                  "example": "Forbidden: WORKER_ADMIN_KEY is not configured",
                }
              },
            },
            "404": {"$ref": "#/components/responses/ProviderNotFound"},
            "405": {
              "description": "Method not allowed. Only `POST` is accepted.",
              "headers": {
                "Access-Control-Allow-Origin": cors_header_ref,
                "Allow": allow_header_ref,
              },
              "content": {
                "text/plain": {
                  "schema": {"type": "string"},
                  "example": "Method Not Allowed",
                }
              },
            },
            "429": {"$ref": "#/components/responses/RateLimitExceeded"},
            "500": {"$ref": "#/components/responses/InternalServerError"},
          },
        },
      },
      "/{provider}/map": {
        "parameters": [{"$ref": "#/components/parameters/Provider"}],
        "get": {
          "tags": ["Map"],
          "operationId": "getProviderMap",
          "summary": "Get provider transit network as GeoJSON",
          "description": (
            "Returns active routes and stops for a service date as a GeoJSON `FeatureCollection`. "
            "Providers with a `shapes` table emit `LineString` route features (`geometry_source: \"shape\"`); "
            "providers without shapes (or routes without shape points) fall back to `MultiLineString` stop-to-stop segments "
            "(`geometry_source: \"stop_sequence\"`). Stops emit `Point` features.\n\n"
            f"Edge-cached per route until the next daily `WORKER_MAP_CACHE_EXPIRE_TIME` (`{map_cache_expire_time}` in `{default_timezone}`); multi-route calls query D1 only for cache misses."
          ),
          "parameters": [
            {
              "name": "route_id",
              "in": "query",
              "required": False,
              "description": (
                f"Optional comma-separated GTFS route identifiers to filter the map "
                f"(1 to {map_max_routes} unique routes, each 1 to 256 bytes after trimming). "
                "Returns `404` only if zero requested routes have active trips on the target date."
              ),
              "schema": {"type": "string", "minLength": 1},
              "example": "KJL,PYL",
            },
            {
              "name": "date",
              "in": "query",
              "required": False,
              "description": f"Service date in `YYYY-MM-DD` or `YYYYMMDD` format. Defaults to today in `{default_timezone}`.",
              "schema": {
                "type": "string",
                "pattern": r"^(\d{4}-\d{2}-\d{2}|\d{8})$",
              },
              "example": "2026-09-23",
            },
          ],
          "responses": {
            "200": {
              "description": "GeoJSON FeatureCollection of route geometries and stops.",
              "headers": {
                "Access-Control-Allow-Origin": cors_header_ref,
                "Cache-Control": cache_control_ref,
              },
              "content": {
                "application/json": {
                  "schema": {"$ref": "#/components/schemas/MapFeatureCollection"}
                }
              },
            },
            "400": {"$ref": "#/components/responses/BadRequestJson"},
            "404": {
              "description": "Provider database unavailable or requested `route_id` not found.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "application/json": {
                  "schema": {"$ref": "#/components/schemas/ErrorResponse"},
                  "example": {"error": "Not found route_id"},
                }
              },
            },
            "405": {"$ref": "#/components/responses/MethodNotAllowedJson"},
            "429": {"$ref": "#/components/responses/RateLimitExceeded"},
            "500": {"$ref": "#/components/responses/InternalServerError"},
            "503": {
              "description": "Map query failed against D1.",
              "headers": {"Access-Control-Allow-Origin": cors_header_ref},
              "content": {
                "application/json": {
                  "schema": {"$ref": "#/components/schemas/ErrorResponse"},
                  "example": {"error": "Map data is unavailable"},
                }
              },
            },
          },
        },
      },
    },
    "components": {
      "securitySchemes": {
        "BearerAuth": {
          "type": "http",
          "scheme": "bearer",
          "description": "Admin key passed as `Authorization: Bearer <WORKER_ADMIN_KEY>`.",
        },
        "AdminKeyHeader": {
          "type": "apiKey",
          "in": "header",
          "name": "X-Admin-Key",
          "description": "Admin key passed via the `X-Admin-Key` header.",
        },
      },
      "parameters": {
        "Provider": {
          "name": "provider",
          "in": "path",
          "required": True,
          "description": "Active GTFS provider identifier from `providers.toml` (matched case-insensitively).",
          "schema": {
            "type": "string",
            "enum": providers,
          },
          "example": providers[0] if providers else "ktmb",
        },
        "Table": {
          "name": "table",
          "in": "path",
          "required": True,
          "description": table_description,
          "schema": {
            "type": "string",
            "enum": all_tables,
          },
          "example": "stops",
        },
      },
      "headers": {
        "CorsAllowOrigin": {
          "description": "CORS allowed origin from `WORKER_CORS_ALLOW_ORIGIN`.",
          "schema": {"type": "string"},
        },
        "Allow": {
          "description": "Allowed HTTP method(s) for the endpoint.",
          "schema": {"type": "string"},
        },
        "CacheControl": {
          "description": "HTTP caching directive for the response.",
          "schema": {"type": "string"},
        },
      },
      "responses": {
        "BadRequestText": {
          "description": "Invalid query parameter, column, or SQL statement.",
          "headers": {"Access-Control-Allow-Origin": cors_header_ref},
          "content": {
            "text/plain": {
              "schema": {"type": "string"},
            }
          },
        },
        "BadRequestJson": {
          "description": "Invalid query parameter.",
          "headers": {"Access-Control-Allow-Origin": cors_header_ref},
          "content": {
            "application/json": {
              "schema": {"$ref": "#/components/schemas/ErrorResponse"},
            }
          },
        },
        "ProviderNotFound": {
          "description": "Provider does not exist or its D1 database binding is unavailable.",
          "headers": {"Access-Control-Allow-Origin": cors_header_ref},
          "content": {
            "text/plain": {
              "schema": {"type": "string"},
              "example": "Provider 'unknown' not found or DB not bound",
            }
          },
        },
        "MethodNotAllowedText": {
          "description": "Method not allowed. Only `GET` is accepted.",
          "headers": {
            "Access-Control-Allow-Origin": cors_header_ref,
            "Allow": allow_header_ref,
          },
          "content": {
            "text/plain": {
              "schema": {"type": "string"},
              "example": "Method Not Allowed",
            }
          },
        },
        "MethodNotAllowedJson": {
          "description": "Method not allowed. Only `GET` is accepted.",
          "headers": {
            "Access-Control-Allow-Origin": cors_header_ref,
            "Allow": allow_header_ref,
          },
          "content": {
            "application/json": {
              "schema": {"$ref": "#/components/schemas/ErrorResponse"},
              "example": {"error": "Method not allowed"},
            }
          },
        },
        "RateLimitExceeded": {
          "description": "Cloudflare D1 daily row read limit exceeded.",
          "headers": {"Access-Control-Allow-Origin": cors_header_ref},
          "content": {
            "application/json": {
              "schema": {"$ref": "#/components/schemas/ErrorResponse"},
              "example": {"error": "Database daily row read limit exceeded. Please try again after midnight UTC."},
            }
          },
        },
        "InternalServerError": {
          "description": "Unhandled server or configuration error.",
          "headers": {"Access-Control-Allow-Origin": cors_header_ref},
          "content": {
            "text/plain": {
              "schema": {"type": "string"},
              "example": "Internal Server Error",
            }
          },
        },
      },
      "schemas": {
        "ErrorResponse": {
          "type": "object",
          "required": ["error"],
          "additionalProperties": False,
          "properties": {
            "error": {"type": "string", "description": "Human-readable error message."}
          },
        },
        "ImportProgress": {
          "type": "object",
          "required": [
            "FileName",
            "CRC",
            "LastProcessedLine",
            "LastProcessedByte",
            "Status",
            "UpdatedAt",
          ],
          "additionalProperties": False,
          "properties": {
            "FileName": {"type": ["string", "null"], "description": "GTFS CSV file name in the upstream archive."},
            "CRC": {"type": ["string", "null"], "description": "CRC32 checksum of the CSV file."},
            "LastProcessedLine": {"type": ["integer", "null"], "format": "int64", "description": "Last committed CSV data line."},
            "LastProcessedByte": {"type": ["integer", "null"], "format": "int64", "description": "Byte offset checkpoint for resumable imports."},
            "Status": {
              "type": ["integer", "null"],
              "enum": [0, 1, 2, None],
              "description": "Import status: `0` = COMPLETED, `1` = IN_PROGRESS, `2` = STAGED.",
            },
            "UpdatedAt": {"type": ["string", "null"], "description": "Timestamp of the last progress update."},
          },
        },
        "DataRow": {
          "type": "object",
          "description": "Arbitrary table row whose keys match the selected columns.",
          "additionalProperties": True,
        },
        "DataResponse": {
          "type": "object",
          "required": ["data", "limit", "offset"],
          "additionalProperties": False,
          "properties": {
            "data": {
              "type": "array",
              "items": {"$ref": "#/components/schemas/DataRow"},
            },
            "limit": {"type": "integer", "minimum": 0, "description": "Effective row limit applied to the query."},
            "offset": {"type": "integer", "minimum": 0, "description": "Row offset applied to the query."},
          },
        },
        "SqlResponse": {
          "type": "object",
          "required": ["data"],
          "additionalProperties": False,
          "properties": {
            "data": {
              "type": "array",
              "items": {"$ref": "#/components/schemas/DataRow"},
            }
          },
        },
        "GeoJsonPosition": {
          "type": "array",
          "description": "GeoJSON `[longitude, latitude]` coordinate pair.",
          "prefixItems": [
            {"type": "number", "minimum": -180, "maximum": 180},
            {"type": "number", "minimum": -90, "maximum": 90},
          ],
          "items": False,
          "minItems": 2,
          "maxItems": 2,
        },
        "LineStringGeometry": {
          "type": "object",
          "required": ["type", "coordinates"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "LineString"},
            "coordinates": {
              "type": "array",
              "minItems": 2,
              "items": {"$ref": "#/components/schemas/GeoJsonPosition"},
            },
          },
        },
        "MultiLineStringGeometry": {
          "type": "object",
          "required": ["type", "coordinates"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "MultiLineString"},
            "coordinates": {
              "type": "array",
              "items": {
                "type": "array",
                "minItems": 2,
                "maxItems": 2,
                "items": {"$ref": "#/components/schemas/GeoJsonPosition"},
              },
            },
          },
        },
        "PointGeometry": {
          "type": "object",
          "required": ["type", "coordinates"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "Point"},
            "coordinates": {"$ref": "#/components/schemas/GeoJsonPosition"},
          },
        },
        "ShapeRouteProperties": {
          "type": "object",
          "required": [
            "kind",
            "route_id",
            "shape_id",
            "geometry_source",
            "route_short_name",
            "route_long_name",
            "route_color",
          ],
          "additionalProperties": False,
          "properties": {
            "kind": {"type": "string", "const": "route"},
            "route_id": {"type": "string"},
            "shape_id": {"type": "string"},
            "trip_ids": {
              "type": "array",
              "items": {"type": "string"},
              "description": "Sorted active `trip_id` values using this `(route_id, shape_id)` shape on unfiltered `/map` requests.",
            },
            "geometry_source": {"type": "string", "const": "shape"},
            "route_short_name": {"type": ["string", "null"]},
            "route_long_name": {"type": ["string", "null"]},
            "route_color": {"type": ["string", "null"]},
          },
        },
        "SegmentRouteProperties": {
          "type": "object",
          "required": [
            "kind",
            "route_id",
            "geometry_source",
            "route_short_name",
            "route_long_name",
            "route_color",
          ],
          "additionalProperties": False,
          "properties": {
            "kind": {"type": "string", "const": "route"},
            "route_id": {"type": "string"},
            "geometry_source": {"type": "string", "const": "stop_sequence"},
            "route_short_name": {"type": ["string", "null"]},
            "route_long_name": {"type": ["string", "null"]},
            "route_color": {"type": ["string", "null"]},
          },
        },
        "StopProperties": {
          "type": "object",
          "required": [
            "kind",
            "route_id",
            "stop_id",
            "stop_name",
            "arrival_times",
          ],
          "additionalProperties": False,
          "properties": {
            "kind": {"type": "string", "const": "stop"},
            "route_id": {"type": "string"},
            "stop_id": {"type": "string"},
            "stop_name": {"type": ["string", "null"]},
            "trip_ids": {
              "type": "array",
              "items": {"type": "string"},
              "description": "Ordered GTFS trip_ids aligned with arrival_times (present on unfiltered /map responses).",
            },
            "arrival_times": {"type": ["string", "null"], "description": "Aggregated arrival times ordered by trip_id and stop_sequence."},
          },
        },
        "ShapeRouteFeature": {
          "type": "object",
          "required": ["type", "geometry", "properties"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "Feature"},
            "geometry": {"$ref": "#/components/schemas/LineStringGeometry"},
            "properties": {"$ref": "#/components/schemas/ShapeRouteProperties"},
          },
        },
        "SegmentRouteFeature": {
          "type": "object",
          "required": ["type", "geometry", "properties"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "Feature"},
            "geometry": {"$ref": "#/components/schemas/MultiLineStringGeometry"},
            "properties": {"$ref": "#/components/schemas/SegmentRouteProperties"},
          },
        },
        "StopFeature": {
          "type": "object",
          "required": ["type", "geometry", "properties"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "Feature"},
            "geometry": {"$ref": "#/components/schemas/PointGeometry"},
            "properties": {"$ref": "#/components/schemas/StopProperties"},
          },
        },
        "MapFeature": {
          "oneOf": [
            {"$ref": "#/components/schemas/ShapeRouteFeature"},
            {"$ref": "#/components/schemas/SegmentRouteFeature"},
            {"$ref": "#/components/schemas/StopFeature"},
          ]
        },
        "MapFeatureCollection": {
          "type": "object",
          "required": ["type", "features"],
          "additionalProperties": False,
          "properties": {
            "type": {"type": "string", "const": "FeatureCollection"},
            "features": {
              "type": "array",
              "items": {"$ref": "#/components/schemas/MapFeature"},
            },
          },
        },
      },
    },
  }


def format_yaml_scalar(value: Any) -> str:
  if value is None:
    return "null"
  if isinstance(value, bool):
    return "true" if value else "false"
  if isinstance(value, (int, float)):
    return str(value)
  if isinstance(value, str):
    if (
      value
      and value.lower() not in YAML_RESERVED
      and not YAML_NUMBER_RE.match(value)
      and not YAML_DATE_RE.match(value)
      and YAML_PLAIN_SAFE_RE.match(value)
      and not value.startswith(("-", "@", "`"))
      and ": " not in value
      and not value.endswith((" ", ":"))
    ):
      return value
    escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("\t", "\\t")
    return f'"{escaped}"'
  raise TypeError(f"Unsupported YAML scalar type: {type(value)!r}")


def format_yaml_key(key: str) -> str:
  if (
    key
    and key.lower() not in YAML_RESERVED
    and not YAML_NUMBER_RE.match(key)
    and re.match(r"^[A-Za-z0-9_.-]+$", key)
  ):
    return key
  escaped = key.replace("\\", "\\\\").replace('"', '\\"')
  return f'"{escaped}"'


def dump_yaml(value: Any, indent: int = 0) -> list[str]:
  prefix = " " * indent
  if isinstance(value, dict):
    if not value:
      return [f"{prefix}{{}}"]
    lines: list[str] = []
    for k, v in value.items():
      key_str = format_yaml_key(str(k))
      if isinstance(v, str) and "\n" in v:
        block_header = "|" if v.endswith("\n") else "|-"
        lines.append(f"{prefix}{key_str}: {block_header}")
        for line in v.rstrip("\n").split("\n"):
          lines.append(f"{' ' * (indent + 2)}{line}" if line else "")
      elif isinstance(v, dict) and v:
        lines.append(f"{prefix}{key_str}:")
        lines.extend(dump_yaml(v, indent + 2))
      elif isinstance(v, list) and v:
        if all(not isinstance(item, (dict, list)) and (not isinstance(item, str) or "\n" not in item) for item in v) and len(v) <= 4:
          inline = ", ".join(format_yaml_scalar(item) for item in v)
          if len(inline) <= 60:
            lines.append(f"{prefix}{key_str}: [{inline}]")
            continue
        lines.append(f"{prefix}{key_str}:")
        lines.extend(dump_yaml(v, indent + 2))
      else:
        scalar = "{}" if isinstance(v, dict) else "[]" if isinstance(v, list) else format_yaml_scalar(v)
        lines.append(f"{prefix}{key_str}: {scalar}")
    return lines
  if isinstance(value, list):
    if not value:
      return [f"{prefix}[]"]
    lines = []
    for item in value:
      if isinstance(item, dict) and item:
        sub = dump_yaml(item, indent + 2)
        lines.append(f"{prefix}- {sub[0].lstrip()}")
        lines.extend(sub[1:])
      elif isinstance(item, list) and item:
        lines.append(f"{prefix}-")
        lines.extend(dump_yaml(item, indent + 2))
      elif isinstance(item, str) and "\n" in item:
        block_header = "|" if item.endswith("\n") else "|-"
        lines.append(f"{prefix}- {block_header}")
        for line in item.rstrip("\n").split("\n"):
          lines.append(f"{' ' * (indent + 2)}{line}" if line else "")
      else:
        scalar = "{}" if isinstance(item, dict) else "[]" if isinstance(item, list) else format_yaml_scalar(item)
        lines.append(f"{prefix}- {scalar}")
    return lines
  return [f"{prefix}{format_yaml_scalar(value)}"]


def main() -> None:
  spec = build_openapi_spec()
  header = (
    "# AUTO-GENERATED from providers.toml, migrations/*/0_gtfs_schema.sql, Cargo.toml, and wrangler.toml — do not edit directly.\n"
    "# Regenerate with: python3 scripts/generate-openapi.py\n"
  )
  yaml_body = "\n".join(dump_yaml(spec)) + "\n"
  OUTPUT_PATH.write_text(header + yaml_body, encoding="utf-8")
  provider_count = len(spec["components"]["parameters"]["Provider"]["schema"]["enum"])
  table_count = len(spec["components"]["parameters"]["Table"]["schema"]["enum"])
  print(f"Generated {OUTPUT_PATH} ({provider_count} providers, {table_count} tables)")


if __name__ == "__main__":
  main()
