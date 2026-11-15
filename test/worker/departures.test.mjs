import assert from "node:assert/strict";
import { after, afterEach, before, beforeEach, test } from "node:test";
import { createTestHarness } from "wrangler";

const server = createTestHarness({
  workers: [{ configPath: "./test/worker/wrangler.jsonc" }]
});
const worker = server.getWorker("my-gtfs-worker-test");
const at = "2026-09-24T00:00:00Z";

async function request(query, options) {
  return worker.fetch(`/ktmb/departures${query}`, options);
}

before(async () => {
  await server.listen();
});

beforeEach(async () => {
  await worker.applyD1Migrations("DB_KTMB");
  const { DB_KTMB: db } = await worker.getEnv();
  await db.batch([
    db
      .prepare("INSERT INTO agency (agency_id, agency_timezone) VALUES (?, ?)")
      .bind("A", "Asia/Kuala_Lumpur"),
    db
      .prepare("INSERT INTO routes (route_id, agency_id) VALUES (?, ?)")
      .bind("R", "A"),
    db
      .prepare("INSERT INTO stops (stop_id, stop_name) VALUES (?, ?)")
      .bind("S", "Test stop"),
    db
      .prepare(
        "INSERT INTO trips (trip_id, route_id, service_id, direction_id) VALUES (?, ?, ?, ?)"
      )
      .bind("early", "R", "daily", 0),
    db
      .prepare(
        "INSERT INTO trips (trip_id, route_id, service_id, direction_id) VALUES (?, ?, ?, ?)"
      )
      .bind("late", "R", "daily", 1),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("early", "S", 1, "08:09:00", "08:10:00"),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("late", "S", 1, "08:19:00", "08:20:00"),
    db
      .prepare(
        "INSERT INTO calendar (service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
      )
      .bind("daily", 1, 1, 1, 1, 1, 1, 1, 20260924, 20260925)
  ]);
});

afterEach(async () => {
  await server.reset();
});

after(async () => {
  await server.close();
});

test("validates departure requests through HTTP", async () => {
  for (const query of [
    "",
    "?stop_id=",
    "?stop_id=S&limit=0",
    "?stop_id=S&limit=-1",
    "?stop_id=S&limit=101",
    "?stop_id=S&limit=1.5",
    "?stop_id=S&limit=",
    "?stop_id=S&direction_id=2",
    "?stop_id=S&at=2026-09-24T08:00:00",
    "?stop_id=S&stop_id=T",
    "?stop_id=S&api_key=secret",
    "?stop_id=S&route_id="
  ]) {
    const response = await request(query);
    assert.equal(response.status, 400, query);
    assert.equal(typeof (await response.json()).error, "string", query);
  }
  const method = await request("?stop_id=S", { method: "POST" });
  assert.equal(method.status, 405);
  assert.deepEqual(await method.json(), { error: "Method not allowed" });

  const valid = await request(
    "?stop_id=S&route_id=R&direction_id=1&limit=100&at=2026-09-24T08:00:00%2B08:00"
  );
  assert.equal(valid.status, 200);
  const body = await valid.json();
  assert.equal(body.route_id, "R");
  assert.equal(body.direction_id, 1);
  assert.equal(body.limit, 100);
  assert.equal(body.requested_at, at);
});

test("returns scheduled departures with route and direction filters", async () => {
  const response = await request(
    `?stop_id=S&route_id=R&direction_id=1&limit=1&at=${at}`
  );
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("X-Departure-Cache"), "MISS");
  const body = await response.json();
  assert.equal(body.stop.stop_name, "Test stop");
  assert.equal(body.route_id, "R");
  assert.equal(body.direction_id, 1);
  assert.equal(body.limit, 1);
  assert.equal(body.search_until, "2026-10-01T00:00:00Z");
  assert.equal(body.is_estimate, true);
  assert.equal(body.realtime, false);
  assert.equal(body.departures.length, 1);
  assert.equal(body.departures[0].trip_id, "late");
  assert.equal(body.departures[0].estimate_method, "gtfs_schedule");
  assert.equal(body.departures[0].wait_seconds, 20 * 60);
  assert.equal(
    body.departures[0].estimated_departure_at,
    "2026-09-24T08:20:00+08:00"
  );
});

