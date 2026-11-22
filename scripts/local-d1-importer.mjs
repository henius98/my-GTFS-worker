#!/usr/bin/env node
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
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
      .filter((entry) => entry.binding && entry.database_id)
      .map((entry) => ({
        binding: entry.binding,
        databaseName: entry.database_name || entry.binding,
        databaseId: entry.database_id,
      }));
  }
  const entries = [];
  for (const block of raw.split("[[d1_databases]]").slice(1)) {
    const binding = block.match(/binding\s*=\s*"([^"]+)"/)?.[1];
    const databaseName = block.match(/database_name\s*=\s*"([^"]+)"/)?.[1] || binding;
    const databaseId = block.match(/database_id\s*=\s*"([^"]+)"/)?.[1];
    if (binding && databaseId) {
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

function summarizeSqlPreview(sql) {
  const collapsed = String(sql).replace(/\s+/g, " ").trim();
  return collapsed.length > 96 ? `${collapsed.slice(0, 93)}...` : collapsed;
}

function sumResultMeta(results) {
  let rowsRead = 0;
  let rowsWritten = 0;
  for (const result of results) {
    rowsRead += Number(result?.meta?.rows_read ?? 0);
    rowsWritten += Number(result?.meta?.rows_written ?? 0);
  }
  return { rowsRead, rowsWritten };
}

export async function startLocalD1Bridge({ configPath, persistDir, port = 0, quiet = false, logQueries = false }) {
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
  let rowsRead = 0;
  let rowsWritten = 0;
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
          const startedAt = Date.now();
          const stmts = body.batch.map((query) => prepareStatement(entry.db, query));
          const results = await entry.db.batch(stmts);
          const usage = sumResultMeta(results);
          rowsRead += usage.rowsRead;
          rowsWritten += usage.rowsWritten;
          if (logQueries && !quiet) {
            const firstSql = body.batch[0]?.sql ? ` (${summarizeSqlPreview(body.batch[0].sql)})` : "";
            console.log(
              `[local-d1] [${entry.binding}] BATCH ${body.batch.length} stmt(s) in ${Date.now() - startedAt}ms read=${usage.rowsRead} written=${usage.rowsWritten}${firstSql}`,
            );
          }
          return sendJson(res, 200, { success: true, result: results, errors: [] });
        }
        if (typeof body.sql === "string") {
          singleQueries += 1;
          const startedAt = Date.now();
          const result = await prepareStatement(entry.db, body).all();
          const usage = sumResultMeta([result]);
          rowsRead += usage.rowsRead;
          rowsWritten += usage.rowsWritten;
          if (logQueries && !quiet) {
            console.log(
              `[local-d1] [${entry.binding}] QUERY in ${Date.now() - startedAt}ms read=${usage.rowsRead} written=${usage.rowsWritten}: ${summarizeSqlPreview(body.sql)}`,
            );
          }
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
    stats: () => ({ singleQueries, batchQueries, rowsRead, rowsWritten, errorResponses }),
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

export function parseProvidersToml(providersPath) {
  const raw = fs.readFileSync(providersPath, "utf8");
  let budgetProvider = null;
  const providers = [];
  let section = null;
  let current = null;

  for (const rawLine of raw.split(/\r?\n/)) {
    const line = rawLine.replace(/#.*$/, "").trim();
    if (!line) {
      continue;
    }
    const headerMatch = line.match(/^\[\[([^\]]+)\]\]$/);
    if (headerMatch) {
      section = headerMatch[1].trim();
      if (section === "providers") {
        current = { name: "", isActive: false, staticUrl: "", staticProvider: "", databaseId: "" };
        providers.push(current);
      } else {
        current = null;
      }
      continue;
    }
    const sep = line.indexOf("=");
    if (sep <= 0) {
      continue;
    }
    const key = line.slice(0, sep).trim();
    const rawValue = line.slice(sep + 1).trim();
    const strMatch = rawValue.match(/^"([^"]*)"$/);

    if (section === null && key === "budget_provider" && strMatch) {
      budgetProvider = strMatch[1];
    } else if (section === "providers" && current) {
      if (key === "name" && strMatch) current.name = strMatch[1];
      else if (key === "is_active") current.isActive = rawValue === "true";
      else if (key === "static_url" && strMatch) current.staticUrl = strMatch[1];
      else if (key === "static_provider" && strMatch) current.staticProvider = strMatch[1];
      else if (key === "database_id" && strMatch) current.databaseId = strMatch[1];
    }
  }

  return { budgetProvider, providers };
}

export function validateProviderDatabaseIds(providersPath, wranglerDatabases, selectedProviders = null) {
  const parsed = parseProvidersToml(providersPath);
  const selectedSet = selectedProviders ? new Set(selectedProviders) : null;
  if (selectedSet) {
    const knownNames = new Set(parsed.providers.map((entry) => entry.name));
    const unknown = selectedProviders.filter((name) => !knownNames.has(name));
    if (unknown.length > 0) {
      throw new Error(`Unknown provider(s) for --provider: ${unknown.join(", ")} (available: ${Array.from(knownNames).join(", ")})`);
    }
  }

  const wranglerIds = new Set(wranglerDatabases.map((entry) => entry.databaseId));
  const required = [];
  for (const provider of parsed.providers) {
    const isBudget = provider.name === parsed.budgetProvider;
    const isActive = selectedSet ? selectedSet.has(provider.name) : provider.isActive;
    if ((isBudget || isActive) && provider.databaseId) {
      required.push(provider);
    }
  }

  const missing = required.filter((provider) => !wranglerIds.has(provider.databaseId));
  if (missing.length > 0) {
    const details = missing.map((provider) => `${provider.name}=${provider.databaseId}`).join(", ");
    throw new Error(
      `providers.toml and wrangler.toml database_id mismatch for: ${details}. Run ./scripts/generate-wrangler.sh (or pass matching --config / --providers).`,
    );
  }
  return parsed;
}

function writeFilteredProvidersToml(parsedProviders, selectedNames) {
  const selectedSet = new Set(selectedNames);
  const lines = [];
  if (parsedProviders.budgetProvider) {
    lines.push(`budget_provider = ${JSON.stringify(parsedProviders.budgetProvider)}`);
    lines.push("");
  }
  for (const provider of parsedProviders.providers) {
    lines.push("[[providers]]");
    lines.push(`name = ${JSON.stringify(provider.name)}`);
    lines.push(`is_active = ${selectedSet.has(provider.name) ? "true" : "false"}`);
    lines.push(`static_url = ${JSON.stringify(provider.staticUrl)}`);
    lines.push(`static_provider = ${JSON.stringify(provider.staticProvider)}`);
    lines.push(`database_id = ${JSON.stringify(provider.databaseId)}`);
    lines.push("");
  }
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "gtfs-local-providers-"));
  const tempPath = path.join(tempDir, "providers.toml");
  fs.writeFileSync(tempPath, `${lines.join("\n")}`, "utf8");
  return {
    path: tempPath,
    cleanup: () => fs.rmSync(tempDir, { recursive: true, force: true }),
  };
}

