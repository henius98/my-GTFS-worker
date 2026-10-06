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

test("bypasses cached results while a feed import is in progress", async () => {
  const first = await request(`?stop_id=S&at=${at}`);
  assert.equal(first.headers.get("X-Departure-Cache"), "MISS");
  const { DB_KTMB: db } = await worker.getEnv();
  await db
    .prepare(
      "INSERT INTO import_progress (Provider, FileName, CRC, LastProcessedLine, Status) VALUES (?, ?, ?, ?, ?)"
    )
    .bind("ktmb", "trips.txt", "changed", 1, 1)
    .run();

  const second = await request(`?stop_id=S&at=${at}`);
  assert.equal(second.status, 200);
  assert.equal(second.headers.get("X-Departure-Cache"), "BYPASS");
  assert.equal((await second.json()).departures[0].trip_id, "early");
});

test("reports unknown stops and unbound providers", async () => {
  const stop = await request(`?stop_id=unknown&at=${at}`);
  assert.equal(stop.status, 404);
  assert.deepEqual(await stop.json(), { error: "Unknown stop_id" });

  const provider = await worker.fetch(`/unknown/departures?stop_id=S&at=${at}`);
  assert.equal(provider.status, 404);
});