test("reuses cached departures and recalculates waits", async () => {
  const first = await request(`?stop_id=S&limit=2&at=${at}`);
  assert.equal(first.status, 200);
  assert.equal(first.headers.get("X-Departure-Cache"), "MISS");
  assert.deepEqual(
    (await first.json()).departures.map((row) => row.trip_id),
    ["early", "late"]
  );

  const second = await request("?stop_id=S&limit=1&at=2026-09-24T00:10:01Z");
  assert.equal(second.status, 200);
  assert.equal(second.headers.get("X-Departure-Cache"), "HIT");
  const body = await second.json();
  assert.equal(body.departures[0].trip_id, "late");
  assert.equal(body.departures[0].wait_seconds, 9 * 60 + 59);
});

test("invalidates and recaches departures when feed progress changes", async () => {
  const first = await request(`?stop_id=S&at=${at}`);
  assert.equal(first.headers.get("X-Departure-Cache"), "MISS");
  const { DB_KTMB: db } = await worker.getEnv();
  await db
    .prepare(
      "INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, Status) VALUES (?, ?, ?, ?)"
    )
    .bind("trips.txt", "changed", 1, 1)
    .run();

  const second = await request(`?stop_id=S&at=${at}`);
  assert.equal(second.status, 200);
  assert.equal(second.headers.get("X-Departure-Cache"), "MISS");
  assert.equal((await second.json()).departures[0].trip_id, "early");

  const secondCached = await request(`?stop_id=S&at=${at}`);
  assert.equal(secondCached.status, 200);
  assert.equal(secondCached.headers.get("X-Departure-Cache"), "HIT");
});

test("reports unknown stops and unbound providers", async () => {
  const stop = await request(`?stop_id=unknown&at=${at}`);
  assert.equal(stop.status, 404);
  assert.deepEqual(await stop.json(), { error: "Unknown stop_id" });

  const provider = await worker.fetch(`/unknown/departures?stop_id=S&at=${at}`);
  assert.equal(provider.status, 404);
});