export async function resetLocalDailyBudget(databases, { quiet = false } = {}) {
  let resetCount = 0;
  for (const entry of databases.values()) {
    const budgetTable = await entry.db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = '0_daily_import_budget'").all();
    if (budgetTable.results.length > 0) {
      await entry.db.prepare('UPDATE "0_daily_import_budget" SET DAY = ?, Reserved = 0 WHERE Id = 1').bind("").run();
      resetCount += 1;
      if (!quiet) {
        console.log(`[local-d1] Reset daily import budget in ${entry.binding}`);
      }
    }
  }
  return resetCount;
}

function requireArgValue(argv, index, flag) {
  const value = argv[index + 1];
  if (value === undefined) {
    throw new Error(`Missing value for ${flag}`);
  }
  return value;
}

export function resolveSelectedBindings(parsedProviders, wranglerDatabases, selectedProviders = null) {
  if (!selectedProviders || selectedProviders.length === 0) {
    return null;
  }
  const selectedSet = new Set(selectedProviders);
  const requiredIds = new Set();
  for (const provider of parsedProviders.providers) {
    if ((selectedSet.has(provider.name) || provider.name === parsedProviders.budgetProvider) && provider.databaseId) {
      requiredIds.add(provider.databaseId);
    }
  }
  return wranglerDatabases.filter((entry) => requiredIds.has(entry.databaseId)).map((entry) => entry.binding);
}

