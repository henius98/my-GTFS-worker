# Worker API collection

Open this directory as a collection in Bruno. Select `Local` for
`http://localhost:8787` or `Production` for the deployed Worker. No
authentication is required. Edit the example `provider`, `table`, `stopId`,
and `routeId` values in `collection.bru` to explore other GTFS data.

- **Worker health**: `GET /`, returning a plain-text liveness message.
- **Status**: `GET /<provider>/status` for every active provider in `providers.toml`.
  Responses contain import progress records and are cached for 60 seconds.
- **Data**: `GET /<provider>/data/<table>` returns paginated GTFS rows. The
  request includes optional examples for column selection, filters, and sort.
- **SQL**: `POST /<provider>/sql` accepts a single read-only SQL query as plain text.
- **Departures**: `GET /<provider>/departures` estimates upcoming departures at
  a required `stop_id`; route, direction, limit, and time filters are optional.
- **Map**: `GET /<provider>/map` returns GeoJSON for a full network or one route.

The local Worker must be running and have its D1 databases and migrations
configured before provider requests can succeed. The health request does not
require a database.

After changing providers, regenerate status requests from the repository root
using Python 3.11 or newer:

```sh
python3 scripts/generate-bruno.py
```

Generated status requests are replaced on regeneration; obsolete generated
requests are removed. Keep custom requests under separate filenames.

The collection covers the routes in `worker/src/lib.rs`. The importer is a CLI
and does not expose HTTP endpoints.
