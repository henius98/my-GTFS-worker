import assert from "node:assert/strict";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";
import { execFileSync, spawn } from "node:child_process";
import { after, before, test } from "node:test";
import { unstable_startWorker } from "wrangler";
import { applyLocalD1Migrations, startLocalD1Bridge } from "../../scripts/local-d1-importer.mjs";

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function buildZip(entries) {
  const localParts = [];
  const centralParts = [];
  let offset = 0;

  for (const [name, content] of entries) {
    const nameBuf = Buffer.from(name, "utf8");
    const rawBuf = Buffer.isBuffer(content) ? content : Buffer.from(content, "utf8");
    const compressed = zlib.deflateRawSync(rawBuf);
    const crc = crc32(rawBuf);

    const localHeader = Buffer.alloc(30);
    localHeader.writeUInt32LE(0x04034b50, 0);
    localHeader.writeUInt16LE(20, 4);
    localHeader.writeUInt16LE(0, 6);
    localHeader.writeUInt16LE(8, 8);
    localHeader.writeUInt16LE(0, 10);
    localHeader.writeUInt16LE(0, 12);
    localHeader.writeUInt32LE(crc, 14);
    localHeader.writeUInt32LE(compressed.length, 18);
    localHeader.writeUInt32LE(rawBuf.length, 22);
    localHeader.writeUInt16LE(nameBuf.length, 26);
    localHeader.writeUInt16LE(0, 28);

    localParts.push(localHeader, nameBuf, compressed);

    const centralHeader = Buffer.alloc(46);
    centralHeader.writeUInt32LE(0x02014b50, 0);
    centralHeader.writeUInt16LE(20, 4);
    centralHeader.writeUInt16LE(20, 6);
    centralHeader.writeUInt16LE(0, 8);
    centralHeader.writeUInt16LE(8, 10);
    centralHeader.writeUInt16LE(0, 12);
    centralHeader.writeUInt16LE(0, 14);
    centralHeader.writeUInt32LE(crc, 16);
    centralHeader.writeUInt32LE(compressed.length, 20);
    centralHeader.writeUInt32LE(rawBuf.length, 24);
    centralHeader.writeUInt16LE(nameBuf.length, 28);
    centralHeader.writeUInt16LE(0, 30);
    centralHeader.writeUInt16LE(0, 32);
    centralHeader.writeUInt16LE(0, 34);
    centralHeader.writeUInt16LE(0, 36);
    centralHeader.writeUInt32LE(0, 38);
    centralHeader.writeUInt32LE(offset, 42);

    centralParts.push(centralHeader, nameBuf);
    offset += localHeader.length + nameBuf.length + compressed.length;
  }

  const centralDir = Buffer.concat(centralParts);
  const endRecord = Buffer.alloc(22);
  endRecord.writeUInt32LE(0x06054b50, 0);
  endRecord.writeUInt16LE(0, 4);
  endRecord.writeUInt16LE(0, 6);
  endRecord.writeUInt16LE(entries.length, 8);
  endRecord.writeUInt16LE(entries.length, 10);
  endRecord.writeUInt32LE(centralDir.length, 12);
  endRecord.writeUInt32LE(offset, 16);
  endRecord.writeUInt16LE(0, 20);

  return Buffer.concat([...localParts, centralDir, endRecord]);
}

function buildKtmbFixtureFeed() {
  const stopTimeLines = ["trip_id,arrival_time,departure_time,stop_id,stop_sequence,shape_dist_traveled"];
  for (let seq = 1; seq <= 120; seq += 1) {
    const minutes = String(seq % 60).padStart(2, "0");
    const hours = String(8 + Math.floor(seq / 60)).padStart(2, "0");
    stopTimeLines.push(`T1,${hours}:${minutes}:00,${hours}:${minutes}:00,S1,${seq},${seq * 100}`);
  }
  return buildZip([
    ["agency.txt", "agency_id,agency_name,agency_url,agency_timezone,agency_phone,agency_lang\nKTMB,KTM Berhad,https://ktmb.example,Asia/Kuala_Lumpur,03-2267,en\n"],
    ["calendar.txt", "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nDAILY,1,1,1,1,1,1,1,20260924,20260930\n"],
    ["calendar_dates.txt", "service_id,date,exception_type\nDAILY,20260925,1\n"],
    ["routes.txt", "agency_id,route_id,route_type,route_short_name,route_long_name,route_desc,route_url,route_color,route_text_color\nKTMB,R1,2,KTM1,Main Line,,,112233,FFFFFF\n"],
    ["stops.txt", "stop_id,stop_name,stop_lat,stop_lon\nS1,KL Sentral,3.134,101.686\nS2,Mid Valley,3.118,101.678\n"],
    ["trips.txt", "route_id,service_id,trip_id,direction_id\nR1,DAILY,T1,0\n"],
    ["stop_times.txt", `${stopTimeLines.join("\n")}\n`],
  ]);
}

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "gtfs-importer-local-d1-"));
const persistDir = path.join(tempDir, "persist");
const providersPath = path.join(tempDir, "providers.toml");
const wranglerConfigPath = path.resolve("test/worker/wrangler.jsonc");
const importerBin = path.resolve("target/debug/importer");
const ktmbDbId = "00000000-0000-0000-0000-000000000001";