async function seedRapidBusKl() {
  await worker.applyD1Migrations("DB_RAPID_BUS_KL");
  const { DB_RAPID_BUS_KL: db } = await worker.getEnv();
  await db.batch([
    db
      .prepare("INSERT INTO agency (agency_id, agency_timezone) VALUES (?, ?)")
      .bind("RKL", "Asia/Kuala_Lumpur"),
    db
      .prepare(
        "INSERT INTO routes (route_id, agency_id, route_short_name, route_long_name, route_color) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("U1", "RKL", "U1", "Test Line", "E51B24"),
    db
      .prepare(
        "INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES (?, ?, ?, ?)"
      )
      .bind("S1", "First Stop", 3.1, 101.6),
    db
      .prepare(
        "INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES (?, ?, ?, ?)"
      )
      .bind("S2", "Second Stop", 3.2, 101.7),
    db
      .prepare(
        "INSERT INTO shapes (shape_id, shape_pt_lat, shape_pt_lon, shape_pt_sequence) VALUES (?, ?, ?, ?)"
      )
      .bind("SH1", 3.1, 101.6, 1),
    db
      .prepare(
        "INSERT INTO shapes (shape_id, shape_pt_lat, shape_pt_lon, shape_pt_sequence) VALUES (?, ?, ?, ?)"
      )
      .bind("SH1", 3.2, 101.7, 2),
    db
      .prepare(
        "INSERT INTO calendar (service_id, monday, tuesday, wednesday, thursday, friday, saturday, sunday, start_date, end_date) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
      )
      .bind("daily", 1, 1, 1, 1, 1, 1, 1, 20260924, 20260925),
    db
      .prepare(
        "INSERT INTO trips (trip_id, route_id, service_id, shape_id, trip_headsign, direction_id) VALUES (?, ?, ?, ?, ?, ?)"
      )
      .bind("freq-headway", "U1", "daily", null, "Headway", 0),
    db
      .prepare(
        "INSERT INTO trips (trip_id, route_id, service_id, shape_id, trip_headsign, direction_id) VALUES (?, ?, ?, ?, ?, ?)"
      )
      .bind("freq-exact", "U1", "daily", "SH1", "Exact", 1),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("freq-headway", "S1", 1, "06:00:00", "06:00:00"),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("freq-headway", "S2", 2, "06:05:00", "06:05:00"),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("freq-exact", "S1", 1, "06:00:00", "06:00:00"),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("freq-exact", "S2", 2, "06:03:00", "06:03:00"),
    db
      .prepare(
        "INSERT INTO frequencies (trip_id, start_time, end_time, headway_secs, exact_times) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("freq-headway", "08:00:00", "08:30:00", 600, 0),
    db
      .prepare(
        "INSERT INTO frequencies (trip_id, start_time, end_time, headway_secs, exact_times) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("freq-exact", "08:00:00", "08:30:00", 600, 1)
  ]);
}

async function seedKtmbProgress(rows) {
  const { DB_KTMB: db } = await worker.getEnv();
  await db.batch(
    rows.map(([file, crc, line, byte, status]) =>
      db
        .prepare(
          "INSERT INTO \"0_import_progress\" (FileName, CRC, LastProcessedLine, LastProcessedByte, Status) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(file, crc, line, byte, status)
    )
  );
}

test("serves status rows and validates method and provider", async () => {
  const method = await worker.fetch("/ktmb/status", { method: "POST" });
  assert.equal(method.status, 405);
  assert.equal((await worker.fetch("/unknown/status")).status, 404);

  await seedKtmbProgress([
    ["stops.txt", "crc1", 10, 200, 0],
    ["trips.txt", "crc2", 5, 100, 1]
  ]);

  const response = await worker.fetch("/KTMB/status/?unused=1");
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("Cache-Control"), "public, max-age=60");
  const rows = (await response.json()).sort((a, b) =>
    a.FileName.localeCompare(b.FileName)
  );
  assert.equal(rows.length, 2);
  assert.equal(rows[0].FileName, "stops.txt");
  assert.equal(rows[0].CRC, "crc1");
  assert.equal(rows[0].LastProcessedLine, 10);
  assert.equal(rows[0].LastProcessedByte, 200);
  assert.equal(rows[0].Status, 0);
});

test("queries seeded /data endpoint deterministically", async () => {
  await seedKtmbProgress([
    ["stops.txt", "abc123", 12, 240, 0],
    ["trips.txt", "def456", 5, 100, 1]
  ]);

  const exact = await worker.fetch(
    "/ktmb/data/0_import_progress?filter=1@Status"
  );
  assert.equal(exact.status, 200);
  assert.deepEqual(
    (await exact.json()).data.map((row) => row.FileName),
    ["trips.txt"]
  );

  const icontains = await worker.fetch(
    "/ktmb/data/0_import_progress?icontains=OPS.T@FileName"
  );
  assert.equal(icontains.status, 200);
  assert.deepEqual(
    (await icontains.json()).data.map((row) => row.FileName),
    ["stops.txt"]
  );

  const contains = await worker.fetch(
    "/ktmb/data/0_import_progress?contains=trips@FileName"
  );
  assert.equal(contains.status, 200);
  assert.deepEqual(
    (await contains.json()).data.map((row) => row.FileName),
    ["trips.txt"]
  );

  const ranged = await worker.fetch(
    "/ktmb/data/0_import_progress?range=LastProcessedLine[10:20]&sort=-Status"
  );
  assert.equal(ranged.status, 200);
  assert.deepEqual(
    (await ranged.json()).data.map((row) => row.FileName),
    ["stops.txt"]
  );

  const projected = await worker.fetch(
    "/ktmb/data/0_import_progress?include=CRC,Status"
  );
  assert.equal(projected.status, 200);
  const projectedRows = (await projected.json()).data;
  assert.equal(projectedRows.length, 2);
  assert.equal("FileName" in projectedRows[0], false);
  assert.equal(typeof projectedRows[0].CRC, "string");
});

test("returns map GeoJSON from stop_sequence fallback and shapes", async () => {
  assert.equal(
    (await worker.fetch("/ktmb/map?route_id=R", { method: "POST" })).status,
    405
  );
  assert.equal((await worker.fetch("/ktmb/map")).status, 400);
  assert.equal((await worker.fetch("/ktmb/map?route_id=missing")).status, 404);

  const { DB_KTMB: db } = await worker.getEnv();
  await db.batch([
    db
      .prepare(
        "UPDATE routes SET route_short_name = ?, route_color = ? WHERE route_id = ?"
      )
      .bind("KTM1", "112233", "R"),
    db
      .prepare(
        "UPDATE stops SET stop_lat = ?, stop_lon = ? WHERE stop_id = ?"
      )
      .bind(3.139, 101.686, "S"),
    db
      .prepare(
        "INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES (?, ?, ?, ?)"
      )
      .bind("S2", "Second", 3.149, 101.696),
    db
      .prepare(
        "INSERT INTO stop_times (trip_id, stop_id, stop_sequence, arrival_time, departure_time) VALUES (?, ?, ?, ?, ?)"
      )
      .bind("early", "S2", 2, "08:14:00", "08:15:00")
  ]);

  const ktmbMap = await worker.fetch("/ktmb/map?route_id=R");
  assert.equal(ktmbMap.status, 200);
  assert.equal(ktmbMap.headers.get("Cache-Control"), "public, max-age=86400");
  const ktmbBody = await ktmbMap.json();
  assert.equal(ktmbBody.type, "FeatureCollection");
  const fallbackRoute = ktmbBody.features.find(
    (feature) => feature.properties.kind === "route"
  );
  assert.equal(fallbackRoute.properties.geometry_source, "stop_sequence");
  assert.deepEqual(fallbackRoute.geometry.coordinates, [
    [101.686, 3.139],
    [101.696, 3.149]
  ]);
  assert.equal(
    ktmbBody.features.filter((feature) => feature.properties.kind === "stop")
      .length,
    2
  );

  await seedRapidBusKl();
  const shapeMap = await worker.fetch("/rapid-bus-kl/map?route_id=U1");
  assert.equal(shapeMap.status, 200);
  const shapeBody = await shapeMap.json();
  const shapeRoute = shapeBody.features.find(
    (feature) => feature.properties.kind === "route"
  );
  assert.equal(shapeRoute.properties.geometry_source, "shape");
  assert.deepEqual(shapeRoute.geometry.coordinates, [
    [101.6, 3.1],
    [101.7, 3.2]
  ]);
});

test("computes frequency departures with exact_times 0 and 1", async () => {
  await seedRapidBusKl();
  const response = await worker.fetch(
    `/rapid-bus-kl/departures?stop_id=S2&limit=4&at=${at}`
  );
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.ok(body.departures.length >= 2);
  assert.equal(body.departures[0].trip_id, "freq-exact");
  assert.equal(body.departures[0].estimate_method, "gtfs_schedule");
  assert.equal(
    body.departures[0].estimated_departure_at,
    "2026-09-24T08:03:00+08:00"
  );
  assert.equal(body.departures[0].headway_secs, 600);
  assert.equal(body.departures[1].trip_id, "freq-headway");
  assert.equal(
    body.departures[1].estimate_method,
    "frequency_start_plus_headway"
  );
  assert.equal(
    body.departures[1].estimated_departure_at,
    "2026-09-24T08:05:00+08:00"
  );
  assert.equal(body.departures[1].headway_secs, 600);
});
