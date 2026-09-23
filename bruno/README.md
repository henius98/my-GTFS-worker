# Worker API collection

Open this directory as a collection in Bruno and select the `Local` environment.
Its `baseUrl` is `http://localhost:8787`. No authentication is required.

- **Worker health**: `GET /`, returning a plain-text liveness message.
- **Status**: `GET /<provider>/status` for every active provider in `providers.toml`.
  Responses contain import progress records and are cached for 60 seconds.

The local Worker must be running and have its D1 databases and migrations
configured before status requests can succeed. The health request does not
require a database.

After changing providers, regenerate status requests from the repository root
using Python 3.11 or newer:

```sh
python3 scripts/generate-bruno.py
```

Generated status requests are replaced on regeneration; obsolete generated
requests are removed. Keep custom requests under separate filenames.

These are all routes currently exposed by `worker/src/lib.rs`. The importer is
a CLI and does not expose HTTP endpoints.
