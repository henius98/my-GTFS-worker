# Worker API collection

Open this directory as a collection in Bruno. Select `Local` for
`http://localhost:8787` or `Production` for the deployed Worker. Public `GET`
endpoints require no authentication; `POST /<provider>/sql` requires
`WORKER_ADMIN_KEY` via `Authorization: Bearer <key>` or `X-Admin-Key: <key>`
(unless `WORKER_ALLOW_ANONYMOUS_SQL=true` is set locally). Edit the example
`provider`, `table`, `stopId`, and `routeId` values in `collection.bru` to
explore other GTFS data.

- **Worker health**: `GET /`, returning a plain-text liveness message.
- **Status**: `GET /<provider>/status` for every active provider in `providers.toml`.
  Responses contain `0_import_progress` records (`Status`: `0 = COMPLETED`,
  `1 = IN_PROGRESS`, `2 = STAGED`) and are edge-cached for `WORKER_API_CACHE_TTL_SECONDS`
  (60 seconds in `.env.example`).
- **Data**: `GET /<provider>/data/<table>` returns paginated GTFS rows. The
  request includes optional examples for column selection, filters, range bounds, and sort.
- **SQL**: `POST /<provider>/sql` accepts a single read-only `SELECT` (or
  `WITH ... SELECT`) query as plain text. Protected by `WORKER_ADMIN_KEY`.
- **Departures**: `GET /<provider>/departures` estimates upcoming departures at
  a required `stop_id`; `route_id`, `direction_id`, `limit`, and `at` filters are optional.
- **Map**: `GET /<provider>/map` returns GeoJSON for a full network or one route,
  edge-cached for `WORKER_FEED_CACHE_TTL_SECONDS` (86400 seconds in `.env.example`).
- **Original GTFS**: `GET https://api.data.gov.my/gtfs-static/...` downloads the
  raw upstream GTFS static ZIP archive for every active provider in `providers.toml`.
  In Bruno **Developer Mode** (shield icon at top-right), the post-response script
  automatically extracts all `.csv` files to `bruno/Original GTFS/extracted/<provider>/`.

The local Worker must be running and have its D1 databases and migrations
configured before provider requests can succeed. The health and original GTFS requests
do not require a database.

After changing providers, regenerate status and original GTFS requests from the repository root
using Python 3.11 or newer:

```sh
python3 scripts/generate-bruno.py
```

Generated status and original GTFS requests are replaced on regeneration; obsolete generated
requests are removed. Keep custom requests under separate filenames.

The collection covers the routes in `worker/src/lib.rs` alongside upstream GTFS static feeds. The importer is a CLI
and does not expose HTTP endpoints.
