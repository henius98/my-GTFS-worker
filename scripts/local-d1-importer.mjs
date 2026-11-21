#!/usr/bin/env node
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { execFileSync, spawn } from "node:child_process";
import { pathToFileURL } from "node:url";
import { getPlatformProxy } from "wrangler";

function stripJsonComments(text) {
  return text.replace(/^\s*\/\/.*$/gm, "").replace(/\/\*[\s\S]*?\*\//g, "");
}

export function parseWranglerDatabases(configPath) {
  const raw = fs.readFileSync(configPath, "utf8");
  if (configPath.endsWith(".json") || configPath.endsWith(".jsonc")) {
    const parsed = JSON.parse(stripJsonComments(raw));
    return (parsed.d1_databases || [])
      .filter((entry) => entry.binding && entry.database_name && entry.database_id)
      .map((entry) => ({
        binding: entry.binding,
        databaseName: entry.database_name,
        databaseId: entry.database_id,
      }));
  }
  const entries = [];
  for (const block of raw.split("[[d1_databases]]").slice(1)) {
    const binding = block.match(/binding\s*=\s*"([^"]+)"/)?.[1];
    const databaseName = block.match(/database_name\s*=\s*"([^"]+)"/)?.[1];
    const databaseId = block.match(/database_id\s*=\s*"([^"]+)"/)?.[1];
    if (binding && databaseName && databaseId) {
      entries.push({ binding, databaseName, databaseId });
    }
  }
  return entries;
}

export function applyLocalD1Migrations({ configPath, persistDir, bindings, quiet = false }) {
  const databases = parseWranglerDatabases(configPath);
  const selected = bindings ? databases.filter((entry) => bindings.includes(entry.binding)) : databases;
  for (const entry of selected) {
    if (!quiet) {
      console.log(`[local-d1] Applying local migrations for ${entry.binding}...`);
    }
    execFileSync(
      "npx",
      ["wrangler", "d1", "migrations", "apply", entry.binding, "--local", "--config", configPath, "--persist-to", persistDir],
      {
        cwd: process.cwd(),
        env: { ...process.env, CI: "true" },
        stdio: quiet ? "pipe" : "inherit",
      },
    );
  }
  return selected;
}

function readJsonBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      try {
        const raw = Buffer.concat(chunks).toString("utf8");
        resolve(raw ? JSON.parse(raw) : {});
      } catch (err) {
        reject(err);
      }
    });
    req.on("error", reject);
  });
}

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) });
  res.end(body);
}

function prepareStatement(db, query) {
  const stmt = db.prepare(query.sql);
  const params = Array.isArray(query.params) ? query.params : [];
  return params.length > 0 ? stmt.bind(...params) : stmt;
}