let feedServer;
let feedRequests = [];
let currentEtag = '"ktmb-local-v1"';
let currentZip = buildKtmbFixtureFeed();
let bridge;

function runImporter(extraEnv) {
  return new Promise((resolve, reject) => {
    const env = {
      ...process.env,
      CLOUDFLARE_ACCOUNT_ID: "local-account",
      CLOUDFLARE_API_TOKEN: "local-token",
      D1_API_BASE_URL: bridge.baseUrl,
      PROVIDERS_CONFIG: providersPath,
      D1_CONCURRENCY_LIMIT: "4",
      D1_MAX_DATABASES: "10",
      D1_STATEMENTS_PER_REQUEST: "2",
      D1_DAILY_WRITE_LIMIT: "20000",
      MAX_ROWS_PER_WORKFLOW: "50000",
      MAX_FEED_DOWNLOAD_MB: "16",
      MAX_UNCOMPRESSED_FEED_MB: "32",
      MAX_FEED_ARCHIVE_ENTRIES: "64",
      MAX_CSV_RECORD_KB: "64",
      MAX_STATEMENT_PAYLOAD_KB: "256",
      MAX_TEMP_FEED_STORAGE_MB: "32",
      QUERY_STATEMENT_BATCH_SIZE: "20",
      DB_SIZE_THRESHOLD_MB: "490",
      HTTP_CONNECT_TIMEOUT_SECS: "5",
      HTTP_REQUEST_TIMEOUT_SECS: "30",
      D1_QUERY_TIMEOUT_SECS: "15",
      D1_MAX_QUERY_RETRIES: "2",
      D1_MAX_RETRY_DELAY_SECS: "5",
      ...extraEnv,
    };
    const child = spawn(importerBin, [], { cwd: process.cwd(), env, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, stdout, stderr }));
  });
}

before(async () => {
  execFileSync("cargo", ["build", "-p", "importer"], { cwd: process.cwd(), stdio: "inherit" });

  feedServer = http.createServer((req, res) => {
    feedRequests.push({ url: req.url, ifNoneMatch: req.headers["if-none-match"] ?? null });
    if (req.headers["if-none-match"] === currentEtag) {
      res.writeHead(304, { ETag: currentEtag });
      res.end();
      return;
    }
    res.writeHead(200, {
      "Content-Type": "application/zip",
      "Content-Length": currentZip.length,
      ETag: currentEtag,
    });
    res.end(currentZip);
  });
  await new Promise((resolve) => feedServer.listen(0, "127.0.0.1", resolve));
  const { port: feedPort } = feedServer.address();

  fs.writeFileSync(
    providersPath,
    `budget_provider = "ktmb"\n\n[[providers]]\nname = "ktmb"\nis_active = true\nstatic_url = "http://127.0.0.1:${feedPort}/"\nstatic_provider = "ktmb"\ndatabase_id = "${ktmbDbId}"\n`,
    "utf8",
  );

  fs.mkdirSync(persistDir, { recursive: true });
  applyLocalD1Migrations({ configPath: wranglerConfigPath, persistDir, bindings: ["DB_KTMB"], quiet: true });
  bridge = await startLocalD1Bridge({ configPath: wranglerConfigPath, persistDir, quiet: true });

  // Seed a legacy row that must survive until all staged tables swap atomically.
  const { db } = bridge.databases.get(ktmbDbId);
  await db.prepare("INSERT INTO stops (stop_id, stop_name, stop_lat, stop_lon) VALUES (?, ?, ?, ?)").bind("LEGACY", "Old Stop", 3.0, 101.0).run();
});

after(async () => {
  if (bridge) {
    await bridge.close();
  }
  if (feedServer) {
    await new Promise((resolve) => feedServer.close(resolve));
  }
  fs.rmSync(tempDir, { recursive: true, force: true });
});