export function parseCliArgs(argv) {
  const options = {
    configPath: path.resolve("wrangler.toml"),
    persistDir: path.resolve(".wrangler/state"),
    providersPath: null,
    selectedProviders: [],
    dailyWriteLimit: null,
    maxRows: null,
    skipMigrations: false,
    reset: false,
    resetBudget: false,
    serveOnly: false,
    status: false,
    logQueries: false,
    port: null,
    release: false,
    binPath: null,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--":
        break;
      case "--config":
        options.configPath = path.resolve(requireArgValue(argv, index, arg));
        index += 1;
        break;
      case "--persist-to":
        options.persistDir = path.resolve(requireArgValue(argv, index, arg));
        index += 1;
        break;
      case "--providers":
        options.providersPath = path.resolve(requireArgValue(argv, index, arg));
        index += 1;
        break;
      case "--provider": {
        const raw = requireArgValue(argv, index, arg);
        index += 1;
        const names = raw
          .split(",")
          .map((name) => name.trim())
          .filter(Boolean);
        if (names.length === 0) {
          throw new Error("--provider must include at least one provider name");
        }
        options.selectedProviders.push(...names);
        break;
      }
      case "--daily-write-limit": {
        const raw = requireArgValue(argv, index, arg);
        index += 1;
        const parsed = Number(raw);
        if (!Number.isInteger(parsed) || parsed <= 0) {
          throw new Error(`Invalid --daily-write-limit: ${raw} (must be a positive integer)`);
        }
        options.dailyWriteLimit = parsed;
        break;
      }
      case "--max-rows": {
        const raw = requireArgValue(argv, index, arg);
        index += 1;
        const parsed = Number(raw);
        if (!Number.isInteger(parsed) || parsed <= 0) {
          throw new Error(`Invalid --max-rows: ${raw} (must be a positive integer)`);
        }
        options.maxRows = parsed;
        break;
      }
      case "--skip-migrations":
        options.skipMigrations = true;
        break;
      case "--reset":
        options.reset = true;
        break;
      case "--reset-budget":
        options.resetBudget = true;
        break;
      case "--serve-only":
        options.serveOnly = true;
        break;
      case "--status":
        options.status = true;
        break;
      case "--log-queries":
        options.logQueries = true;
        break;
      case "--port": {
        const raw = requireArgValue(argv, index, arg);
        index += 1;
        const parsed = Number(raw);
        if (!Number.isInteger(parsed) || parsed < 0 || parsed > 65535) {
          throw new Error(`Invalid --port: ${raw}`);
        }
        options.port = parsed;
        break;
      }
      case "--release":
        options.release = true;
        break;
      case "--bin":
        options.binPath = path.resolve(requireArgValue(argv, index, arg));
        index += 1;
        break;
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }
  if (options.status && (options.reset || options.resetBudget || options.serveOnly)) {
    throw new Error("--status cannot be combined with --reset, --reset-budget, or --serve-only");
  }
  return options;
}

async function printDatabaseSummary(databases, bindings = null) {
  const bindingFilter = bindings ? new Set(bindings) : null;
  for (const entry of databases.values()) {
    if (bindingFilter && !bindingFilter.has(entry.binding)) {
      continue;
    }
    const tables = await entry.db.prepare("SELECT name FROM sqlite_schema WHERE type = 'table'").all();
    const tableNames = new Set(tables.results.map((row) => row.name));
    if (!tableNames.has("0_import_progress")) {
      console.log(`[local-d1] === ${entry.binding} (not migrated) ===`);
      continue;
    }
    const progress = await entry.db.prepare('SELECT FileName, LastProcessedLine, LastProcessedByte, Status FROM "0_import_progress" ORDER BY FileName').all();
    const version = tableNames.has("0_dataset_versions")
      ? await entry.db.prepare('SELECT ETag FROM "0_dataset_versions" ORDER BY rowid DESC LIMIT 1').all()
      : { results: [] };
    const staging = tables.results.filter((row) => row.name.endsWith("__staging"));
    const probe = await entry.db.prepare("SELECT 1").all();
    console.log(
      `[local-d1] === ${entry.binding} (size=${probe?.meta?.size_after ?? 0}, etag=${version.results[0]?.ETag ?? "none"}, leftover_staging=${staging.length}) ===`,
    );
    for (const row of progress.results) {
      console.log(`  ${row.FileName}: line=${row.LastProcessedLine} byte=${row.LastProcessedByte} status=${row.Status}`);
    }
    if (tableNames.has("0_daily_import_budget")) {
      const budget = await entry.db.prepare('SELECT DAY, Reserved FROM "0_daily_import_budget" WHERE Id = 1').all();
      console.log(`  budget: ${JSON.stringify(budget.results)}`);
    }
  }
}