export async function startLocalD1Bridge({ configPath, persistDir, port = 0, quiet = false }) {
  const databases = parseWranglerDatabases(configPath);
  const proxy = await getPlatformProxy({
    configPath,
    persist: { path: path.join(persistDir, "v3") },
  });

  const dbById = new Map();
  for (const entry of databases) {
    const db = proxy.env[entry.binding];
    if (!db) {
      await proxy.dispose();
      throw new Error(`Missing D1 binding ${entry.binding} in ${configPath}`);
    }
    dbById.set(entry.databaseId, { ...entry, db });
  }

  let singleQueries = 0;
  let batchQueries = 0;
  let errorResponses = 0;

  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, "http://127.0.0.1");
      const match = url.pathname.match(/^\/client\/v4\/accounts\/[^/]+\/d1\/database(?:\/([^/]+)(?:\/(query))?)?$/);
      if (!match) {
        return sendJson(res, 404, { success: false, errors: [{ message: `Unknown path ${url.pathname}` }] });
      }
      const [, dbId, action] = match;

      if (!dbId && req.method === "GET") {
        const list = Array.from(dbById.values()).map((entry) => ({ uuid: entry.databaseId, name: entry.databaseName }));
        return sendJson(res, 200, { success: true, result: list, result_info: { total_count: list.length } });
      }

      if (dbId && !action && req.method === "GET") {
        const entry = dbById.get(dbId);
        if (!entry) {
          return sendJson(res, 404, { success: false, errors: [{ message: `Database ${dbId} not found` }] });
        }
        const probe = await entry.db.prepare("SELECT 1").all();
        const fileSize = Number(probe?.meta?.size_after ?? 0);
        return sendJson(res, 200, { success: true, result: { uuid: entry.databaseId, name: entry.databaseName, file_size: fileSize } });
      }

      if (dbId && action === "query" && req.method === "POST") {
        const entry = dbById.get(dbId);
        if (!entry) {
          return sendJson(res, 404, { success: false, errors: [{ message: `Database ${dbId} not found` }] });
        }
        const body = await readJsonBody(req);
        if (Array.isArray(body.batch)) {
          batchQueries += 1;
          const stmts = body.batch.map((query) => prepareStatement(entry.db, query));
          const results = await entry.db.batch(stmts);
          return sendJson(res, 200, { success: true, result: results, errors: [] });
        }
        if (typeof body.sql === "string") {
          singleQueries += 1;
          const result = await prepareStatement(entry.db, body).all();
          return sendJson(res, 200, { success: true, result: [result], errors: [] });
        }
        return sendJson(res, 400, { success: false, errors: [{ message: "Expected sql or batch in query request" }] });
      }

      return sendJson(res, 405, { success: false, errors: [{ message: `Unsupported ${req.method} ${url.pathname}` }] });
    } catch (err) {
      errorResponses += 1;
      if (!quiet) {
        console.error(`[local-d1] Error handling ${req.method} ${req.url}:`, err?.message || err);
      }
      return sendJson(res, 400, { success: false, errors: [{ message: String(err?.message || err) }] });
    }
  });

  await new Promise((resolve) => server.listen(port, "127.0.0.1", resolve));
  const address = server.address();
  const baseUrl = `http://127.0.0.1:${address.port}`;

  return {
    baseUrl,
    port: address.port,
    databases: dbById,
    stats: () => ({ singleQueries, batchQueries, errorResponses }),
    close: async () => {
      await new Promise((resolve) => server.close(resolve));
      await proxy.dispose();
    },
  };
}

function loadDotEnvDefaults(envPath) {
  const loaded = {};
  if (!fs.existsSync(envPath)) {
    return loaded;
  }
  for (const rawLine of fs.readFileSync(envPath, "utf8").split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) {
      continue;
    }
    const separator = line.indexOf("=");
    if (separator <= 0) {
      continue;
    }
    const key = line.slice(0, separator).trim();
    let value = line.slice(separator + 1).trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }
    loaded[key] = value;
  }
  return loaded;
}

function parseCliArgs(argv) {
  const options = {
    configPath: path.resolve("wrangler.toml"),
    persistDir: path.resolve(".wrangler/state"),
    providersPath: null,
    skipMigrations: false,
    reset: false,
    serveOnly: false,
    port: null,
    release: false,
    binPath: null,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--config":
        options.configPath = path.resolve(argv[++index]);
        break;
      case "--persist-to":
        options.persistDir = path.resolve(argv[++index]);
        break;
      case "--providers":
        options.providersPath = path.resolve(argv[++index]);
        break;
      case "--skip-migrations":
        options.skipMigrations = true;
        break;
      case "--reset":
        options.reset = true;
        break;
      case "--serve-only":
        options.serveOnly = true;
        break;
      case "--port":
        options.port = Number(argv[++index]);
        break;
      case "--release":
        options.release = true;
        break;
      case "--bin":
        options.binPath = path.resolve(argv[++index]);
        break;
      case "--help":
      case "-h":
        console.log(
          "Usage: node scripts/local-d1-importer.mjs [--config <wrangler.toml>] [--persist-to <.wrangler/state>] [--providers <providers.toml>] [--skip-migrations] [--reset] [--serve-only] [--port <port>] [--release] [--bin <path>]",
        );
        process.exit(0);
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }
  return options;
}