test("importer advances contiguous checkpoints under tight write budget, resumes to atomic swap, skips on 304, and serves via Worker", async () => {
  const { db } = bridge.databases.get(ktmbDbId);
  const initialLiveStops = (await db.prepare("SELECT stop_id FROM stops ORDER BY stop_id").all()).results.map((row) => row.stop_id);
  assert.ok(initialLiveStops.includes("LEGACY"));

  // Run 1: Tight daily write budget (2000 metadata + 2 ledger + 88 data writes = 2090).
  // stop_times.txt has 120 rows in 20-row batches; ordered budget reservation must advance
  // stop_times.txt contiguously (LastProcessedLine > 0, not stalled at 0) without swapping live tables.
  const run1 = await runImporter({ D1_DAILY_WRITE_LIMIT: "2090" });
  assert.equal(run1.code, 0, `Run 1 failed:\n${run1.stdout}\n${run1.stderr}`);
  assert.match(run1.stdout, /write budget exhausted: true/);

  const liveStopsAfterRun1 = await db.prepare("SELECT stop_id FROM stops ORDER BY stop_id").all();
  assert.deepEqual(
    liveStopsAfterRun1.results.map((row) => row.stop_id),
    initialLiveStops,
    "live stops must remain untouched until every file completes staging",
  );

  const stopTimesProg1 = await db
    .prepare('SELECT LastProcessedLine, LastProcessedByte, Status FROM "0_import_progress" WHERE FileName = ?')
    .bind("stop_times.txt")
    .all();
  assert.equal(stopTimesProg1.results.length, 1);
  assert.equal(stopTimesProg1.results[0].Status, 1);
  assert.ok(stopTimesProg1.results[0].LastProcessedLine > 0, `expected contiguous checkpoint > 0, got ${JSON.stringify(stopTimesProg1.results[0])}`);
  assert.ok(stopTimesProg1.results[0].LastProcessedByte > 0, "expected byte checkpoint > 0");

  // Run 2: Raise daily limit to resume from the saved byte checkpoint and commit atomic swap.
  const run2 = await runImporter({ D1_DAILY_WRITE_LIMIT: "20000" });
  assert.equal(run2.code, 0, `Run 2 failed:\n${run2.stdout}\n${run2.stderr}`);
  assert.match(run2.stdout, /Committing atomic swap for 7 table\(s\)/);
  assert.match(run2.stdout, /0 provider\(s\) remain incomplete; write budget exhausted: false/);
  assert.match(run2.stdout, new RegExp(`Importing stop_times\\.txt into stop_times__staging \\(resuming from row ${stopTimesProg1.results[0].LastProcessedLine}, byte ${stopTimesProg1.results[0].LastProcessedByte}\\)`));

  const liveStopsAfterRun2 = await db.prepare("SELECT stop_id FROM stops ORDER BY stop_id").all();
  assert.deepEqual(
    liveStopsAfterRun2.results.map((row) => row.stop_id),
    ["S1", "S2"],
  );
  const stopTimesCount = await db.prepare("SELECT COUNT(*) AS c FROM stop_times").all();
  assert.equal(stopTimesCount.results[0].c, 120);

  const leftoverStaging = await db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name LIKE '%__staging'").all();
  assert.equal(leftoverStaging.results.length, 0, "all staging tables must be swapped or removed");

  const versionRows = await db.prepare('SELECT ETag FROM "0_dataset_versions" ORDER BY rowid DESC LIMIT 1').all();
  assert.equal(versionRows.results[0]?.ETag, currentEtag);

  // Run 3: Unchanged ETag -> 304 Not Modified -> skips with 0 rows processed.
  const requestsBeforeRun3 = feedRequests.length;
  const run3 = await runImporter({ D1_DAILY_WRITE_LIMIT: "20000" });
  assert.equal(run3.code, 0, `Run 3 failed:\n${run3.stdout}\n${run3.stderr}`);
  assert.match(run3.stdout, /Feed unchanged and all 7 files are complete\. Skipping\./);
  assert.match(run3.stdout, /processed 0 of 50000 rows/);
  assert.equal(feedRequests.length, requestsBeforeRun3 + 1);
  assert.equal(feedRequests.at(-1).ifNoneMatch, currentEtag);

  // Close the importer bridge before starting the Worker against the same local D1 state.
  await bridge.close();
  bridge = null;

  const worker = await unstable_startWorker({
    config: wranglerConfigPath,
    dev: { persist: persistDir, remote: false, logLevel: "error" },
  });
  try {
    const statusRes = await worker.fetch("http://local/ktmb/status");
    assert.equal(statusRes.status, 200);
    const statusBody = await statusRes.json();
    assert.equal(statusBody.length, 7);
    assert.ok(statusBody.every((row) => row.Status === 0));

    const stopsRes = await worker.fetch("http://local/ktmb/data/stops");
    assert.equal(stopsRes.status, 200);
    const stopsBody = await stopsRes.json();
    assert.deepEqual(
      stopsBody.data.map((row) => row.stop_id).sort(),
      ["S1", "S2"],
    );

    const depRes = await worker.fetch("http://local/ktmb/departures?stop_id=S1&limit=3&at=2026-09-24T00:00:00Z");
    assert.equal(depRes.status, 200);
    const depBody = await depRes.json();
    assert.equal(depBody.departures.length, 3);
    assert.equal(depBody.departures[0].trip_id, "T1");

    const mapRes = await worker.fetch("http://local/ktmb/map?route_id=R1");
    assert.equal(mapRes.status, 200);
    const mapBody = await mapRes.json();
    assert.equal(mapBody.type, "FeatureCollection");
  } finally {
    await worker.dispose();
  }
});