async function runCli() {
  const options = parseCliArgs(process.argv.slice(2));
  if (options.help) {
    console.log(
      "Usage: node scripts/local-d1-importer.mjs [--config <wrangler.toml>] [--persist-to <.wrangler/state>] [--providers <providers.toml>] [--provider <name[,name2]>] [--daily-write-limit <n>] [--max-rows <n>] [--skip-migrations] [--reset] [--reset-budget] [--status] [--log-queries] [--serve-only] [--port <port>] [--release] [--bin <path>]",
    );
    process.exit(0);
  }

  const wranglerDatabases = parseWranglerDatabases(options.configPath);
  const resolvedProvidersPath = options.providersPath || (process.env.PROVIDERS_CONFIG ? path.resolve(process.env.PROVIDERS_CONFIG) : path.resolve("providers.toml"));
  const selectedProviders = options.selectedProviders.length > 0 ? options.selectedProviders : null;
  const parsedProviders = validateProviderDatabaseIds(resolvedProvidersPath, wranglerDatabases, selectedProviders);
  const selectedBindings = resolveSelectedBindings(parsedProviders, wranglerDatabases, selectedProviders);

  if (options.status) {
    if (!fs.existsSync(path.join(options.persistDir, "v3"))) {
      console.log(`[local-d1] No local D1 state found at ${options.persistDir}`);
      process.exit(0);
    }
    const bridge = await startLocalD1Bridge({ configPath: options.configPath, persistDir: options.persistDir, quiet: true });
    try {
      await printDatabaseSummary(bridge.databases, selectedBindings);
    } finally {
      await bridge.close();
    }
    process.exit(0);
  }

  if (options.reset) {
    fs.rmSync(options.persistDir, { recursive: true, force: true });
  }
  fs.mkdirSync(options.persistDir, { recursive: true });

  if (!options.skipMigrations) {
    applyLocalD1Migrations({ configPath: options.configPath, persistDir: options.persistDir, bindings: selectedBindings });
  }

  const port = options.port ?? (options.serveOnly ? 8799 : 0);
  const bridge = await startLocalD1Bridge({
    configPath: options.configPath,
    persistDir: options.persistDir,
    port,
    logQueries: options.logQueries,
  });
  console.log(`[local-d1] Bridge listening on ${bridge.baseUrl} (persist: ${options.persistDir})`);

  if (options.resetBudget) {
    await resetLocalDailyBudget(bridge.databases);
  }

  const filteredProviders = selectedProviders ? writeFilteredProvidersToml(parsedProviders, selectedProviders) : null;
  const effectiveProvidersPath = filteredProviders ? filteredProviders.path : options.providersPath;

  if (options.serveOnly) {
    const envOverrides = [
      `D1_API_BASE_URL=${bridge.baseUrl}`,
      "CLOUDFLARE_ACCOUNT_ID=local-account",
      "CLOUDFLARE_API_TOKEN=local-token",
    ];
    if (effectiveProvidersPath) {
      envOverrides.push(`PROVIDERS_CONFIG=${JSON.stringify(effectiveProvidersPath)}`);
    }
    if (options.dailyWriteLimit !== null) {
      envOverrides.push(`D1_DAILY_WRITE_LIMIT=${options.dailyWriteLimit}`);
    }
    if (options.maxRows !== null) {
      envOverrides.push(`MAX_ROWS_PER_WORKFLOW=${options.maxRows}`);
    }
    console.log(`[local-d1] Run the importer against local D1 with:`);
    console.log(`  set -a; source .env; set +a; ${envOverrides.join(" ")} cargo run -p importer`);
    const shutdown = async () => {
      filteredProviders?.cleanup();
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
    CLOUDFLARE_ACCOUNT_ID: "local-account",
    CLOUDFLARE_API_TOKEN: "local-token",
    D1_API_BASE_URL: bridge.baseUrl,
  };
  if (effectiveProvidersPath) {
    childEnv.PROVIDERS_CONFIG = effectiveProvidersPath;
  }
  if (options.dailyWriteLimit !== null) {
    childEnv.D1_DAILY_WRITE_LIMIT = String(options.dailyWriteLimit);
  }
  if (options.maxRows !== null) {
    childEnv.MAX_ROWS_PER_WORKFLOW = String(options.maxRows);
  }

  const [command, args] = options.binPath
    ? [options.binPath, []]
    : ["cargo", options.release ? ["run", "--release", "-p", "importer"] : ["run", "-p", "importer"]];

  let exitCode = 1;
  const startedAt = Date.now();
  try {
    const child = spawn(command, args, { cwd: process.cwd(), env: childEnv, stdio: "inherit" });
    exitCode = (await new Promise((resolve) => child.on("close", resolve))) ?? 1;
    const elapsedMs = Date.now() - startedAt;
    const stats = bridge.stats();
    console.log(
      `[local-d1] Importer exited with code ${exitCode} in ${elapsedMs}ms (single queries: ${stats.singleQueries}, batch queries: ${stats.batchQueries}, rows read: ${stats.rowsRead}, rows written: ${stats.rowsWritten}, error responses: ${stats.errorResponses})`,
    );
    await printDatabaseSummary(bridge.databases, selectedBindings);
  } finally {
    filteredProviders?.cleanup();
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