async function printDatabaseSummary(databases) {
  for (const entry of databases.values()) {
    const progress = await entry.db.prepare('SELECT FileName, LastProcessedLine, LastProcessedByte, Status FROM "0_import_progress" ORDER BY FileName').all();
    const version = await entry.db.prepare('SELECT ETag FROM "0_dataset_versions" ORDER BY rowid DESC LIMIT 1').all();
    const staging = await entry.db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name LIKE '%__staging'").all();
    const probe = await entry.db.prepare("SELECT 1").all();
    console.log(
      `[local-d1] === ${entry.binding} (size=${probe?.meta?.size_after ?? 0}, etag=${version.results[0]?.ETag ?? "none"}, leftover_staging=${staging.results.length}) ===`,
    );
    for (const row of progress.results) {
      console.log(`  ${row.FileName}: line=${row.LastProcessedLine} byte=${row.LastProcessedByte} status=${row.Status}`);
    }
    const budgetTable = await entry.db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = '0_daily_import_budget'").all();
    if (budgetTable.results.length > 0) {
      const budget = await entry.db.prepare('SELECT DAY, Reserved FROM "0_daily_import_budget" WHERE Id = 1').all();
      console.log(`  budget: ${JSON.stringify(budget.results)}`);
    }
  }
}

async function runCli() {
  const options = parseCliArgs(process.argv.slice(2));
  if (options.reset) {
    fs.rmSync(options.persistDir, { recursive: true, force: true });
  }
  fs.mkdirSync(options.persistDir, { recursive: true });

  if (!options.skipMigrations) {
    applyLocalD1Migrations({ configPath: options.configPath, persistDir: options.persistDir });
  }

  const port = options.port ?? (options.serveOnly ? 8799 : 0);
  const bridge = await startLocalD1Bridge({ configPath: options.configPath, persistDir: options.persistDir, port });
  console.log(`[local-d1] Bridge listening on ${bridge.baseUrl} (persist: ${options.persistDir})`);

  if (options.serveOnly) {
    console.log(`[local-d1] Run the importer against local D1 with:`);
    console.log(`  D1_API_BASE_URL=${bridge.baseUrl} CLOUDFLARE_ACCOUNT_ID=local-account CLOUDFLARE_API_TOKEN=local-token cargo run -p importer`);
    const shutdown = async () => {
      await bridge.close();
      process.exit(0);
    };
    process.on("SIGINT", shutdown);
    process.on("SIGTERM", shutdown);
    return;
  }

  const dotenvDefaults = loadDotEnvDefaults(path.resolve(".env"));
  const childEnv = {
    ...dotenvDefaults,
    ...process.env,
    CLOUDFLARE_ACCOUNT_ID: process.env.CLOUDFLARE_ACCOUNT_ID || dotenvDefaults.CLOUDFLARE_ACCOUNT_ID || "local-account",
    CLOUDFLARE_API_TOKEN: process.env.CLOUDFLARE_API_TOKEN || dotenvDefaults.CLOUDFLARE_API_TOKEN || "local-token",
    D1_API_BASE_URL: bridge.baseUrl,
  };
  if (options.providersPath) {
    childEnv.PROVIDERS_CONFIG = options.providersPath;
  }

  const [command, args] = options.binPath
    ? [options.binPath, []]
    : ["cargo", options.release ? ["run", "--release", "-p", "importer"] : ["run", "-p", "importer"]];

  let exitCode = 1;
  try {
    const child = spawn(command, args, { cwd: process.cwd(), env: childEnv, stdio: "inherit" });
    exitCode = (await new Promise((resolve) => child.on("close", resolve))) ?? 1;
    const stats = bridge.stats();
    console.log(
      `[local-d1] Importer exited with code ${exitCode} (single queries: ${stats.singleQueries}, batch queries: ${stats.batchQueries}, error responses: ${stats.errorResponses})`,
    );
    await printDatabaseSummary(bridge.databases);
  } finally {
    await bridge.close();
  }
  process.exit(exitCode);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  runCli().catch((err) => {
    console.error("[local-d1] Fatal error:", err);
    process.exit(1);
  });
}
