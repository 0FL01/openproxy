// Synthetic same-origin fixture only. No OpenProxy instance, credentials, or provider traffic.
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(__dirname, "../target/dx/openproxy-dashboard/release/web/public");
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
const state = {
  writes: [],
  connections: [
    { id: "connection-one", provider: "kilocode", name: "Fixture account", authType: "oauth", testStatus: "active", isActive: true, priority: 1 },
    { id: "connection-other", provider: "codex", name: "Other account", isActive: true },
  ],
  custom: [
    { providerAlias: "kc", id: "fixture-standard", name: "My Standard", type: "llm", opencode: { limit: { context: 123456 } } },
    { providerAlias: "kc", id: "custom-disabled", name: "Custom Disabled", type: "llm" },
    { providerAlias: "cx", id: "codex-only", name: "Codex custom", type: "llm" },
  ],
  aliases: { legacy: "kc/legacy-model", builtin: "kc/fixture-standard", codexLegacy: "cx/codex-legacy" },
  disabled: { kc: ["fixture-disabled", "custom-disabled"], cx: [] },
  nodes: [{ id: "openai-compatible-fixture", name: "Fixture Node", type: "openai-compatible", prefix: "fixture-prefix", apiType: "responses", baseUrl: "https://example.invalid/v1" }],
  filters: {}, favorites: { kc: ["fixture-standard"] }, pools: [],
};

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  const respond = (status, value) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(value)); };
  if (url.pathname.startsWith("/api/")) {
    let raw = ""; for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : null;
    if (req.method !== "GET") state.writes.push({ method: req.method, path: `${url.pathname}${url.search}`, body, at: Date.now() });
    if (state.xaiForceOnly && req.method === "GET" && ["/api/settings", "/api/provider-nodes", "/api/proxy-pools"].includes(url.pathname)) {
      (state.xaiAuxReads ||= []).push({ path: `${url.pathname}${url.search}`, cache: req.headers["cache-control"] });
    }
    if (url.pathname === "/api/auth/status") {
      state.authStatusRequests = (state.authStatusRequests || 0) + 1;
      return respond(200, state.expired ? { requireLogin: true, authenticated: false } : { requireLogin: true, authenticated: true });
    }
    if (url.pathname === "/api/settings") {
      if (req.method === "GET") {
        if (state.failSettingsRead) return respond(503, { error: "Synthetic settings read failure" });
        return respond(200, state.settings || {});
      }
      if (req.method === "PATCH") {
        if (state.holdSettingsWrite) {
          state.holdSettingsWrite = false;
          await new Promise(resolve => { state.releaseSettingsWrite = resolve; });
        }
        if (state.rejectSettingsWrite) {
          const status = typeof state.rejectSettingsWrite === "number" ? state.rejectSettingsWrite : 503;
          if (status === 401) state.expired = true;
          return respond(status, { error: "Synthetic settings write failure" });
        }
        Object.assign(state.settings ||= {}, body);
        return respond(200, state.settings);
      }
    }
    if (url.pathname === "/api/build") return respond(200, {});
    if (url.pathname === "/api/catalog") {
      state.catalogRequests = (state.catalogRequests || 0) + 1;
      const dynamic = provider => {
        const conn = state.connections.find(c => c.provider === provider);
        return `${provider}-${conn ? (conn.isActive === false ? "inactive" : "active") : "removed"}`;
      };
      const data = { providerIdToAlias: { kilocode: "kc", codex: "cx" }, providerModels: [
        { alias: "kc", models: [{ id: "fixture-standard", name: "Official Name", kind: "llm", contextWindow: 128000 }, { id: "fixture-disabled", name: "Disabled", kind: "llm" }, { id: "image-only", kind: "image" }] },
        { alias: "cx", models: [{ id: "codex-base", name: "Codex Base", kind: "llm" }, { id: dynamic("codex") }] },
        ...["commandcode", "a6api", "xai"].map(id => ({ alias: id, models: id === "xai" && state.xaiForceOnly ? structuredClone(state.xaiCatalogRows) : [{ id: dynamic(id) }] })),
        { alias: "openrouter", models: [{ id: "openrouter-base", kind: "llm" }] },
      ] };
      if (state.holdCatalogOnce) { state.holdCatalogOnce = false; await new Promise(resolve => { state.releaseCatalog = resolve; }); }
      return respond(state.failCatalog ? 500 : 200, data);
    }
    if (url.pathname === "/api/providers" && req.method === "GET") {
      if (state.xaiForceOnly) {
        state.xaiConnectionReads = (state.xaiConnectionReads || 0) + 1;
        (state.xaiConnectionHeaders ||= []).push(req.headers["cache-control"]);
        if (state.xaiConnectionStatus) return respond(state.xaiConnectionStatus, { error: "Synthetic connection read failure" });
        if (state.xaiMalformedConnections) return respond(200, { connections: {} });
      }
      if (state.holdConnectionsOnce) {
        state.holdConnectionsOnce = false;
        const captured = structuredClone(state.connections);
        await new Promise(resolve => { state.releaseConnections = resolve; });
        return respond(200, { connections: captured });
      }
      return respond(200, { connections: state.connections });
    }
    if (url.pathname === "/api/providers/validate" && req.method === "POST") return respond(200, { valid: state.validationValid !== false });
    if (url.pathname === "/api/oauth/codex/bulk-import" && req.method === "POST") {
      if (state.holdCodexImport) {
        state.holdCodexImport = false;
        await new Promise(resolve => { state.releaseCodexImport = resolve; });
      }
      if (state.bulkImportStatus) {
        if (state.bulkImportStatus === 401) state.expired = true;
        return respond(state.bulkImportStatus, { error: "Synthetic import failure" });
      }
      if (state.bulkImportMalformed) return respond(200, { success: true });
      const results = body.accounts.map((account, index) => {
        const token = account?.tokens?.access_token || account?.access_token || account?.accessToken;
        if (!account || typeof account !== "object" || Array.isArray(account)) return { index, ok: false, error: "Item is not an object" };
        if (account.auth_mode != null && account.auth_mode !== "chatgpt") return { index, ok: false, error: "Unexpected auth_mode (expected chatgpt)" };
        if (!token) return { index, ok: false, error: "Missing accessToken" };
        const id = `codex-import-${++state.codexImportCount}`;
        // Only a sanitized connection is retained, never the synthetic token.
        state.connections.push({ id, provider: "codex", name: `Imported ${state.codexImportCount}`, authType: "oauth", isActive: true });
        return { index, ok: true, id };
      });
      const success = results.filter(result => result.ok).length;
      return respond(200, { success, failed: results.length - success, results });
    }
    if (url.pathname === "/api/providers" && req.method === "POST") {
      if (state.rejectAdd) return respond(400, { error: "Synthetic add rejected" });
      const conn = { id: `fixture-added-${++state.addCounter}`, provider: body.provider, name: body.name, authType: "apikey", hasApiKey: true, testStatus: body.testStatus, isActive: true, priority: body.priority, providerSpecificData: { proxyPoolId: body.proxyPoolId } };
      state.connections.push(conn);
      return respond(200, { connection: conn });
    }
    if (url.pathname === "/api/oauth/kilocode/device-code") return state.deviceFailure ? respond(500, { error: "Synthetic device failure" }) : respond(200, { device_code: "fixture-device-code", user_code: "FIXTURE-123", codeVerifier: "fixture-verifier", verification_uri: `http://127.0.0.1:${server.address().port}/fixture-verify`, interval: 0.02, expires_in: 3 });
    if (url.pathname === "/api/oauth/kilocode/poll" && req.method === "POST") {
      state.oauthPolls++;
      if (state.holdDevicePollOnce) {
        state.holdDevicePollOnce = false;
        await new Promise(resolve => { state.releaseDevicePoll = resolve; });
        state.connections.push({ id: "delayed-device", provider: "kilocode", name: "Delayed Device", authType: "oauth", isActive: true });
        return respond(200, { success: true });
      }
      if (state.completeDevice) {
        state.connections.push({ id: "device-added", provider: "kilocode", name: "Device account", authType: "oauth", isActive: true });
        return respond(200, { success: true });
      }
      return respond(200, { error: "authorization_pending" });
    }
    if (/^\/api\/oauth\/(claude|cline|xai|codex)\/authorize$/.test(url.pathname)) {
      const oauthState = state.callbackOnly ? `fixture-state-${state.callbackAuthCounter = (state.callbackAuthCounter || 0) + 1}` : "fixture-state";
      if (state.popupOnly) state.popupAuthorizations.push({ provider: url.pathname.split("/")[3], redirectUri: url.searchParams.get("redirect_uri"), state: oauthState });
      return respond(200, { authUrl: state.popupOnly ? `${state.popupProviderOrigin}/authorize` : `http://127.0.0.1:${server.address().port}/fixture-authorize`, codeVerifier: "fixture-verifier", state: oauthState });
    }
    if (/^\/api\/oauth\/(xai|codex)\/start-proxy$/.test(url.pathname)) {
      state.proxyStarts = (state.proxyStarts || 0) + 1;
      if (state.rejectProxyStart) return respond(503, { error: "Synthetic proxy failure" });
      if (state.holdProxy) await new Promise(resolve => { state.releaseProxy = resolve; });
      state.proxyCompleted = (state.proxyCompleted || 0) + 1;
      state.proxyLive = true;
      if (state.holdProxyResponse) await new Promise(resolve => { state.releaseProxy = resolve; });
      return respond(200, { success: true, serverSide: state.proxyServerSide === true });
    }
    if (/^\/api\/oauth\/(xai|codex)\/stop-proxy$/.test(url.pathname)) {
      state.proxyStops = (state.proxyStops || 0) + 1;
      if (state.gatewayStopOnce) {
        state.gatewayStopOnce = false;
        // The synthetic gateway times out while the backend stop is still pending.
        state.releaseGatewayStop = () => { state.proxyLive = false; state.gatewayStopApplied = true; };
        res.once("finish", () => { state.gatewayStopResponded = true; });
        return respond(504, { error: "Synthetic gateway timeout" });
      }
      state.proxyLive = false;
      if (state.holdStop) await new Promise(resolve => { state.releaseStop = resolve; });
      return respond(200, { success: true });
    }
    if (/^\/api\/oauth\/(xai|codex)\/poll-status$/.test(url.pathname)) {
      if (state.holdStatusOnce) {
        state.holdStatusOnce = false;
        await new Promise(resolve => { state.releaseStatus = resolve; });
        state.connections.push({ id: "status-added", provider: "xai", name: "Status account", authType: "oauth", isActive: true });
        return respond(200, { status: "done" });
      }
      return respond(200, { status: "pending" });
    }
    if (url.pathname === "/api/oauth/claude/exchange" && req.method === "POST") {
      if (state.rejectClaudeExchange) { state.rejectClaudeExchange = false; return respond(400, { error: "Synthetic exchange failure" }); }
      const index = state.claudeExchangeCount = (state.claudeExchangeCount || 0) + 1;
      if (state.holdExchangeOnce) { state.holdExchangeOnce = false; await new Promise(resolve => { state.releaseExchange = resolve; }); }
      state.connections.push({ id: `claude-added-${index}`, provider: "claude", name: state.authOnly ? `Claude fixture ${index}` : "Claude fixture", authType: "oauth", isActive: true });
      return respond(200, { success: true });
    }
    if (url.pathname === "/api/oauth/cline/exchange" && req.method === "POST") {
      const index = state.clineExchangeCount = (state.clineExchangeCount || 0) + 1;
      if (state.holdClineExchange) { state.holdClineExchange = false; await new Promise(resolve => { state.releaseClineExchange = resolve; }); }
      state.connections.push({ id: `cline-added-${index}`, provider: "cline", name: `Cline fixture ${index}`, authType: "oauth", isActive: true });
      return respond(200, { success: true });
    }
    if (url.pathname === "/api/oauth/xai/exchange" && req.method === "POST") {
      if (state.holdXaiCallback) await new Promise(resolve => { state.releaseXaiCallback = resolve; });
      state.connections.push({ id: "callback-xai", provider: "xai", name: "Callback xAI", authType: "oauth", isActive: true });
      return respond(200, { success: true });
    }
    if (url.pathname === "/api/oauth/xai/manual-code" && req.method === "POST") {
      const index = state.xaiExchangeCount = (state.xaiExchangeCount || 0) + 1;
      if (state.holdManualOnce) { state.holdManualOnce = false; await new Promise(resolve => { state.releaseManual = resolve; }); }
      state.connections.push({ id: `xai-added-${index}`, provider: "xai", name: state.authOnly ? `xAI fixture ${index}` : "xAI fixture", authType: "oauth", isActive: true });
      return respond(200, { success: true });
    }
    if (url.pathname === "/api/provider-nodes") {
      state.nodeReads = (state.nodeReads || 0) + 1;
      if (state.holdXaiNodeOnce) { state.holdXaiNodeOnce = false; await new Promise(resolve => { state.releaseXaiNode = resolve; }); }
      if (state.holdNodeRead) await new Promise(resolve => { state.releaseNodeRead = resolve; });
      if (state.nodeReadMode === "fail") return respond(503, { error: "Synthetic node read failure" });
      if (state.nodeReadMode === "unauthorized") { state.expired = true; return respond(401, { error: "expired" }); }
      if (state.nodeReadMode === "malformed") return respond(200, { nodes: {} });
      return respond(200, { nodes: state.nodes });
    }
    if (url.pathname === "/api/provider-nodes/validate" && req.method === "POST") {
      if (state.expired) return respond(401, { error: "expired" });
      const valid = state.nodeCheckValid !== false;
      if (state.holdNodeCheck) { state.holdNodeCheck = false; await new Promise(resolve => { state.releaseNodeCheck = resolve; }); }
      return respond(200, { valid });
    }
    if (url.pathname.startsWith("/api/provider-nodes/") && req.method === "PUT") {
      if (state.expired) return respond(401, { error: "expired" });
      if (state.rejectNodeSave) return respond(400, { error: "Synthetic node update rejected" });
      if (state.holdNodeSave) { state.holdNodeSave = false; await new Promise(resolve => { state.releaseNodeSave = resolve; }); }
      const node = state.nodes.find(item => item.id === decodeURIComponent(url.pathname.split("/").pop()));
      if (!node) return respond(404, { error: "Node not found" });
      Object.assign(node, body);
      return respond(200, { node });
    }
    if (url.pathname.startsWith("/api/provider-nodes/") && req.method === "DELETE") {
      if (state.holdNodeDelete) { state.holdNodeDelete = false; await new Promise(resolve => { state.releaseNodeDelete = resolve; }); }
      if (state.expired) return respond(401, { error: "expired" });
      if (state.rejectNodeDelete) return respond(500, { error: "Synthetic node delete rejected" });
      const id = decodeURIComponent(url.pathname.split("/").pop());
      if (!state.nodes.some(node => node.id === id)) return respond(404, { error: "Node not found" });
      state.nodes = state.nodes.filter(node => node.id !== id);
      return respond(200, { success: true, message: "Node deleted" });
    }
    if (url.pathname === "/api/proxy-pools") return respond(200, { proxyPools: state.pools });
    if (url.pathname === "/api/providers/kilo/free-models") return respond(200, { models: [{ id: "fixture-free", name: "Fixture Free" }, { id: "fixture-standard", name: "Duplicate live" }] });
    if (url.pathname === "/api/providers/suggested-models") {
      assert.equal(url.searchParams.get("url"), "https://openrouter.ai/api/v1/models");
      assert.equal(url.searchParams.get("type"), "openrouter-free");
      if (state.holdLive) await new Promise(resolve => { state.releaseLive = resolve; });
      return respond(200, { data: [{ id: "fixture-paid", name: "Fixture Paid", isFree: true }, { id: "fixture:free", name: "Fixture Free" }] });
    }
    if (url.pathname === "/api/models") return respond(200, { models: [{ fullModel: "kilocode/fixture-standard", model: "fixture-standard", caps: { vision: true, reasoning: true } }] });
    if (url.pathname === "/api/providers/filters") {
      if (req.method === "PUT") state.filters[body.alias] = { freeOnly: body.freeOnly };
      return respond(200, { filters: state.filters });
    }
    if (url.pathname === "/api/models/favorites") {
      if (req.method === "PUT") state.favorites[body.alias] = body.favorites;
      else if (state.holdFavorites) { const captured = structuredClone(state.favorites); await new Promise(resolve => { state.releaseFavorites = resolve; }); return respond(200, { favorites: captured }); }
      return respond(200, { favorites: state.favorites });
    }
    if (url.pathname === "/api/models/disabled") {
      const alias = req.method === "POST" ? body.providerAlias : url.searchParams.get("providerAlias");
      const saved = state.disabled[alias] ||= [];
      if (req.method === "POST") state.disabled[alias] = [...new Set([...saved, ...body.ids])];
      if (req.method === "DELETE") state.disabled[alias] = url.searchParams.has("id") ? saved.filter(id => id !== url.searchParams.get("id")) : [];
      return respond(200, req.method === "GET" ? { ids: saved } : { success: true });
    }
    if (url.pathname === "/api/models/custom") {
      if (req.method === "GET") {
        state.customReads = (state.customReads || 0) + 1;
        if (state.customReadStatus) {
          if (state.customReadStatus === 401) state.expired = true;
          return respond(state.customReadStatus, { error: "Synthetic custom-model read failure" });
        }
      }
      if (req.method === "POST") {
        if (state.expired) return respond(401, { error: "expired" });
        if (state.unauthorizedCustomId === body.id) { state.expired = true; return respond(401, { error: "expired" }); }
        if (state.rejectCustomIds?.includes(body.id)) return respond(400, { error: "Synthetic custom-model failure" });
        if (state.rejectCustom) return respond(400, { error: "Fixture rejection" });
        if (state.holdCustom) await new Promise(resolve => { state.releaseCustom = resolve; });
        if (state.importOnly) {
          if (state.malformedCustomId === body.id) {
            state.malformedCustomId = undefined;
            state.custom.push(body);
            return respond(200, { success: true });
          }
          if (state.concurrentCustomId === body.id) {
            state.concurrentCustomId = undefined;
            state.custom.push({ ...body, name: "Other writer", opencode: { limit: { context: 654321 } } });
          }
          const exists = state.custom.some(model => model.providerAlias === body.providerAlias && model.id === body.id && (model.type || "llm") === body.type);
          if (!exists) state.custom.push(body);
          return respond(200, { success: true, added: !exists });
        }
        state.custom.push(body);
      }
      if (req.method === "DELETE") state.custom = state.custom.filter(m => !(m.providerAlias === url.searchParams.get("providerAlias") && m.id === url.searchParams.get("id") && (m.type || "llm") === url.searchParams.get("type")));
      return respond(200, req.method === "GET" ? { models: state.custom } : { success: true });
    }
    if (url.pathname === "/api/models/alias") {
      if (req.method === "DELETE") delete state.aliases[url.searchParams.get("alias")];
      if (state.numericAliases && req.method === "GET") {
        const rest = Object.entries(state.aliases).filter(([key]) => key !== "10" && key !== "2");
        const values = ["10", "2"].filter(key => Object.hasOwn(state.aliases, key)).map(key => `${JSON.stringify(key)}:${JSON.stringify(state.aliases[key])}`);
        values.push(...rest.map(([key, value]) => `${JSON.stringify(key)}:${JSON.stringify(value)}`));
        res.writeHead(200, { "content-type": "application/json" }); return res.end(`{"aliases":{${values.join(",")}}}`);
      }
      return respond(200, { aliases: state.aliases });
    }
    if (url.pathname === "/api/models/test") {
      if (state.holdModelTest) await new Promise(resolve => { state.releaseModelTest = resolve; });
      return respond(200, state.modelTestResult || { ok: true });
    }
    if (/^\/api\/providers\/[^/]+\/models$/.test(url.pathname) && req.method === "GET") {
      state.importGets ||= [];
      state.importGets.push(url.pathname);
      if (state.xaiForceOnly) {
        state.xaiForceHeaders ||= [];
        state.xaiForceHeaders.push(req.headers);
        if (state.holdXaiForce) await new Promise(resolve => { state.releaseXaiForce = resolve; });
        const status = state.xaiForceStatus || 200;
        if (status === 401) state.expired = true;
        if (status === 200 && state.xaiNextCatalogRows) state.xaiCatalogRows = structuredClone(state.xaiNextCatalogRows);
        return respond(status, { models: [{ id: "must-not-auto-import", kind: "llm" }] });
      }
      const status = state.importStatus || 200;
      const data = structuredClone(state.importData || { models: [] });
      if (state.holdImport) await new Promise(resolve => { state.releaseImport = resolve; });
      if (status === 401) state.expired = true;
      return respond(status, data);
    }
    if (/^\/api\/providers\/[^/]+\/test$/.test(url.pathname) && req.method === "POST") {
      if (state.holdConnectionTest && url.pathname.includes("bulk-one")) await new Promise(resolve => { state.releaseConnectionTest = resolve; });
      if (state.holdEditTest && url.pathname.includes("oauth-first")) await new Promise(resolve => { state.releaseEditTest = resolve; });
      return respond(200, { valid: !url.pathname.includes("bulk-two") && !url.pathname.includes("oauth-second"), error: "Synthetic connection failure" });
    }
    if (url.pathname.startsWith("/api/providers/") && req.method === "PUT") {
      if (url.pathname.endsWith("/connection-race") && state.holdToggle) {
        await new Promise(resolve => { state.releaseToggle = resolve; });
        return respond(401, { error: "expired" });
      }
      const conn = state.connections.find(c => c.id === decodeURIComponent(url.pathname.split("/").pop()));
      if (conn) {
        const { proxyPoolId, apiKey, ...rest } = body;
        Object.assign(conn, rest);
        if (Object.hasOwn(body, "proxyPoolId")) conn.providerSpecificData = { ...conn.providerSpecificData, proxyPoolId };
        if (apiKey) conn.hasApiKey = true; // simulate masked backend response, never return the fixture secret
      }
      return respond(conn ? 200 : 404, {});
    }
    if (url.pathname.startsWith("/api/providers/") && req.method === "DELETE") {
      if (url.pathname.endsWith("/connection-one") && state.holdDelete) await new Promise(resolve => { state.releaseDelete = resolve; });
      if (url.pathname.endsWith(`/${state.failDeleteId}`)) return respond(500, { error: "Synthetic delete failure" });
      state.connections = state.connections.filter(c => c.id !== decodeURIComponent(url.pathname.split("/").pop()));
      return respond(200, {});
    }
    return respond(404, {});
  }
  if (url.pathname === "/fixture-observer") { res.writeHead(200, { "content-type": "text/html" }); return res.end("<!doctype html><title>Synthetic observer</title>"); }
  const relative = url.pathname === "/login" || url.pathname === "/callback" || url.pathname.startsWith("/dashboard") || url.pathname === "/" ? "index.html" : url.pathname.slice(1);
  const file = path.resolve(publicDir, relative);
  if (!file.startsWith(publicDir + path.sep)) return res.writeHead(403).end();
  try { const content = await fs.readFile(file); res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" }); res.end(content); }
  catch { res.writeHead(404).end(); }
});

async function waitForFixture(predicate) {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error("synthetic fixture action timed out");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

async function runXaiForceFixture(page, origin) {
  state.xaiForceOnly = true;
  state.expired = false;
  state.connections = [
    { id: "inactive", provider: "xai", name: "Inactive", isActive: false, priority: -100 },
    { id: "canonical /?", provider: "xai", name: "Canonical first", authType: "apikey", priority: 90 },
    { id: "priority-first", provider: "xai", name: "Priority first", authType: "apikey", isActive: true, priority: 0 },
    { id: "other-provider", provider: "codex", name: "Other provider", isActive: true },
  ];
  state.custom = [
    { providerAlias: "xai", id: "saved-custom", name: "Saved custom", type: "llm", opencode: { limit: { context: 123456 } } },
    { providerAlias: "xai", id: "saved-disabled", name: "Saved disabled", type: "llm" },
    { providerAlias: "cx", id: "other-custom", type: "llm" },
  ];
  state.aliases = { personal: "xai/saved-custom", other: "cx/other-custom" };
  state.disabled = { xai: ["saved-disabled"], cx: ["other-disabled"] };
  state.favorites = { xai: ["saved-custom"], cx: ["other-custom"] };
  state.pools = [];
  state.xaiCatalogRows = [{ id: "grok-old", kind: "llm" }, { id: "saved-disabled", kind: "llm" }];
  state.xaiNextCatalogRows = [{ id: "grok-new", kind: "llm" }, { id: "saved-disabled", kind: "llm" }];
  state.writes = [];
  state.importGets = [];
  state.xaiAuxReads = [];
  state.releaseCatalog = null;
  state.holdCatalogOnce = true;
  const code = id => page.locator("code").filter({ hasText: new RegExp(`^xai/${id}$`) });
  const refresh = () => page.getByRole("button", { name: "Refresh models", exact: true });
  const pending = () => page.getByRole("button", { name: "Refreshing models...", exact: true });
  const alert = message => page.getByRole("alert").filter({ hasText: message });
  const saved = () => ({ custom: structuredClone(state.custom), aliases: structuredClone(state.aliases), disabled: structuredClone(state.disabled), favorites: structuredClone(state.favorites) });
  const initialSaved = saved();

  await page.goto(`${origin}/dashboard/providers/xai`);
  await waitForFixture(() => typeof state.releaseCatalog === "function");
  await pending().waitFor();
  assert.equal(await pending().isDisabled(), true, "initial catalog loading disables force refresh");
  await page.getByRole("status").filter({ hasText: "Loading xAI model catalog..." }).waitFor();
  assert.deepEqual(state.importGets, [], "page entry does not force a connection");
  state.releaseCatalog();
  await refresh().waitFor();
  await code("grok-old").waitFor();
  await page.getByText("Saved custom", { exact: true }).waitFor();

  // Keep the force in flight while a separate connection edit triggers an old
  // catalog snapshot. The later post-force projection must beat that snapshot.
  state.holdXaiForce = true; state.releaseXaiForce = null;
  state.holdXaiNodeOnce = true; state.releaseXaiNode = null;
  state.pools = [{ id: "fixture-pool", name: "Fixture pool", isActive: true }];
  const readsBefore = state.xaiConnectionReads;
  const auxiliaryBefore = state.xaiAuxReads.length;
  await refresh().click();
  await waitForFixture(() => typeof state.releaseXaiNode === "function");
  assert.deepEqual(state.importGets, [], "force waits for the connection refresh's auxiliary reads");
  await pending().waitFor();
  state.releaseXaiNode();
  await waitForFixture(() => typeof state.releaseXaiForce === "function");
  await pending().waitFor();
  assert.equal(await pending().isDisabled(), true);
  await pending().evaluate(button => button.click());
  assert.equal(state.xaiConnectionReads, readsBefore + 1, "force reads canonical connections again");
  assert.match(state.xaiConnectionHeaders.at(-1) || "", /no-cache/, "canonical read bypasses cache");
  assert.deepEqual(state.importGets, ["/api/providers/canonical%20%2F%3F/models"], "first active canonical source wins over priority; ID is encoded");
  assert.match(state.xaiForceHeaders[0]["cache-control"] || "", /no-cache/, "force discovery bypasses cache");
  const auxiliary = state.xaiAuxReads.slice(auxiliaryBefore);
  assert.deepEqual(auxiliary.map(read => read.path).sort(), ["/api/provider-nodes", "/api/proxy-pools?isActive=true", "/api/settings"]);
  assert.ok(auxiliary.every(read => /no-cache/.test(read.cache || "")), "connection refresh auxiliary reads bypass cache");
  await page.getByRole("button", { name: /Apply Proxy/ }).waitFor();
  assert.deepEqual(state.writes, [], "discovery performs no user-data writes");

  await page.getByRole("button", { name: /Add Model/ }).click();
  const dialog = page.getByRole("dialog", { name: "Add Custom Model" });
  await dialog.getByPlaceholder("e.g. claude-opus-4-5").fill("concurrent-custom");
  await dialog.getByRole("button", { name: "Add Model" }).click();
  await dialog.waitFor({ state: "detached" });
  await code("concurrent-custom").waitFor();
  const afterConcurrentEdit = saved();
  assert.deepEqual(afterConcurrentEdit.custom.slice(0, initialSaved.custom.length), initialSaved.custom);
  assert.deepEqual(afterConcurrentEdit.aliases, initialSaved.aliases);
  assert.deepEqual(afterConcurrentEdit.disabled, initialSaved.disabled);
  assert.deepEqual(afterConcurrentEdit.favorites, initialSaved.favorites);

  state.holdCatalogOnce = true; state.releaseCatalog = null;
  const priorityRow = page.getByRole("checkbox", { name: "Select connection Priority first" }).locator("xpath=../..");
  await priorityRow.getByRole("switch", { name: "Disable connection" }).click();
  await waitForFixture(() => typeof state.releaseCatalog === "function");
  const heldCatalogs = state.catalogRequests;
  const forceResponse = page.waitForResponse(response => response.url().includes("/api/providers/canonical%20%2F%3F/models"));
  state.releaseXaiForce(); state.holdXaiForce = false;
  await forceResponse;
  await page.waitForTimeout(100);
  assert.equal(state.catalogRequests, heldCatalogs, "post-force reload waits behind the older catalog read");
  assert.equal(await pending().isDisabled(), true, "all reload waiters include the queued follow-up");
  state.releaseCatalog();
  await code("grok-new").waitFor();
  await refresh().waitFor();
  await page.waitForTimeout(100);
  assert.equal(state.catalogRequests, heldCatalogs + 1, "post-force invalidation coalesces to one follow-up");
  assert.equal(await code("grok-old").count(), 0, "late older catalog cannot replace post-force catalog");
  assert.equal(await code("must-not-auto-import").count(), 0, "force response is not imported");
  await code("concurrent-custom").waitFor();
  await page.getByTitle("Restore model").filter({ hasText: "saved-disabled" }).waitFor();
  assert.deepEqual(saved(), afterConcurrentEdit, "catalog projection preserves concurrent and pre-existing overlays");

  state.xaiForceStatus = 503;
  const failedCatalogs = state.catalogRequests;
  await refresh().click();
  await alert("xAI model refresh failed (503). Showing the last available catalog.").waitFor();
  await refresh().waitFor();
  assert.ok(state.catalogRequests > failedCatalogs, "failed force still reloads catalog");
  await code("grok-new").waitFor();
  assert.deepEqual(saved(), afterConcurrentEdit);

  state.xaiForceStatus = 0; state.failCatalog = true;
  await refresh().click();
  await alert("Failed to load model catalog (500)").waitFor();
  await refresh().waitFor();
  await code("grok-new").waitFor();
  assert.deepEqual(saved(), afterConcurrentEdit, "a failed catalog read retains last-good rows and saved data");
  state.failCatalog = false;
  await refresh().click();
  await refresh().waitFor();
  assert.equal(await alert("Failed to load model catalog").count(), 0, "a later catalog success clears its error");

  const malformedCatalog = route => route.fulfill({ status: 200, contentType: "application/json", body: "not-json" });
  const parseMessage = await page.evaluate(() => { try { JSON.parse("not-json"); } catch (error) { return error.message; } });
  await page.route("**/api/catalog", malformedCatalog);
  await refresh().click();
  await alert(parseMessage).waitFor();
  await refresh().waitFor();
  await code("grok-new").waitFor();
  assert.deepEqual(saved(), afterConcurrentEdit, "malformed catalog preserves the last-good projection and saved models");
  await page.unroute("**/api/catalog", malformedCatalog);
  await refresh().click();
  await refresh().waitFor();
  assert.equal(await alert(parseMessage).count(), 0);

  const sourcePattern = /\/api\/providers\/canonical%20%2F%3F\/models$/;
  const failNetwork = route => route.abort("failed");
  await page.route(sourcePattern, failNetwork);
  const networkCatalogs = state.catalogRequests;
  await refresh().click();
  await alert("Failed to fetch").waitFor();
  await refresh().waitFor();
  assert.ok(state.catalogRequests > networkCatalogs, "network failure still reloads catalog");
  await page.unroute(sourcePattern, failNetwork);
  state.xaiForceStatus = 0;
  await refresh().click();
  await refresh().waitFor();
  assert.equal(await alert("xAI model refresh failed").count(), 0, "success clears the force warning");

  // A later connection operation supersedes a held canonical read. The stale
  // force must not discover from the captured old connection vector.
  state.holdConnectionsOnce = true; state.releaseConnections = null;
  await refresh().click();
  await waitForFixture(() => typeof state.releaseConnections === "function");
  const canonicalRow = page.getByRole("checkbox", { name: "Select connection Canonical first" }).locator("xpath=../..");
  await canonicalRow.getByRole("switch", { name: "Disable connection" }).click();
  await refresh().waitFor();
  const supersededSources = state.importGets.length;
  const supersededCatalogs = state.catalogRequests;
  state.releaseConnections();
  await waitForFixture(() => state.catalogRequests > supersededCatalogs);
  await page.waitForTimeout(100);
  assert.equal(state.importGets.length, supersededSources, "superseded canonical read cannot force its stale source");
  assert.equal(await alert("Could not load xAI connections").count(), 0, "superseded failure does not replace the latest state");

  state.connections.filter(c => c.provider === "xai").forEach(c => { c.isActive = false; });
  const beforeNoActive = state.importGets.length;
  const noActiveCatalogs = state.catalogRequests;
  await refresh().click();
  await alert("No active xAI connection. Add or enable a connection to discover models.").waitFor();
  assert.equal(state.importGets.length, beforeNoActive, "no fallback source is forced");
  assert.ok(state.catalogRequests > noActiveCatalogs);
  await code("concurrent-custom").waitFor();
  assert.deepEqual(saved(), afterConcurrentEdit);

  for (const failure of ["status", "malformed"]) {
    state.xaiConnectionStatus = failure === "status" ? 503 : 0;
    state.xaiMalformedConnections = failure === "malformed";
    const beforeCatalogs = state.catalogRequests;
    await refresh().click();
    await alert("Could not load xAI connections. Try refreshing again.").waitFor();
    await refresh().waitFor();
    assert.ok(state.catalogRequests > beforeCatalogs, "connection read failure still reloads catalog");
    assert.equal(state.importGets.length, beforeNoActive, "failed canonical reads never force a source");
    assert.deepEqual(saved(), afterConcurrentEdit);
  }
  state.xaiConnectionStatus = 0; state.xaiMalformedConnections = false;
  state.connections.find(c => c.id === "canonical /?").isActive = true;
  state.xaiForceStatus = 401;
  const authRequests = state.authStatusRequests;
  await refresh().click();
  await page.waitForURL(`${origin}/login`);
  assert.ok(state.authStatusRequests > authRequests, "401 force rechecks the session");
  assert.deepEqual(saved(), afterConcurrentEdit);
}

async function runContextFixture(page, origin) {
  state.connections = [];
  state.writes = [];
  state.settings = {
    providerContextLimits: { codex: 430000, "opencode-go": 210000, glm: 250000, claude: 600000 },
    otherSetting: { untouched: true },
  };
  const context = () => page.getByLabel("Advertised Context");
  const title = "Context metadata advertised to clients; a model's smaller native context window still applies. Proxy memory byte limits are separate.";
  for (const [provider, value] of [["opencode-go", 210000], ["glm", 250000], ["claude", 600000]]) {
    await page.goto(`${origin}/dashboard/providers/${provider}`);
    await context().waitFor();
    await page.waitForFunction(value => {
      const input = document.querySelector('input[type="number"]');
      return input?.value === String(value) && !input.disabled;
    }, value);
    assert.equal(await context().locator("xpath=..").getAttribute("title"), title);
  }

  await page.goto(`${origin}/dashboard/providers/glm`);
  await context().waitFor();
  await page.waitForFunction(() => document.querySelector('input[type="number"]')?.value === "250000" && !document.querySelector('input[type="number"]')?.disabled);
  const catalogs = state.catalogRequests;
  await context().fill("275000");
  await context().press("Enter");
  await waitForFixture(() => state.settings.providerContextLimits.glm === 275000 && state.catalogRequests > catalogs);
  assert.deepEqual(state.writes.filter(w => w.path === "/api/settings").at(-1).body, {
    providerContextLimits: { codex: 430000, "opencode-go": 210000, glm: 275000, claude: 600000 },
  });
  assert.deepEqual(state.settings.otherSetting, { untouched: true });
  await page.reload();
  await page.waitForFunction(() => document.querySelector('input[type="number"]')?.value === "275000" && !document.querySelector('input[type="number"]')?.disabled);

  await page.goto(`${origin}/dashboard/providers/opencode-zen`);
  await page.getByText("No authentication required", { exact: true }).waitFor();
  assert.equal(await context().count(), 0, "no-auth providers do not show the context control");
  await page.goto(`${origin}/dashboard/providers/kilocode`);
  await page.getByRole("heading", { name: "Available Models" }).waitFor();
  assert.equal(await context().count(), 0, "unsupported providers do not show the context control");
}

async function runCodexControlsFixture(page, origin) {
  state.expired = false;
  state.writes = [];
  state.codexImportCount = 0;
  state.connections = [];
  state.settings = {
    providerContextLimits: { codex: 430000, glm: 250000, claude: 600000 },
    codexAutoPing: { enabled: false, connections: { orphan: false } },
    glmAutoPing: { enabled: false, connections: {} },
    otherSetting: { untouched: true },
  };
  const settingsWrites = () => state.writes.filter(w => w.path === "/api/settings" && w.method === "PATCH");
  const importWrites = () => state.writes.filter(w => w.path === "/api/oauth/codex/bulk-import");
  const context = () => page.getByLabel("Advertised Context");
  const dialog = () => page.getByRole("dialog", { name: "Bulk Add Codex Accounts" });
  const field = () => dialog().getByLabel("Codex accounts JSON");
  const open = async () => { await page.getByRole("button", { name: /playlist_add Bulk Add/ }).click(); await dialog().waitFor(); };
  const submit = async input => { await field().fill(input); await dialog().getByRole("button", { name: "Import All" }).click(); };

  await page.goto(`${origin}/dashboard/providers/codex`);
  await page.getByText("No connections yet").waitFor();
  const link = page.getByRole("link", { name: /Sign up \/ Learn more/ });
  assert.equal(await link.getAttribute("href"), "https://chatgpt.com/codex");
  assert.equal(await link.getAttribute("target"), "_blank");
  assert.equal(await link.getAttribute("rel"), "noopener noreferrer");
  await context().waitFor();
  await page.waitForFunction(() => document.querySelector('input[type="number"]')?.value === "430000" && !document.querySelector('input[type="number"]')?.disabled);
  assert.match(await context().locator("xpath=..").getAttribute("title"), /50,000 input tokens.*128,000 output tokens/);
  for (const invalid of ["0", "1000001", "1.5", ""]) {
    await context().fill(invalid);
    await context().blur();
    await page.getByText("Context limit must be a whole number from 1 to 1,000,000", { exact: true }).last().waitFor();
    assert.equal(await context().inputValue(), "430000");
  }
  assert.equal(settingsWrites().length, 0, "invalid limits never write");
  state.settings.providerContextLimits.glm = 275000; // another writer completed after page load
  state.holdSettingsWrite = true; state.releaseSettingsWrite = null;
  const catalogs = state.catalogRequests;
  await context().fill("4.5e5");
  await context().press("Enter");
  await waitForFixture(() => typeof state.releaseSettingsWrite === "function");
  assert.equal(await context().isDisabled(), true);
  assert.deepEqual(settingsWrites().at(-1).body, { providerContextLimits: { codex: 450000, glm: 275000, claude: 600000 } });
  state.releaseSettingsWrite();
  await page.waitForFunction(() => document.querySelector('input[type="number"]')?.value === "450000" && !document.querySelector('input[type="number"]')?.disabled);
  await waitForFixture(() => state.catalogRequests > catalogs);
  assert.deepEqual(state.settings.codexAutoPing, { enabled: false, connections: { orphan: false } });
  assert.deepEqual(state.settings.otherSetting, { untouched: true });
  await page.reload();
  await page.waitForFunction(() => document.querySelector('input[type="number"]')?.value === "450000" && !document.querySelector('input[type="number"]')?.disabled);
  state.failSettingsRead = true;
  const writesBeforeReadFailure = settingsWrites().length;
  await context().fill("460000"); await context().blur();
  await page.getByText("Failed to load context limits", { exact: true }).last().waitFor();
  assert.equal(await context().inputValue(), "450000");
  assert.equal(settingsWrites().length, writesBeforeReadFailure);
  state.failSettingsRead = false;
  state.rejectSettingsWrite = true;
  await context().fill("460000"); await context().blur();
  await page.getByText("Failed to save context limit", { exact: true }).last().waitFor();
  assert.equal(await context().inputValue(), "450000");
  assert.equal(state.settings.providerContextLimits.codex, 450000);
  state.rejectSettingsWrite = false;
  // A lost response may still have committed: a later edit re-reads the map.
  const loseSettingsAck = async route => {
    if (route.request().method() !== "PATCH") return route.fallback();
    await route.fetch(); await route.abort("failed");
  };
  await page.route("**/api/settings", loseSettingsAck);
  await context().fill("460000"); await context().blur();
  await waitForFixture(() => state.settings.providerContextLimits.codex === 460000);
  await page.waitForFunction(() => document.querySelector('input[type="number"]')?.value === "450000" && !document.querySelector('input[type="number"]')?.disabled);
  await page.unroute("**/api/settings", loseSettingsAck);
  state.settings.providerContextLimits.glm = 285000;
  await context().fill("1"); await context().blur();
  await waitForFixture(() => state.settings.providerContextLimits.codex === 1);
  await page.waitForFunction(() => !document.querySelector('input[type="number"]')?.disabled);
  await context().fill("1000000"); await context().press("Enter");
  await waitForFixture(() => state.settings.providerContextLimits.codex === 1000000);
  await page.waitForFunction(() => !document.querySelector('input[type="number"]')?.disabled);
  await context().fill("450000"); await context().blur();
  await waitForFixture(() => state.settings.providerContextLimits.codex === 450000);
  await page.waitForFunction(() => !document.querySelector('input[type="number"]')?.disabled);
  assert.deepEqual(settingsWrites().at(-1).body, { providerContextLimits: { codex: 450000, glm: 285000, claude: 600000 } });

  await open();
  assert.equal(await dialog().getByRole("button", { name: "Import All" }).isDisabled(), true);
  await submit("{broken");
  await dialog().getByRole("alert").getByText(/^Invalid JSON:/).waitFor();
  await submit("[]");
  await dialog().getByText("No accounts found in input", { exact: true }).waitFor();
  await submit('{"accounts":[]}');
  await dialog().getByText("No accounts found in input", { exact: true }).waitFor();
  await submit("42");
  await dialog().getByText("No accounts found in input", { exact: true }).waitFor();
  assert.equal(importWrites().length, 0, "invalid or empty JSON never writes");
  await submit('[{},42]');
  await dialog().getByText(", ✗ 2 failed", { exact: true }).waitFor();
  await dialog().getByText("[0] Missing accessToken", { exact: true }).waitFor();
  await dialog().getByText("[1] Item is not an object", { exact: true }).waitFor();
  assert.deepEqual(importWrites().at(-1).body, { accounts: [{}, 42] });
  state.bulkImportStatus = 400;
  await submit('{"accessToken":"synthetic-access-one"}');
  await dialog().getByText("Synthetic import failure", { exact: true }).waitFor();
  state.bulkImportStatus = 0;
  state.bulkImportMalformed = true;
  await submit('{"accessToken":"synthetic-access-one"}');
  await dialog().getByText("Invalid import response; refresh connections before retrying.", { exact: true }).waitFor();
  state.bulkImportMalformed = false;
  await dialog().getByRole("button", { name: "Close", exact: true }).last().click();
  await dialog().waitFor({ state: "detached" });
  await open();
  assert.equal(await field().inputValue(), "", "closing discards the draft and result");

  const importCatalogs = state.catalogRequests;
  state.holdCodexImport = true; state.releaseCodexImport = null;
  await submit('{"accounts":[{"auth_mode":"chatgpt","tokens":{"access_token":"synthetic-access-two"}},{}]}\nfixture@terminal:~$');
  await waitForFixture(() => typeof state.releaseCodexImport === "function");
  assert.equal(await field().isDisabled(), true);
  assert.equal(await dialog().getByRole("button", { name: "Close", exact: true }).last().isDisabled(), true);
  const heldImportCount = importWrites().length;
  await dialog().getByRole("button", { name: "Importing..." }).evaluate(element => element.click());
  await dialog().getByRole("button", { name: "Close", exact: true }).first().click();
  await page.keyboard.press("Escape");
  await dialog().waitFor();
  assert.equal(importWrites().length, heldImportCount, "in-flight imports cannot be submitted twice");
  assert.deepEqual(importWrites().at(-1).body, { accounts: [{ auth_mode: "chatgpt", tokens: { access_token: "synthetic-access-two" } }, {}] });
  state.releaseCodexImport();
  await dialog().waitFor({ state: "detached" });
  await page.getByText("Imported 1", { exact: true }).waitFor();
  await waitForFixture(() => state.catalogRequests > importCatalogs);
  assert.equal(await page.getByRole("button", { name: /playlist_add Bulk Add/ }).getAttribute("title"), "Bulk import codex accounts from JSON");

  await open();
  await submit('{"accessToken":"synthetic-access-three"}');
  await dialog().waitFor({ state: "detached" });
  await page.getByText("Imported 2", { exact: true }).waitFor();
  assert.deepEqual(importWrites().at(-1).body, { accounts: [{ accessToken: "synthetic-access-three" }] });
  await page.reload();
  await page.getByText("Imported 1", { exact: true }).waitFor();
  await page.getByText("Imported 2", { exact: true }).waitFor();
  assert.equal(await context().inputValue(), "450000");
  assert.deepEqual(state.settings.codexAutoPing, { enabled: false, connections: { orphan: false } });
  assert.ok(state.writes.every(w => ["/api/settings", "/api/oauth/codex/bulk-import"].includes(w.path)), "only synthetic settings and import writes");

  state.rejectSettingsWrite = 401;
  await context().fill("470000"); await context().blur();
  await page.waitForURL(`${origin}/login`);
  assert.equal(state.settings.providerContextLimits.codex, 450000, "unauthorized context writes do not mutate settings");
  state.rejectSettingsWrite = false; state.expired = false;
  await page.goto(`${origin}/dashboard/providers/codex`);
  await page.getByText("Imported 2", { exact: true }).waitFor();
  await open();
  state.bulkImportStatus = 401;
  await submit('{"accessToken":"synthetic-access-four"}');
  await page.waitForURL(`${origin}/login`);
  assert.equal(state.codexImportCount, 2, "unauthorized import does not create connections");
  state.bulkImportStatus = 0;
}

async function runAutoPingFixture(page, origin) {
  state.expired = false;
  state.writes = [];
  state.connections = [
    { id: "codex-one", provider: "codex", name: "Codex One", authType: "oauth", isActive: true },
    { id: "codex-two", provider: "codex", name: "Codex Two", authType: "oauth", isActive: true },
    { id: "codex-nameless", provider: "codex", authType: "oauth", isActive: true },
    { id: "codex-wrong", provider: "codex", name: "Codex Wrong", authType: "OAuth", isActive: true },
    { id: "codex-missing", provider: "codex", name: "Codex Missing", isActive: true },
    { id: "glm-one", provider: "glm", name: "GLM One", authType: " API_KEY ", isActive: true },
    { id: "glm-two", provider: "glm", name: "GLM Two", authType: "ApiKey", isActive: true },
    { id: "glm-wrong", provider: "glm", name: "GLM Wrong", authType: "oauth", isActive: true },
    { id: "glm-cn-one", provider: "glm-cn", name: "GLM CN", authType: "apikey", isActive: true },
  ];
  state.settings = {
    codexAutoPing: { enabled: false, "codex-one": true, "codex-two": false, "orphan-codex": true },
    glmAutoPing: { enabled: true, connections: { "glm-one": false, "glm-two": false, "orphan-glm": false, invalid: "not boolean" } },
    otherSetting: { untouched: true },
  };
  const writes = () => state.writes.filter(w => w.path === "/api/settings" && w.method === "PATCH");
  const button = (verb, name) => page.getByRole("button", { name: `${verb} auto-ping for ${name}`, exact: true });
  const allPing = () => page.getByRole("button", { name: /^(Enable|Disable) auto-ping for / });
  state.holdSettingsWrite = true; state.releaseSettingsWrite = null;
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`${origin}/dashboard/providers/codex`);
  await page.getByText("Codex One", { exact: true }).waitFor();
  await waitForFixture(() => typeof state.releaseSettingsWrite === "function");
  assert.equal(await allPing().count(), 0, "controls wait for both settings migrations");
  state.releaseSettingsWrite();
  await button("Disable", "Codex One").waitFor();
  await button("Enable", "Codex Two").waitFor();
  await button("Enable", "OAuth Account").waitFor();
  assert.equal(await allPing().count(), 3, "Codex requires the exact oauth auth type");
  assert.deepEqual(writes().map(w => w.body), [
    { codexAutoPing: { enabled: true, connections: { "codex-one": true, "codex-two": false, "orphan-codex": true } } },
    { glmAutoPing: { enabled: false, connections: { "glm-one": false, "glm-two": false, "orphan-glm": false } } },
  ]);
  assert.deepEqual(state.settings.otherSetting, { untouched: true });
  assert.equal(await button("Disable", "Codex One").getAttribute("aria-pressed"), "true");
  const tooltip = button("Disable", "Codex One").locator("xpath=..").getByText("Auto-starts the next available Codex quota window after reset with a tiny gpt-6-luna request. Consumes a small amount of quota.", { exact: true });
  await button("Disable", "Codex One").hover();
  await tooltip.waitFor();
  await page.reload();
  await button("Enable", "Codex Two").waitFor();
  assert.equal(writes().length, 2, "canonical settings do not migrate again");

  // A completed external change since loading is merged; only one same-provider
  // toggle may be in flight, even if another DOM click is attempted.
  state.settings.codexAutoPing.connections.external = true;
  state.holdSettingsWrite = true; state.releaseSettingsWrite = null;
  await button("Enable", "Codex Two").click();
  await waitForFixture(() => typeof state.releaseSettingsWrite === "function");
  const saving = button("Disable", "Codex Two");
  assert.equal(await saving.getAttribute("aria-pressed"), "true");
  assert.equal(await saving.isDisabled(), true);
  assert.equal(await button("Disable", "Codex One").isDisabled(), true);
  await button("Disable", "Codex One").evaluate(el => el.click());
  assert.equal(writes().length, 3);
  assert.deepEqual(writes().at(-1).body, { codexAutoPing: { enabled: true, connections: { "codex-one": true, "codex-two": true, "orphan-codex": true, external: true } } });
  state.releaseSettingsWrite();
  await page.waitForFunction(() => !document.querySelector('button[aria-label="Disable auto-ping for Codex Two"]')?.disabled);

  state.rejectSettingsWrite = true;
  await button("Disable", "Codex Two").click();
  await page.getByText("Failed to update settings: Service Unavailable", { exact: true }).waitFor();
  await button("Disable", "Codex Two").waitFor();
  assert.equal(await button("Disable", "Codex Two").getAttribute("aria-pressed"), "true", "failed writes roll back");
  state.rejectSettingsWrite = false;
  state.failSettingsRead = true;
  const beforeFailedRead = writes().length;
  await button("Disable", "Codex Two").click();
  await page.getByText("Failed to load auto-ping settings", { exact: true }).waitFor();
  await button("Disable", "Codex Two").waitFor();
  assert.equal(writes().length, beforeFailedRead, "failed re-read cannot issue a stale PATCH");
  state.failSettingsRead = false;

  // The backend may commit even when its response is lost. A later toggle
  // rereads the stored map rather than restoring an older connection value.
  const lostAck = async route => {
    if (route.request().method() !== "PATCH") return route.fallback();
    await route.fetch(); await route.abort("failed");
  };
  await page.route("**/api/settings", lostAck);
  await button("Disable", "Codex Two").click();
  await page.getByText("Failed to fetch", { exact: true }).waitFor();
  await button("Disable", "Codex Two").waitFor();
  assert.equal(state.settings.codexAutoPing.connections["codex-two"], false);
  await page.unroute("**/api/settings", lostAck);
  await button("Disable", "Codex One").click();
  await button("Enable", "Codex One").waitFor();
  await button("Enable", "Codex Two").waitFor();
  assert.deepEqual(writes().at(-1).body, { codexAutoPing: { enabled: true, connections: { "codex-one": false, "codex-two": false, "orphan-codex": true, external: true } } });

  await page.goto(`${origin}/dashboard/providers/glm`);
  await button("Enable", "GLM One").waitFor();
  assert.equal(await allPing().count(), 2, "GLM accepts trimmed, case-insensitive apikey and api_key only");
  await button("Enable", "GLM One").locator("xpath=..").getByText("Auto-starts the next GLM 5-hour window after reset with a tiny glm-5.3-flash request. Consumes a small amount of Coding Plan quota.", { exact: true }).waitFor();
  await button("Enable", "GLM One").focus();
  await page.keyboard.press("Space");
  await button("Disable", "GLM One").waitFor();
  await page.waitForFunction(() => !document.querySelector('button[aria-label="Disable auto-ping for GLM One"]')?.disabled);
  assert.deepEqual(writes().at(-1).body, { glmAutoPing: { enabled: true, connections: { "glm-one": true, "glm-two": false, "orphan-glm": false } } });
  await button("Disable", "GLM One").click();
  await button("Enable", "GLM One").waitFor();
  await page.waitForFunction(() => !document.querySelector('button[aria-label="Enable auto-ping for GLM One"]')?.disabled);
  assert.deepEqual(writes().at(-1).body, { glmAutoPing: { enabled: false, connections: { "glm-one": false, "glm-two": false, "orphan-glm": false } } });
  await page.goto(`${origin}/dashboard/providers/glm-cn`);
  await page.getByText("GLM CN", { exact: true }).waitFor();
  assert.equal(await allPing().count(), 0);

  state.settings.glmAutoPing = { "glm-one": true, "orphan-glm": false };
  state.rejectSettingsWrite = true;
  await page.goto(`${origin}/dashboard/providers/glm`);
  await page.getByText("Auto-ping settings unavailable: Failed to update settings: Service Unavailable", { exact: true }).waitFor();
  assert.equal(await allPing().count(), 0, "failed migration never exposes settings as ready");
  assert.deepEqual(state.settings.glmAutoPing, { "glm-one": true, "orphan-glm": false });
  state.rejectSettingsWrite = false;
  state.failSettingsRead = true;
  await page.goto(`${origin}/dashboard/providers/codex`);
  await page.getByText("Auto-ping settings unavailable: Failed to load auto-ping settings", { exact: true }).waitFor();
  assert.equal(await allPing().count(), 0);
  state.failSettingsRead = false;
  state.settings.glmAutoPing = { enabled: false, connections: {} };
  await page.goto(`${origin}/dashboard/providers/codex`);
  await button("Enable", "Codex One").waitFor();
  const beforeUnauthorized = structuredClone(state.settings);
  state.rejectSettingsWrite = 401;
  await button("Enable", "Codex One").click();
  await page.waitForURL(`${origin}/login`);
  assert.deepEqual(state.settings, beforeUnauthorized, "unauthorized writes do not change settings");
  state.rejectSettingsWrite = false;
  assert.ok(state.writes.every(w => w.method === "PATCH" && w.path === "/api/settings"), "fixture never initiates provider actions");
}

async function runConnectionFixture(page, origin) {
  state.connections = [
    { id: "proxy-bound", provider: "kilocode", name: "Primary", email: " \uFEFFfirst@example.invalid\uFEFF ", displayName: "Unused label", globalPriority: 7, isActive: true, priority: 1, providerSpecificData: { proxyPoolId: "pool-a" } },
    { id: "proxy-legacy", provider: "kilocode", name: "Legacy", email: " Legacy ", displayName: " \uFEFFFallback label\uFEFF ", globalPriority: 0, isActive: true, priority: 2, providerSpecificData: { connectionProxyEnabled: true, connectionProxyUrl: "http://fixture-user:fixture-password@127.0.0.1:9999/?token=fixture-query", connectionNoProxy: "localhost" } },
    { id: "no-name", provider: "kilocode", name: " ", email: "  third@example.invalid  ", displayName: "Ignored without name", isActive: true, priority: 3 },
    { id: "missing-pool", provider: "kilocode", name: "Missing Pool", isActive: true, priority: 4, providerSpecificData: { proxyPoolId: "removed-pool" } },
  ];
  state.pools = [{ id: "pool-a", name: "Pool A", isActive: true }, { id: "pool-b", name: "Pool B", isActive: true }];
  await page.goto(`${origin}/dashboard/providers/kilocode`);
  const row = name => page.getByText(name, { exact: true }).locator("xpath=../../..");
  const bound = row("Primary");
  const legacy = row("Legacy");
  const missing = row("Missing Pool");
  await bound.getByText("first@example.invalid", { exact: true }).waitFor();
  assert.equal(await bound.getByText("Unused label", { exact: true }).count(), 0);
  assert.equal(await bound.getByText("first@example.invalid", { exact: true }).getAttribute("class"), "truncate text-xs text-text-muted");
  await bound.getByText("Auto: 7", { exact: true }).waitFor();
  await legacy.getByText("Fallback label", { exact: true }).waitFor();
  assert.equal(await legacy.getByText(/Auto:/).count(), 0);
  const noName = row("third@example.invalid");
  assert.equal(await noName.getByText("Ignored without name", { exact: true }).count(), 0);
  await page.getByRole("button", { name: /Apply Proxy/ }).waitFor();
  const label = bound.getByTitle("Pool: Pool A");
  await label.waitFor();
  assert.deepEqual(await label.evaluate(el => [...el.childNodes].map(node => [node.nodeType, node.nodeValue])), [[3, "Pool: Pool A"]]);
  const badge = parent => parent.locator("span.rounded-full").filter({ hasText: /^Proxy$/ });
  const proxyButton = parent => parent.getByText("lan", { exact: true }).locator("xpath=..");
  assert.match(await badge(bound).getAttribute("class"), /bg-success-bg text-success-text/);
  assert.match(await proxyButton(bound).getAttribute("class"), /\btext-primary\b/);
  assert.match(await badge(legacy).getAttribute("class"), /text-\[color:var\(--color-danger\)\]/);
  assert.match(await proxyButton(legacy).getAttribute("class"), /\btext-primary\b/);
  await legacy.getByTitle("Legacy: http://127.0.0.1:9999").waitFor();
  await legacy.getByTitle("localhost").waitFor();
  assert.equal((await legacy.innerHTML()).includes("fixture-password"), false);
  assert.equal((await legacy.innerHTML()).includes("fixture-query"), false);
  await missing.getByTitle("Pool: removed-pool (inactive/missing)").waitFor();
  assert.match(await badge(missing).getAttribute("class"), /text-\[color:var\(--color-danger\)\]/);
  assert.match(await proxyButton(missing).getAttribute("class"), /\btext-primary\b/);
  await noName.getByRole("button", { name: /delete Delete/ }).click();
  await page.getByRole("dialog", { name: "Delete connection" }).getByRole("button", { name: "Delete" }).click();
  await page.getByRole("dialog", { name: "Delete connection" }).waitFor({ state: "detached" });
  await page.getByText("third@example.invalid", { exact: true }).waitFor({ state: "detached" });
  await bound.getByText("keyboard_arrow_down", { exact: true }).locator("xpath=..").click();
  await waitForFixture(() => state.writes.filter(w => w.method === "PUT" && ["/api/providers/proxy-bound", "/api/providers/proxy-legacy"].includes(w.path)).length === 2);
  await bound.getByText("keyboard_arrow_up", { exact: true }).locator("xpath=..").click();
  await waitForFixture(() => state.writes.filter(w => w.method === "PUT" && ["/api/providers/proxy-bound", "/api/providers/proxy-legacy"].includes(w.path)).length === 4);
  await proxyButton(bound).click();
  assert.match(await bound.getByRole("button", { name: "Pool A", exact: true }).getAttribute("class"), /font-medium text-primary/);
  await bound.getByRole("button", { name: "Pool B", exact: true }).click();
  await bound.getByTitle("Pool: Pool B").waitFor();
  assert.deepEqual(state.writes.filter(w => w.path === "/api/providers/proxy-bound" && w.method === "PUT").at(-1).body, { proxyPoolId: "pool-b" });
  await page.getByRole("button", { name: /Apply Proxy/ }).click();
  const modal = page.getByRole("dialog", { name: "Apply Proxy (3 connections)" });
  await modal.getByRole("button", { name: /None \(unbind all\)/ }).click();
  await modal.waitFor({ state: "detached" });
  assert.match(await proxyButton(bound).getAttribute("class"), /text-text-muted hover:text-primary/);
  assert.equal(await badge(bound).count(), 0);
  assert.match(await proxyButton(legacy).getAttribute("class"), /\btext-primary\b/);
  assert.deepEqual(state.writes.filter(w => w.method === "PUT" && w.body && Object.hasOwn(w.body, "proxyPoolId")).slice(-3).map(w => w.body), Array(3).fill({ proxyPoolId: null }));
}

async function runAuthFixture(page, origin) {
  state.authOnly = true;
  state.connections = [
    { id: "oauth-first", provider: "xai", name: "OAuth First", authType: "oauth", isActive: true },
    { id: "oauth-second", provider: "xai", name: "OAuth Second", authType: "oauth", isActive: true },
    { id: "kilo-existing", provider: "kilocode", name: "Kilo Existing", authType: "oauth", isActive: true },
  ];
  await page.goto(`${origin}/dashboard/providers/xai`);
  const row = name => page.getByText(name, { exact: true }).locator("xpath=../../..");
  await row("OAuth First").getByRole("button", { name: /edit Edit/ }).click();
  let edit = page.getByRole("dialog", { name: "Edit Connection" });
  assert.equal(await edit.getByLabel("API Key").count(), 0);
  state.holdEditTest = true;
  await edit.getByRole("button", { name: "Test Connection", exact: true }).click();
  await waitForFixture(() => typeof state.releaseEditTest === "function");
  assert.equal(await edit.getByRole("button", { name: "Testing..." }).isDisabled(), true);
  await edit.getByRole("button", { name: "Close" }).click();
  await edit.waitFor({ state: "detached" });
  await row("OAuth Second").getByRole("button", { name: /edit Edit/ }).click();
  edit = page.getByRole("dialog", { name: "Edit Connection" });
  state.releaseEditTest(); state.holdEditTest = false;
  await page.waitForTimeout(150);
  assert.equal(await edit.getByText("Valid", { exact: true }).count(), 0, "a prior connection's result cannot leak into a new dialog");
  await edit.getByRole("button", { name: "Test Connection", exact: true }).click();
  await edit.getByText("Failed", { exact: true }).waitFor();
  assert.deepEqual(state.writes.filter(w => w.path.endsWith("/test") && w.path.startsWith("/api/providers/")).map(w => [w.path, w.body]), [["/api/providers/oauth-first/test", null], ["/api/providers/oauth-second/test", null]]);
  await edit.getByRole("button", { name: "Close" }).click();
  await row("OAuth First").getByRole("button", { name: /edit Edit/ }).click();
  edit = page.getByRole("dialog", { name: "Edit Connection" });
  await edit.getByRole("button", { name: "Test Connection", exact: true }).click();
  await edit.getByText("Valid", { exact: true }).waitFor();
  assert.equal(state.writes.filter(w => w.path === "/api/providers/oauth-first/test").length, 2);
  await edit.getByRole("button", { name: "Close" }).click();

  async function openProxy(provider) {
    if (provider === "xai") await page.getByRole("button", { name: /Grok Build OAuth/ }).click();
    else await page.getByRole("button", { name: /Add Connection/ }).click();
    const dialog = page.getByRole("dialog", { name: provider === "xai" ? "Connect Grok Build OAuth" : "Connect OpenAI Codex" });
    await dialog.waitFor();
    return dialog;
  }
  async function closeProxy(dialog) {
    await dialog.getByRole("button", { name: "Close" }).click();
    await dialog.waitFor({ state: "detached" });
  }
  // The backend has started the listener, but the first start response is held.
  // Final cancellation must stop the listener without waiting for that response.
  await page.goto(`${origin}/dashboard/providers/xai`);
  let initialStops = state.proxyStops || 0;
  state.holdProxyResponse = true; state.releaseProxy = null;
  let firstHeld = await openProxy("xai");
  await waitForFixture(() => typeof state.releaseProxy === "function" && state.proxyLive === true);
  await closeProxy(firstHeld);
  state.holdProxyResponse = false;
  let secondHeld = await openProxy("xai");
  await secondHeld.getByLabel("Authorization URL").waitFor();
  await closeProxy(secondHeld);
  await waitForFixture(() => state.proxyStops >= initialStops + 2 && state.proxyLive === false);
  const stopsBeforeLateResponse = state.proxyStops;
  state.releaseProxy();
  await waitForFixture(() => state.proxyStops > stopsBeforeLateResponse && state.proxyLive === false);

  // A stop that already took effect but has no response must not hang the UI,
  // and no replacement start may be sent until its acknowledgement arrives.
  async function assertStopTimeout(provider) {
    await page.goto(`${origin}/dashboard/providers/${provider}`);
    const heldStop = await openProxy(provider);
    await heldStop.getByLabel("Authorization URL").waitFor();
    state.holdStop = true; state.releaseStop = null;
    const stops = state.proxyStops;
    await closeProxy(heldStop);
    await waitForFixture(() => typeof state.releaseStop === "function" && state.proxyStops === stops + 1 && state.proxyLive === false);
    const starts = state.proxyStarts;
    const waiting = await openProxy(provider);
    await waiting.getByRole("alert").getByText("Previous proxy cleanup has not completed. Retry after it finishes.").waitFor({ timeout: 10000 });
    assert.equal(state.proxyStarts, starts, "an unacknowledged stop must fence new starts");
    state.holdStop = false; state.releaseStop();
    await page.waitForTimeout(100);
    assert.equal(state.proxyStarts, starts, "a timed-out attempt requires explicit retry");
    await waiting.getByRole("button", { name: "Retry" }).click();
    await waiting.getByLabel("Authorization URL").waitFor();
    assert.equal(state.proxyStarts, starts + 1);
    const lastStops = state.proxyStops;
    await closeProxy(waiting);
    await waitForFixture(() => state.proxyStops === lastStops + 1 && state.proxyLive === false);
  }
  await assertStopTimeout("xai");
  await assertStopTimeout("codex");

  // A gateway 504 is not a stop acknowledgement: the backend operation can
  // still complete later and kill any listener started in the meantime.
  await page.goto(`${origin}/dashboard/providers/xai`);
  const gatewayOwner = await openProxy("xai");
  await gatewayOwner.getByLabel("Authorization URL").waitFor();
  state.gatewayStopOnce = true; state.gatewayStopResponded = false;
  state.gatewayStopApplied = false; state.releaseGatewayStop = null;
  await closeProxy(gatewayOwner);
  await waitForFixture(() => state.gatewayStopResponded && typeof state.releaseGatewayStop === "function");
  assert.equal(state.proxyLive, true, "synthetic backend stop remains pending after the gateway response");
  const startsAfterGatewayTimeout = state.proxyStarts;
  const gatewayFenced = await openProxy("xai");
  await page.waitForTimeout(3300);
  assert.equal(state.proxyStarts, startsAfterGatewayTimeout, "HTTP 504 must not acknowledge the provider-wide stop");
  await gatewayFenced.getByRole("alert").getByText("Previous proxy cleanup has not completed. Retry after it finishes.").waitFor();
  state.releaseGatewayStop();
  assert.equal(state.gatewayStopApplied && !state.proxyLive, true);
  await gatewayFenced.getByRole("button", { name: "Retry" }).click();
  await gatewayFenced.getByRole("alert").waitFor({ state: "detached" });
  await gatewayFenced.getByRole("alert").getByText("Previous proxy cleanup has not completed. Retry after it finishes.").waitFor({ timeout: 10000 });
  assert.equal(state.proxyStarts, startsAfterGatewayTimeout, "retry cannot bypass an unconfirmed 504 stop");
  await closeProxy(gatewayFenced);

  // A transport error cannot prove the stop was not delivered. Fail closed
  // even if the synthetic server actually stopped before the response failed.
  await page.goto(`${origin}/dashboard/providers/xai`);
  const abortStop = async route => { await route.fetch(); await route.abort("failed"); };
  await page.route("**/api/oauth/xai/stop-proxy", abortStop);
  const uncertainStop = await openProxy("xai");
  await uncertainStop.getByLabel("Authorization URL").waitFor();
  initialStops = state.proxyStops;
  await closeProxy(uncertainStop);
  await waitForFixture(() => state.proxyStops === initialStops + 1 && state.proxyLive === false);
  const startsAfterFailure = state.proxyStarts;
  const fenced = await openProxy("xai");
  await fenced.getByRole("alert").getByText("Previous proxy cleanup has not completed. Retry after it finishes.").waitFor({ timeout: 10000 });
  assert.equal(state.proxyStarts, startsAfterFailure);
  await fenced.getByRole("button", { name: "Retry" }).click();
  await fenced.getByRole("alert").waitFor({ state: "detached" });
  await fenced.getByRole("alert").getByText("Previous proxy cleanup has not completed. Retry after it finishes.").waitFor({ timeout: 10000 });
  assert.equal(state.proxyStarts, startsAfterFailure, "retry cannot bypass an unconfirmed stop");
  await closeProxy(fenced);
  await page.unroute("**/api/oauth/xai/stop-proxy", abortStop);
  // In this fixture the synthetic stop already finished; navigation creates a
  // new page lifetime for subsequent checks, not a cross-page safety promise.
  await page.goto(`${origin}/dashboard/providers/xai`);

  async function assertLateStartOwnership(provider) {
    await page.goto(`${origin}/dashboard/providers/${provider}`);
    const starts = state.proxyStarts || 0, stops = state.proxyStops || 0, completed = state.proxyCompleted || 0;
    state.holdProxy = true; state.releaseProxy = null;
    let old = await openProxy(provider);
    await waitForFixture(() => typeof state.releaseProxy === "function");
    await closeProxy(old);
    state.holdProxy = false;
    let current = await openProxy(provider);
    await current.getByLabel("Authorization URL").waitFor();
    await waitForFixture(() => (state.proxyStarts || 0) === starts + 2);
    const oldResponse = page.waitForResponse(response => response.url().includes(`/api/oauth/${provider}/start-proxy`));
    state.releaseProxy();
    await oldResponse;
    await waitForFixture(() => (state.proxyCompleted || 0) === completed + 2);
    await page.waitForTimeout(150);
    assert.equal(state.proxyStops || 0, stops + 1, "late old startup cannot stop a newer active listener");
    assert.equal(state.proxyLive, true);
    await current.getByLabel("Authorization URL").waitFor();
    await closeProxy(current);
    await waitForFixture(() => (state.proxyStops || 0) === stops + 2 && state.proxyLive === false);
  }
  await assertLateStartOwnership("xai");
  await assertLateStartOwnership("codex");

  // If both dialogs close while the oldest startup has not taken effect yet,
  // cleanup runs immediately and again after that delayed start completes.
  await page.goto(`${origin}/dashboard/providers/xai`);
  let stops = state.proxyStops, completed = state.proxyCompleted;
  state.holdProxy = true; state.releaseProxy = null;
  let old = await openProxy("xai");
  await waitForFixture(() => typeof state.releaseProxy === "function");
  await closeProxy(old); state.holdProxy = false;
  let current = await openProxy("xai");
  await current.getByLabel("Authorization URL").waitFor();
  await closeProxy(current);
  await waitForFixture(() => state.proxyStops === stops + 2 && state.proxyLive === false);
  state.releaseProxy();
  await waitForFixture(() => state.proxyCompleted === completed + 2 && state.proxyStops === stops + 3 && state.proxyLive === false);

  // A stop already on the wire must finish before a new startup is issued.
  current = await openProxy("xai");
  await current.getByLabel("Authorization URL").waitFor();
  state.holdStop = true; state.releaseStop = null;
  stops = state.proxyStops;
  await closeProxy(current);
  await waitForFixture(() => typeof state.releaseStop === "function" && state.proxyStops === stops + 1);
  const startsBeforeBarrier = state.proxyStarts;
  current = await openProxy("xai");
  await page.waitForTimeout(150);
  assert.equal(state.proxyStarts, startsBeforeBarrier, "new startup must wait for the old stop response");
  state.holdStop = false; state.releaseStop();
  await waitForFixture(() => state.proxyStarts === startsBeforeBarrier + 1 && state.proxyLive === true);
  await page.waitForFunction(() => document.querySelector('input[aria-label="Authorization URL"]')?.value.includes("fixture-authorize"));
  assert.equal(state.proxyStarts, startsBeforeBarrier + 1);
  assert.equal(state.proxyLive, true);
  stops = state.proxyStops;
  await closeProxy(current);
  await waitForFixture(() => state.proxyStops === stops + 1 && state.proxyLive === false);

  const failStart = route => route.abort("failed");
  await page.route("**/api/oauth/xai/start-proxy?*", failStart);
  current = await openProxy("xai");
  await current.getByRole("alert").getByText("Network error").waitFor();
  assert.equal(await current.getByLabel("Callback URL").count(), 0, "xAI cannot accept bare codes without a registered proxy session");
  await page.unroute("**/api/oauth/xai/start-proxy?*", failStart);
  state.rejectProxyStart = true;
  await current.getByRole("button", { name: "Retry" }).click();
  await current.getByRole("alert").getByText("Synthetic proxy failure").waitFor();
  state.rejectProxyStart = false;
  await current.getByRole("button", { name: "Retry" }).click();
  await current.getByLabel("Callback URL").waitFor();
  stops = state.proxyStops;
  await closeProxy(current);
  await waitForFixture(() => state.proxyStops === stops + 1 && state.proxyLive === false);

  // An exchange that completes after dismissal must refresh the connection
  // list without closing a newer attempt, and an old list response is stale.
  await page.goto(`${origin}/dashboard/providers/claude`);
  await page.getByRole("button", { name: /Add Connection/ }).click();
  old = page.getByRole("dialog", { name: /Connect Claude/ });
  await old.getByLabel("Callback URL").fill("first-fixture-code");
  state.holdExchangeOnce = true; state.releaseExchange = null;
  await old.getByRole("button", { name: "Connect", exact: true }).click();
  await waitForFixture(() => typeof state.releaseExchange === "function");
  await closeProxy(old);
  await page.getByRole("button", { name: /Add Connection/ }).click();
  current = page.getByRole("dialog", { name: /Connect Claude/ });
  await current.getByLabel("Callback URL").waitFor();
  state.holdConnectionsOnce = true; state.releaseConnections = null;
  state.releaseExchange();
  await waitForFixture(() => typeof state.releaseConnections === "function");
  await current.getByLabel("Callback URL").fill("second-fixture-code");
  await current.getByRole("button", { name: "Connect", exact: true }).click();
  await current.waitFor({ state: "detached" });
  await page.getByText("Claude fixture 1", { exact: true }).waitFor();
  await page.getByText("Claude fixture 2", { exact: true }).waitFor();
  state.releaseConnections();
  await page.waitForTimeout(150);
  await page.getByText("Claude fixture 2", { exact: true }).waitFor();

  await page.goto(`${origin}/dashboard/providers/xai`);
  old = await openProxy("xai");
  await old.getByLabel("Callback URL").fill("first-fixture-xai");
  state.holdManualOnce = true; state.releaseManual = null;
  await old.getByRole("button", { name: "Connect", exact: true }).click();
  await waitForFixture(() => typeof state.releaseManual === "function");
  await closeProxy(old);
  current = await openProxy("xai");
  await current.getByLabel("Callback URL").waitFor();
  state.releaseManual();
  await page.getByText("xAI fixture 1", { exact: true }).waitFor();
  await current.getByLabel("Callback URL").waitFor();
  stops = state.proxyStops;
  await closeProxy(current);
  await waitForFixture(() => state.proxyStops === stops + 1 && state.proxyLive === false);

  state.proxyServerSide = true; state.holdStatusOnce = true; state.releaseStatus = null;
  old = await openProxy("xai");
  await waitForFixture(() => typeof state.releaseStatus === "function");
  await closeProxy(old);
  state.proxyServerSide = false;
  current = await openProxy("xai");
  await current.getByLabel("Callback URL").waitFor();
  state.releaseStatus();
  await page.getByText("Status account", { exact: true }).waitFor();
  await current.getByLabel("Callback URL").waitFor();
  stops = state.proxyStops;
  await closeProxy(current);
  await waitForFixture(() => state.proxyStops === stops + 1 && state.proxyLive === false);

  await page.goto(`${origin}/dashboard/providers/kilocode`);
  state.holdDevicePollOnce = true; state.releaseDevicePoll = null;
  await page.getByRole("button", { name: /lock OAuth$/ }).click();
  old = page.getByRole("dialog", { name: "Connect Kilo Code" });
  await waitForFixture(() => typeof state.releaseDevicePoll === "function");
  await closeProxy(old);
  await page.getByRole("button", { name: /lock OAuth$/ }).click();
  current = page.getByRole("dialog", { name: "Connect Kilo Code" });
  await current.getByText("FIXTURE-123").waitFor();
  state.releaseDevicePoll();
  await page.getByText("Delayed Device", { exact: true }).waitFor();
  await current.getByText("FIXTURE-123").waitFor();
  await closeProxy(current);
  for (const provider of ["azure", "cloudflare-ai", "openai-compatible-fixture"]) {
    const name = `Excluded ${provider}`;
    state.connections.push({ id: `excluded-${provider}`, provider, name, authType: "apikey", isActive: true });
    await page.goto(`${origin}/dashboard/providers/${provider}`);
    await row(name).getByRole("button", { name: /edit Edit/ }).click();
    edit = page.getByRole("dialog", { name: "Edit Connection" });
    assert.equal(await edit.getByRole("button", { name: "Test Connection", exact: true }).count(), 0);
    await edit.getByRole("button", { name: "Close" }).click();
  }
}

async function runCallbackFixture(page, origin) {
  state.callbackOnly = true;
  state.authOnly = true;
  state.connections = [];
  const context = page.context();
  const exchanges = () => state.writes.filter(w => w.path === "/api/oauth/claude/exchange");
  const callbackUrl = `${origin}/callback?code=fixture-relay&state=fixture-standalone#fragment`;
  await page.goto(`${origin}/fixture-observer`);
  await page.evaluate(() => {
    localStorage.removeItem("oauth_callback");
    window.received = [];
    window.callbackChannel = new BroadcastChannel("oauth_callback");
    callbackChannel.onmessage = event => received.push(["channel", event.data]);
    window.addEventListener("storage", event => { if (event.key === "oauth_callback" && event.newValue) received.push(["storage", JSON.parse(event.newValue)]); });
    window.addEventListener("message", event => { if (event.data?.type === "oauth_callback") received.push(["message", event.data.data, event.origin]); });
  });
  const popupEvent = page.waitForEvent("popup");
  const started = Date.now();
  await page.evaluate(url => window.open(url, "synthetic-callback"), callbackUrl);
  const popup = await popupEvent;
  const closed = popup.waitForEvent("close", { timeout: 7000 });
  await popup.getByRole("heading", { name: "Authentication Complete" }).waitFor();
  assert.equal(await popup.title(), "OpenProxy — OAuth Callback");
  assert.equal(await popup.locator("body").evaluate(el => getComputedStyle(el).backgroundColor), "rgb(13, 13, 13)");
  assert.equal(await popup.locator("body").evaluate(el => getComputedStyle(el).fontFeatureSettings), "normal");
  assert.equal(await popup.locator("h1").evaluate(el => getComputedStyle(el).letterSpacing), "normal");
  assert.equal(new URL(popup.url()).search, "?code=fixture-relay&state=fixture-standalone");
  assert.equal(new URL(popup.url()).hash, "#fragment");
  await page.waitForFunction(() => received.length === 3);
  const received = await page.evaluate(() => received);
  assert.deepEqual(received.map(([kind]) => kind).sort(), ["channel", "message", "storage"]);
  for (const [, payload] of received) {
    assert.equal(payload.code, "fixture-relay");
    assert.equal(payload.state, "fixture-standalone");
    assert.equal(typeof payload.timestamp, "number");
  }
  assert.equal(received.find(([kind]) => kind === "message")[2], origin);
  await page.waitForFunction(() => localStorage.getItem("oauth_callback") === null);
  await closed;
  assert.ok(Date.now() - started >= 1700, "opened success callback closes after approximately two seconds");

  const failed = await context.newPage();
  await failed.goto(`${origin}/callback?error=access_denied&error_description=%3Cimg%20src%3Dx%3E&code=ignored`);
  await failed.getByRole("heading", { name: "Authentication Failed" }).waitFor();
  assert.equal(await failed.locator("#message").textContent(), "<img src=x>");
  assert.equal(await failed.locator("#card img").count(), 0, "error descriptions are rendered as text");
  await failed.waitForTimeout(2100);
  assert.equal(failed.isClosed(), false);
  await failed.goto(`${origin}/callback`);
  await failed.getByRole("heading", { name: "No Auth Data Found" }).waitFor();
  await failed.locator("#message strong").getByText("Callback URL").waitFor();
  await failed.close();

  // With every relay transport unavailable the original URL remains copyable,
  // including when the async clipboard API rejects and execCommand is needed.
  const fallbackContext = await context.browser().newContext();
  await fallbackContext.route("**/*", route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
  await fallbackContext.addInitScript(() => {
    Object.defineProperty(window, "BroadcastChannel", { value: undefined });
    Object.defineProperty(window, "localStorage", { get() { throw new DOMException("blocked", "SecurityError"); } });
    if (navigator.clipboard) Object.defineProperty(navigator.clipboard, "writeText", { value: () => Promise.reject(new Error("blocked")) });
    document.execCommand = command => { if (command === "copy") window.fallbackCopied = document.activeElement.value; return true; };
  });
  const fallback = await fallbackContext.newPage();
  await fallback.goto(callbackUrl);
  await fallback.getByRole("heading", { name: "Authentication Complete" }).waitFor();
  assert.equal(await fallback.locator(".fallback-url").textContent(), `Full URL: ${callbackUrl}`);
  assert.equal(await fallback.getByRole("button", { name: "Copy URL" }).evaluate(el => getComputedStyle(el).lineHeight), "normal");
  await fallback.getByRole("button", { name: "Copy URL" }).click();
  await fallback.getByRole("button", { name: "Copied!" }).waitFor();
  await fallback.waitForFunction(url => window.fallbackCopied === url, callbackUrl);
  await fallback.getByRole("button", { name: "Copy URL" }).waitFor({ timeout: 5000 });
  await fallback.evaluate(() => {
    window.fallbackCopied = null;
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async value => { window.__fixtureCopied = value; } } });
  });
  assert.equal(await fallback.evaluate(() => navigator.clipboard instanceof Clipboard), false);
  await fallback.getByRole("button", { name: "Copy URL" }).click();
  await fallback.waitForFunction(url => window.fallbackCopied === url, callbackUrl);
  assert.equal(await fallback.evaluate(() => window.__fixtureCopied), undefined, "a plain clipboard shim takes the execCommand fallback");

  // Real Chromium Clipboard objects retain their native brand, unlike a plain
  // object substituted for navigator.clipboard in a test harness.
  const nativeContext = await context.browser().newContext();
  await nativeContext.grantPermissions(["clipboard-read", "clipboard-write"], { origin });
  await nativeContext.route("**/*", route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
  await nativeContext.addInitScript(() => {
    Object.defineProperty(window, "BroadcastChannel", { value: undefined });
    Object.defineProperty(window, "localStorage", { get() { throw new DOMException("blocked", "SecurityError"); } });
  });
  const native = await nativeContext.newPage();
  await native.goto(callbackUrl);
  await native.getByRole("button", { name: "Copy URL" }).waitFor();
  assert.equal(await native.evaluate(() => navigator.clipboard instanceof Clipboard), true);
  await native.getByRole("button", { name: "Copy URL" }).click();
  assert.equal(await native.evaluate(() => navigator.clipboard.readText()), callbackUrl);
  await native.getByRole("button", { name: "Copy URL" }).waitFor({ timeout: 5000 });
  await native.evaluate(() => navigator.clipboard.writeText("fixture-previous-value"));
  await native.evaluate(() => Object.defineProperty(navigator.clipboard, "writeText", { configurable: true, value: async value => { window.fixtureCopied = value; } }));
  await native.getByRole("button", { name: "Copy URL" }).click();
  await native.waitForFunction(url => window.fixtureCopied === url, callbackUrl);
  await native.getByRole("button", { name: "Copy URL" }).waitFor({ timeout: 5000 });
  await native.evaluate(() => {
    window.nativeClipboard = navigator.clipboard;
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async value => { window.__fixtureCopied = value; } } });
  });
  await native.getByRole("button", { name: "Copy URL" }).click();
  assert.equal(await native.evaluate(() => window.__fixtureCopied), undefined);
  assert.equal(await native.evaluate(() => window.nativeClipboard.readText()), callbackUrl, "the actual execCommand fallback copies for a plain-object shim");
  await nativeContext.close();
  assert.equal(state.authStatusRequests || 0, 0, "the standalone callback does not load or require a dashboard session");

  const attacker = http.createServer((_, res) => { res.writeHead(200, { "content-type": "text/html" }); res.end("<!doctype html><title>Fixture origin</title>"); });
  await new Promise(resolve => attacker.listen(0, "127.0.0.1", resolve));
  const attackerOrigin = `http://127.0.0.1:${attacker.address().port}`;
  await context.route(`${attackerOrigin}/**`, route => route.continue());
  await fallbackContext.route(`${attackerOrigin}/**`, route => route.continue());
  try {
    const foreignOpener = await fallbackContext.newPage();
    await foreignOpener.goto(`${attackerOrigin}/`);
    const fallbackPopupEvent = foreignOpener.waitForEvent("popup");
    await foreignOpener.evaluate(url => window.open(url, "foreign-opener-fallback"), callbackUrl);
    const fallbackPopup = await fallbackPopupEvent;
    await fallbackPopup.getByRole("heading", { name: "Authentication Complete" }).waitFor();
    assert.equal(await fallbackPopup.locator(".fallback-url").textContent(), `Full URL: ${callbackUrl}`, "a mismatched opener is not counted as delivery");
    await fallbackPopup.getByRole("button", { name: "Copy URL" }).waitFor();
    await fallbackPopup.close();
    await foreignOpener.close();
    await page.goto(`${origin}/dashboard/providers/claude`);
    const open = async () => {
      await page.getByRole("button", { name: /^add Add(?: Connection)?$/ }).click();
      const modal = page.getByRole("dialog", { name: /Connect Claude/ });
      await modal.getByLabel("Callback URL").waitFor();
      return modal;
    };
    let modal = await open();
    let expected = `fixture-state-${state.callbackAuthCounter}`;
    const before = exchanges().length;
    const send = payload => page.evaluate(data => window.postMessage({ type: "oauth_callback", data }, window.location.origin), payload);
    await send({ code: "wrong-state", state: "wrong" });
    await send({ code: "wrong-provider", state: expected, provider: "codex" });
    await send({ code: "missing-state" });
    await page.evaluate(state => window.dispatchEvent(new MessageEvent("message", { origin: "https://localhost.attacker.invalid", data: { type: "oauth_callback", data: { code: "spoofed-localhost", state } } })), expected);
    const badEvent = page.waitForEvent("popup", { predicate: async popup => { await popup.waitForLoadState(); return popup.url().startsWith(attackerOrigin); } });
    await page.evaluate(url => window.open(url, "fixture-attacker"), `${attackerOrigin}/`);
    const bad = await badEvent;
    await bad.waitForLoadState();
    await bad.evaluate(({ target, state }) => window.opener.postMessage({ type: "oauth_callback", data: { code: "untrusted-origin", state } }, target), { target: origin, state: expected });
    await page.waitForTimeout(250);
    assert.equal(exchanges().length, before, "invalid state, provider and origin cannot start an exchange");
    await bad.evaluate(() => { window.receivedSecrets = []; window.addEventListener("message", event => window.receivedSecrets.push(event.data)); });
    const unsafeOpenerPopup = bad.waitForEvent("popup");
    await bad.evaluate(url => window.open(url, "fixture-secret-test"), `${origin}/callback?code=synthetic-private-code&state=unrelated`);
    const unsafeCallback = await unsafeOpenerPopup;
    await unsafeCallback.getByRole("heading", { name: "Authentication Complete" }).waitFor();
    await bad.waitForTimeout(250);
    assert.deepEqual(await bad.evaluate(() => receivedSecrets), [], "callback never sends credentials to an unrelated opener origin");
    await unsafeCallback.close();
    await bad.close();

    state.holdExchangeOnce = true; state.releaseExchange = null;
    const relayPage = await context.newPage();
    await relayPage.goto(`${origin}/callback?code=first-fixture-code&state=${expected}`);
    await waitForFixture(() => typeof state.releaseExchange === "function");
    await send({ code: "duplicate", state: expected });
    await page.waitForTimeout(100);
    assert.equal(exchanges().length, before + 1, "channel, storage and message duplicates are coalesced");
    assert.equal(exchanges().at(-1).body.code, "first-fixture-code");
    await modal.getByRole("button", { name: "Close" }).click();
    await modal.waitFor({ state: "detached" });
    modal = await open();
    const newerState = `fixture-state-${state.callbackAuthCounter}`;
    assert.notEqual(newerState, expected);
    state.releaseExchange();
    await page.getByText("Claude fixture 1", { exact: true }).waitFor();
    await modal.getByLabel("Callback URL").waitFor();
    await send({ code: "old-attempt", state: expected });
    await page.waitForTimeout(150);
    assert.equal(exchanges().length, before + 1, "old callbacks cannot close or exchange in a newer attempt");
    await send({ code: "second-fixture-code", state: newerState, provider: "claude" });
    await modal.waitFor({ state: "detached" });
    await page.getByText("Claude fixture 2", { exact: true }).waitFor();
    assert.equal(exchanges().length, before + 2);
    await send({ code: "after-close", state: newerState });
    await relayPage.evaluate(state => {
      const channel = new BroadcastChannel("oauth_callback");
      channel.postMessage({ code: "after-close-channel", state });
      channel.close();
      localStorage.setItem("oauth_callback", JSON.stringify({ code: "after-close-storage", state, timestamp: Date.now() }));
    }, newerState);
    await page.waitForTimeout(150);
    assert.equal(exchanges().length, before + 2, "closed modal listeners are removed");
    assert.equal(await page.evaluate(() => JSON.parse(localStorage.getItem("oauth_callback")).code), "after-close-storage");
    await page.evaluate(() => localStorage.removeItem("oauth_callback"));
    await relayPage.close();

    // Existing stale data is discarded, while a fresh matching storage value
    // is consumed after authorization data becomes available.
    const staleState = `fixture-state-${state.callbackAuthCounter + 1}`;
    await page.evaluate(state => localStorage.setItem("oauth_callback", JSON.stringify({ code: "stale", state, timestamp: Date.now() - 31_000 })), staleState);
    modal = await open();
    await page.waitForFunction(() => localStorage.getItem("oauth_callback") === null);
    assert.equal(exchanges().length, before + 2);
    await modal.getByRole("button", { name: "Close" }).click();
    const freshState = `fixture-state-${state.callbackAuthCounter + 1}`;
    await page.evaluate(state => localStorage.setItem("oauth_callback", JSON.stringify({ code: "fresh-storage", state, timestamp: Date.now() })), freshState);
    await page.getByRole("button", { name: /^add Add(?: Connection)?$/ }).click();
    modal = page.getByRole("dialog", { name: /Connect Claude/ });
    await waitForFixture(() => exchanges().length === before + 3);
    await modal.waitFor({ state: "detached" });
    await page.getByText("Claude fixture 3", { exact: true }).waitFor();
    assert.equal(exchanges().at(-1).body.code, "fresh-storage");
    assert.equal(await page.evaluate(() => localStorage.getItem("oauth_callback")), null);

    modal = await open();
    expected = `fixture-state-${state.callbackAuthCounter}`;
    const errorPage = await context.newPage();
    await errorPage.goto(`${origin}/callback?error=access_denied&error_description=Synthetic%20authorization%20denied&state=${expected}`);
    await modal.getByRole("alert").getByText("Synthetic authorization denied").waitFor();
    assert.equal(exchanges().length, before + 3);
    await modal.getByRole("button", { name: "Close" }).click();
    await errorPage.close();

    // Exercise the actual storage-event path without a BroadcastChannel.
    modal = await open();
    expected = `fixture-state-${state.callbackAuthCounter}`;
    const writer = await context.newPage();
    await writer.goto(`${origin}/fixture-observer`);
    await writer.evaluate(state => localStorage.setItem("oauth_callback", JSON.stringify({ code: "storage-event", state, timestamp: Date.now() })), expected);
    await modal.waitFor({ state: "detached" });
    await page.getByText("Claude fixture 4", { exact: true }).waitFor();
    assert.equal(exchanges().at(-1).body.code, "storage-event");
    assert.equal(await page.evaluate(() => localStorage.getItem("oauth_callback")), null);
    await writer.close();

    // Exercise a channel-only message with a token, as supported by the
    // existing consumer, and verify it becomes the exchange code.
    modal = await open();
    expected = `fixture-state-${state.callbackAuthCounter}`;
    const channelSender = await context.newPage();
    await channelSender.goto(`${origin}/fixture-observer`);
    await channelSender.evaluate(state => { const channel = new BroadcastChannel("oauth_callback"); channel.postMessage({ token: "channel-token", state }); channel.close(); }, expected);
    await modal.waitFor({ state: "detached" });
    await page.getByText("Claude fixture 5", { exact: true }).waitFor();
    assert.equal(exchanges().at(-1).body.code, "channel-token");
    await channelSender.close();

    // A real popup from the exact active redirect origin is also accepted;
    // an arbitrary other origin was rejected above.
    modal = await open();
    expected = `fixture-state-${state.callbackAuthCounter}`;
    const redirectOrigin = `http://localhost:${server.address().port}`;
    await context.route(`${redirectOrigin}/**`, route => route.continue());
    const redirectPopupEvent = page.waitForEvent("popup", { predicate: async popup => { await popup.waitForLoadState(); return popup.url().startsWith(redirectOrigin); } });
    await page.evaluate(url => window.open(url, "fixture-redirect"), `${redirectOrigin}/fixture-observer`);
    const redirectPopup = await redirectPopupEvent;
    await redirectPopup.evaluate(({ target, state }) => window.opener.postMessage({ type: "oauth_callback", data: { code: "redirect-origin", state } }, target), { target: origin, state: expected });
    await modal.waitFor({ state: "detached" });
    await page.getByText("Claude fixture 6", { exact: true }).waitFor();
    assert.equal(exchanges().at(-1).body.code, "redirect-origin");
    await redirectPopup.close();
    await context.unroute(`${redirectOrigin}/**`);

    // A server-side status response may settle while a validated callback
    // exchange is pending; it must not close that active dialog first.
    state.proxyServerSide = true;
    state.holdStatusOnce = true;
    state.holdXaiCallback = true;
    await page.goto(`${origin}/dashboard/providers/xai`);
    await page.getByRole("button", { name: /Grok Build OAuth/ }).click();
    const xaiModal = page.getByRole("dialog", { name: /Connect Grok/ });
    await xaiModal.getByLabel("Callback URL").waitFor();
    await waitForFixture(() => typeof state.releaseStatus === "function");
    const xaiState = `fixture-state-${state.callbackAuthCounter}`;
    await send({ code: "xai-callback", state: xaiState, provider: "xai" });
    await waitForFixture(() => typeof state.releaseXaiCallback === "function");
    state.releaseStatus();
    await page.getByText("Status account", { exact: true }).waitFor();
    await xaiModal.getByLabel("Callback URL").waitFor();
    await xaiModal.getByRole("button", { name: "Connecting..." }).waitFor();
    state.releaseXaiCallback();
    await xaiModal.waitFor({ state: "detached" });
    await page.getByText("Callback xAI", { exact: true }).waitFor();
  } finally {
    await fallbackContext.close();
    await context.unroute(`${attackerOrigin}/**`);
    attacker.closeAllConnections();
    await new Promise(resolve => attacker.close(resolve));
  }
}

async function runPopupProbe(page, origin) {
  const provider = http.createServer((_, res) => { res.writeHead(200, { "content-type": "text/html" }); res.end("<!doctype html><title>Synthetic authorization</title>"); });
  await new Promise(resolve => provider.listen(0, "127.0.0.1", resolve));
  const providerOrigin = `http://127.0.0.1:${provider.address().port}`;
  const context = page.context();
  await context.route(`${providerOrigin}/**`, route => route.continue());
  try {
    await page.goto(`${origin}/fixture-observer`);
    const popupEvent = page.waitForEvent("popup");
    await page.evaluate(url => {
      const popup = window.open("about:blank", "fixture-probe");
      if (!popup) throw new Error("Popup blocked");
      window.probe = popup;
      popup.opener = null;
      popup.location.href = url;
    }, `${providerOrigin}/authorize`);
    const popup = await popupEvent;
    await popup.waitForURL(`${providerOrigin}/authorize`);
    assert.equal(await popup.evaluate(() => window.opener), null, "external authorization page receives no opener");
    assert.equal(await page.evaluate(() => { try { void window.probe.location.href; return "readable"; } catch (e) { return e.name; } }), "SecurityError");
    const callback = `${origin}/callback?code=probe-code&state=probe-state`;
    await popup.evaluate(url => window.location.assign(url), callback);
    await page.waitForFunction(() => { try { return window.probe.location.pathname === "/callback" && window.probe.location.search.includes("probe-code"); } catch { return false; } });
    assert.equal(await popup.evaluate(() => window.opener), null, "returned page still has no opener");
    const closed = popup.waitForEvent("close");
    await page.evaluate(() => window.probe.close());
    await closed;
  } finally {
    await context.unroute(`${providerOrigin}/**`);
    provider.closeAllConnections();
    await new Promise(resolve => provider.close(resolve));
  }
}

async function runPopupFixture(page, origin) {
  const provider = http.createServer((_, res) => { res.writeHead(200, { "content-type": "text/html" }); res.end("<!doctype html><title>Synthetic authorization</title>"); });
  await new Promise(resolve => provider.listen(0, "127.0.0.1", resolve));
  const providerOrigin = `http://127.0.0.1:${provider.address().port}`;
  const context = page.context();
  state.popupOnly = true;
  state.callbackOnly = true;
  state.authOnly = true;
  state.popupAuthorizations = [];
  state.popupProviderOrigin = providerOrigin;
  state.connections = [];
  await context.addInitScript(() => {
    Object.defineProperty(window, "BroadcastChannel", { configurable: true, value: undefined });
    Object.defineProperty(window, "localStorage", { configurable: true, get() { throw new DOMException("Fixture storage unavailable", "SecurityError"); } });
  });
  await context.route(`${providerOrigin}/**`, route => route.continue());
  const exchanges = providerId => state.writes.filter(w => w.path === `/api/oauth/${providerId}/exchange`);
  const open = async providerName => {
    const event = page.waitForEvent("popup");
    await page.getByRole("button", { name: /^add Add(?: Connection)?$/ }).click();
    const popup = await event;
    await popup.waitForURL(`${providerOrigin}/authorize`);
    assert.equal(await popup.evaluate(() => window.opener), null, "external provider cannot access dashboard opener");
    const modal = page.getByRole("dialog", { name: `Connect ${providerName}` });
    await modal.getByLabel("Callback URL").waitFor();
    return { popup, modal, auth: state.popupAuthorizations.at(-1) };
  };
  const pause = () => page.waitForTimeout(500);
  const navigate = async (popup, url) => { await popup.evaluate(next => window.location.assign(next), url); await popup.waitForURL(url); };
  try {
    await page.goto(`${origin}/dashboard/providers/cline`);
    let { popup, modal, auth } = await open("Cline");
    const redirect = new URL(auth.redirectUri);
    assert.equal(redirect.origin, origin);
    assert.equal(redirect.pathname, "/callback");
    const nonce = redirect.searchParams.get("openproxy_oauth_nonce");
    assert.match(nonce, /^[0-9a-f]{64}$/, "Cline redirect binds a cryptographic nonce");
    const initial = exchanges("cline").length;
    for (const query of [
      "code=missing-nonce",
      "code=wrong-nonce&openproxy_oauth_nonce=wrong",
      `code=conflicting-state&openproxy_oauth_nonce=${nonce}&state=wrong`,
      `code=wrong-provider&openproxy_oauth_nonce=${nonce}&provider=claude`,
    ]) {
      await navigate(popup, `${origin}/callback?${query}`);
      await pause();
      assert.equal(exchanges("cline").length, initial, "invalid Cline callback is rejected");
      assert.equal(popup.isClosed(), false, "invalid callback stays available for manual copying");
    }
    await navigate(popup, `${origin}/fixture-observer?code=wrong-path&openproxy_oauth_nonce=${nonce}`);
    await pause();
    assert.equal(exchanges("cline").length, initial, "same-origin non-callback paths are ignored");
    state.holdClineExchange = true;
    await navigate(popup, `${origin}/callback?code=cline-code&openproxy_oauth_nonce=${nonce}`);
    await waitForFixture(() => typeof state.releaseClineExchange === "function");
    assert.deepEqual(exchanges("cline").at(-1).body, { code: "cline-code", redirectUri: auth.redirectUri, codeVerifier: "fixture-verifier", state: auth.state });
    await popup.waitForTimeout(2100);
    assert.equal(popup.isClosed(), false, "a callback is not closed before its exchange succeeds");
    const oldPopup = popup;
    await modal.getByRole("button", { name: "Close" }).click();
    await modal.waitFor({ state: "detached" });
    ({ popup, modal, auth } = await open("Cline"));
    const nextNonce = new URL(auth.redirectUri).searchParams.get("openproxy_oauth_nonce");
    assert.match(nextNonce, /^[0-9a-f]{64}$/);
    assert.notEqual(nextNonce, nonce, "each Cline attempt uses a fresh binding");
    const oldClosed = oldPopup.waitForEvent("close", { timeout: 6000 });
    state.releaseClineExchange();
    await page.getByText("Cline fixture 1", { exact: true }).waitFor();
    await modal.getByLabel("Callback URL").waitFor();
    await oldClosed;
    assert.equal(popup.isClosed(), false, "old completion never closes the newer popup");
    await navigate(popup, `${origin}/callback?code=old-attempt&openproxy_oauth_nonce=${nonce}`);
    await pause();
    assert.equal(exchanges("cline").length, initial + 1, "stale nonce cannot authenticate a newer attempt");
    await modal.getByRole("button", { name: "Close" }).click();
    await modal.waitFor({ state: "detached" });
    await pause();
    assert.equal(popup.isClosed(), false, "dismissal preserves an unconsumed popup for manual fallback");
    await popup.close();

    await page.goto(`${origin}/dashboard/providers/claude`);
    await page.evaluate(() => {
      const original = window.open;
      let block = true;
      window.open = function (...args) { if (block) { block = false; return null; } return original.apply(this, args); };
    });
    await page.getByRole("button", { name: /^add Add(?: Connection)?$/ }).click();
    modal = page.getByRole("dialog", { name: "Connect Claude" });
    await modal.getByLabel("Callback URL").waitFor();
    const openButton = modal.getByRole("button", { name: "Open Authorization" });
    await openButton.waitFor();
    const blockedEvent = page.waitForEvent("popup");
    await openButton.click();
    popup = await blockedEvent;
    await popup.waitForURL(`${providerOrigin}/authorize`);
    assert.equal(await popup.evaluate(() => window.opener), null);
    auth = state.popupAuthorizations.at(-1);
    const before = exchanges("claude").length;
    await navigate(popup, `${origin}/callback?code=wrong-state&state=wrong`);
    await pause();
    assert.equal(exchanges("claude").length, before, "non-Cline providers still require exact state");
    assert.equal(popup.isClosed(), false);
    state.rejectClaudeExchange = true;
    await navigate(popup, `${origin}/callback?code=failing-code&state=${encodeURIComponent(auth.state)}`);
    await modal.getByText("Synthetic exchange failure").waitFor();
    assert.equal(popup.isClosed(), false, "failed exchange leaves callback URL available");
    assert.match(popup.url(), /failing-code/);
    const retryPopupEvent = page.waitForEvent("popup");
    await modal.getByRole("button", { name: "Retry" }).click();
    const oldFailedPopup = popup;
    popup = await retryPopupEvent;
    await popup.waitForURL(`${providerOrigin}/authorize`);
    assert.equal(await popup.evaluate(() => window.opener), null);
    auth = state.popupAuthorizations.at(-1);
    const started = Date.now();
    const closed = popup.waitForEvent("close", { timeout: 7000 });
    await navigate(popup, `${origin}/callback?code=successful-code&state=${encodeURIComponent(auth.state)}`);
    await modal.waitFor({ state: "detached" });
    await popup.waitForTimeout(700);
    assert.equal(popup.isClosed(), false, "successful popup waits before auto-close");
    await closed;
    assert.ok(Date.now() - started >= 1700, "successful callback closes around two seconds after consumption");
    assert.equal(oldFailedPopup.isClosed(), false, "a later attempt never closes an earlier failed popup");
    await oldFailedPopup.close();
  } finally {
    if (state.releaseClineExchange) state.releaseClineExchange();
    await context.unroute(`${providerOrigin}/**`);
    provider.closeAllConnections();
    await new Promise(resolve => provider.close(resolve));
  }
}

async function runUuidCompatibleNodeFixture(page, origin) {
  // The real POST /api/provider-nodes returns UUIDs, not type-prefixed IDs.
  const openId = "1fc62630-3530-41a5-a346-8a75ea520dc1";
  const anthropicId = "76a9513d-1ce3-4e98-a7de-3af2e964c8bb";
  const missingId = "adf83924-b3bb-4d44-b4c6-35ac547f7a38";
  const mismatchId = "openai-compatible-persisted-anthropic";
  state.nodes.push(
    { id: openId, name: "UUID OpenAI", type: "openai-compatible", prefix: "uuid-open", apiType: "responses", baseUrl: "https://uuid-open.example.invalid/v1" },
    { id: anthropicId, name: "UUID Anthropic", type: "anthropic-compatible", prefix: "uuid-anthropic", apiType: null, baseUrl: "https://uuid-anthropic.example.invalid/v1" },
    { id: mismatchId, name: "Type Beats Prefix", type: "anthropic-compatible", prefix: "mixed", baseUrl: "https://mixed.example.invalid/v1" },
    { id: "unsupported-node", name: "Unsupported Node", type: "other", prefix: "unsupported" },
    { id: "codex", name: "Must Not Shadow Builtin", type: "anthropic-compatible", prefix: "shadow" },
  );
  const details = title => page.getByRole("heading", { name: title }).locator("xpath=../..");
  await page.goto(`${origin}/dashboard/providers`);
  await page.getByRole("link", { name: /UUID OpenAI/ }).waitFor();
  state.holdNodeRead = true;
  await page.getByRole("link", { name: /UUID OpenAI/ }).click();
  await waitForFixture(() => typeof state.releaseNodeRead === "function");
  assert.ok(await page.locator("main .animate-pulse").count() > 0, "UUID details stay in the loading state until node metadata arrives");
  assert.equal(await page.getByText("Provider not found", { exact: true }).count(), 0);
  state.holdNodeRead = false;
  state.releaseNodeRead();
  await page.locator("main h1").getByText("UUID OpenAI").waitFor();
  assert.equal(new URL(page.url()).pathname, `/dashboard/providers/${openId}`);
  await page.getByText("Responses API · https://uuid-open.example.invalid/v1/responses", { exact: true }).waitFor();
  assert.equal(await page.locator('main img[alt="UUID OpenAI"]').getAttribute("src"), "/providers/oai-r.png");

  const openCard = details("OpenAI Compatible Details");
  await openCard.getByRole("button", { name: /Add API Key/ }).click();
  const add = page.getByRole("dialog", { name: "Add UUID OpenAI API Key" });
  await add.getByLabel("Name", { exact: true }).fill("UUID connection");
  await add.getByLabel("API Key", { exact: true }).fill("synthetic-uuid-key");
  assert.equal(await add.getByRole("button", { name: "Save", exact: true }).isDisabled(), true, "compatible default model is required even with a UUID");
  await add.getByLabel("Default Model").fill("  uuid-model  ");
  await add.getByRole("button", { name: "Save", exact: true }).click();
  await add.waitFor({ state: "detached" });
  const created = state.writes.filter(write => write.path === "/api/providers" && write.method === "POST").at(-1).body;
  assert.equal(created.provider, openId);
  assert.equal(created.defaultModel, "uuid-model");
  await page.getByText("UUID connection", { exact: true }).waitFor();
  assert.equal(await openCard.getByRole("button", { name: /Add API Key/ }).isDisabled(), true);
  await page.getByRole("checkbox", { name: "Select connection UUID connection" }).locator("xpath=../..").getByRole("button", { name: /edit Edit/ }).click();
  const connectionEdit = page.getByRole("dialog", { name: "Edit Connection" });
  assert.equal(await connectionEdit.getByRole("button", { name: "Test Connection", exact: true }).count(), 0, "UUID compatible connections keep the compatible edit behavior");
  await connectionEdit.getByRole("button", { name: "Close" }).click();

  await page.getByLabel("Model ID", { exact: true }).fill("uuid-custom");
  await page.getByRole("button", { name: /^add Add$/ }).click();
  await page.locator("code").filter({ hasText: "uuid-open/uuid-custom" }).waitFor();
  assert.deepEqual(state.writes.filter(write => write.path === "/api/models/custom" && write.method === "POST").at(-1).body, { providerAlias: openId, id: "uuid-custom", type: "llm" });
  await openCard.getByRole("button", { name: /^edit Edit$/ }).click();
  let edit = page.getByRole("dialog", { name: "Edit OpenAI Compatible" });
  await edit.getByLabel("Name", { exact: true }).fill("UUID OpenAI Updated");
  await edit.getByLabel("Prefix", { exact: true }).fill("uuid-updated");
  await edit.getByLabel("API Type", { exact: true }).selectOption("chat");
  await edit.getByRole("button", { name: "Save", exact: true }).click();
  await edit.waitFor({ state: "detached" });
  assert.deepEqual(state.writes.filter(write => write.path === `/api/provider-nodes/${openId}` && write.method === "PUT").at(-1).body, { name: "UUID OpenAI Updated", prefix: "uuid-updated", baseUrl: "https://uuid-open.example.invalid/v1", apiType: "chat" });
  await page.locator("code").filter({ hasText: "uuid-updated/uuid-custom" }).waitFor();
  assert.equal(await page.locator('main img[alt="UUID OpenAI Updated"]').getAttribute("src"), "/providers/oai-cc.png");
  await page.reload();
  await page.locator("code").filter({ hasText: "uuid-updated/uuid-custom" }).waitFor();
  await details("OpenAI Compatible Details").getByRole("button", { name: /^delete Delete$/ }).click();
  await page.getByRole("dialog", { name: "Delete compatible node" }).getByRole("button", { name: "Delete", exact: true }).click();
  await page.waitForURL(`${origin}/dashboard/providers`);
  assert.ok(!state.nodes.some(node => node.id === openId));
  assert.ok(state.connections.some(conn => conn.provider === openId), "node delete must not cascade to connections");
  assert.ok(state.custom.some(model => model.providerAlias === openId && model.id === "uuid-custom"), "node delete must not cascade to models");

  await page.getByRole("link", { name: /UUID Anthropic/ }).click();
  await page.locator("main h1").getByText("UUID Anthropic").waitFor();
  await page.getByText("Messages API · https://uuid-anthropic.example.invalid/v1/messages", { exact: true }).waitFor();
  assert.equal(await page.locator('main img[alt="UUID Anthropic"]').getAttribute("src"), "/providers/anthropic-m.png");
  await page.getByLabel("Model ID", { exact: true }).fill("anthropic-uuid-custom");
  await page.getByRole("button", { name: /^add Add$/ }).click();
  await page.locator("code").filter({ hasText: "uuid-anthropic/anthropic-uuid-custom" }).waitFor();
  assert.deepEqual(state.writes.filter(write => write.path === "/api/models/custom" && write.method === "POST").at(-1).body, { providerAlias: anthropicId, id: "anthropic-uuid-custom", type: "llm" });
  await details("Anthropic Compatible Details").getByRole("button", { name: /^edit Edit$/ }).click();
  edit = page.getByRole("dialog", { name: "Edit Anthropic Compatible" });
  assert.equal(await edit.getByLabel("API Type", { exact: true }).count(), 0);
  await edit.getByLabel("Name", { exact: true }).fill("UUID Anthropic Updated");
  await edit.getByRole("button", { name: "Save", exact: true }).click();
  await edit.waitFor({ state: "detached" });
  assert.deepEqual(state.writes.filter(write => write.path === `/api/provider-nodes/${anthropicId}` && write.method === "PUT").at(-1).body, { name: "UUID Anthropic Updated", prefix: "uuid-anthropic", baseUrl: "https://uuid-anthropic.example.invalid/v1" });
  await details("Anthropic Compatible Details").getByRole("button", { name: /^delete Delete$/ }).click();
  await page.getByRole("dialog", { name: "Delete compatible node" }).getByRole("button", { name: "Delete", exact: true }).click();
  await page.waitForURL(`${origin}/dashboard/providers`);
  assert.ok(!state.nodes.some(node => node.id === anthropicId));
  assert.ok(state.custom.some(model => model.providerAlias === anthropicId && model.id === "anthropic-uuid-custom"));

  await page.goto(`${origin}/dashboard/providers/${mismatchId}`);
  await page.getByRole("heading", { name: "Anthropic Compatible Details" }).waitFor();
  assert.equal(await page.locator('main img[alt="Type Beats Prefix"]').getAttribute("src"), "/providers/anthropic-m.png", "persisted type wins over the ID prefix");
  state.nodeReadMode = "fail";
  const readsBeforeBuiltin = state.nodeReads;
  await page.goto(`${origin}/dashboard/providers/codex`);
  await page.locator("main h1").filter({ hasText: "OpenAI Codex" }).last().waitFor();
  assert.equal(state.nodeReads, readsBeforeBuiltin, "built-in details do not depend on a node lookup");
  assert.equal(await page.getByText("Must Not Shadow Builtin").count(), 0);
  await page.goto(`${origin}/dashboard/providers/${missingId}`);
  await page.getByRole("alert").getByText("Compatible provider could not be loaded.").waitFor();
  assert.equal(await page.getByText("Provider not found").count(), 0, "failed lookups are not mistaken for missing nodes");
  state.nodeReadMode = undefined;
  await page.reload();
  await page.getByText("Provider not found", { exact: true }).waitFor();
  await page.goto(`${origin}/dashboard/providers/unsupported-node`);
  await page.getByText("Provider not found", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: /^edit Edit$/ }).count(), 0);
  state.nodeReadMode = "malformed";
  await page.goto(`${origin}/dashboard/providers/${missingId}`);
  await page.getByRole("alert").getByText("Compatible provider could not be loaded.").waitFor();
  assert.equal(await page.getByText("Provider not found").count(), 0);
  state.nodeReadMode = "unauthorized";
  await page.goto(`${origin}/dashboard/providers/${anthropicId}`);
  await page.waitForURL(`${origin}/login`);
  state.nodeReadMode = undefined;
  state.expired = false;
}

async function runCompatibleImportFixture(page, origin) {
  const id = "4e6808c9-b26f-42fb-b6fc-bcd09c79c55f";
  state.importOnly = true;
  state.writes = [];
  state.importGets = [];
  state.nodes = [{ id, name: "Import Fixture", type: "openai-compatible", prefix: "display-prefix", apiType: "chat", baseUrl: "https://example.invalid/v1" }];
  state.connections = [];
  state.custom = [];
  state.aliases = {};
  await page.goto(`${origin}/dashboard/providers/${id}`);
  await page.getByText("Add OpenAI-compatible models manually or import them from the /models endpoint.", { exact: true }).waitFor();
  await page.getByText("Add a connection to enable importing models.", { exact: true }).waitFor();
  await page.getByText("Add a model ID above or import from the provider's /models endpoint.", { exact: true }).waitFor();
  let importButton = page.getByRole("button", { name: /Import from \/models/ });
  assert.equal(await importButton.isDisabled(), true);
  await importButton.evaluate(button => button.click());
  assert.deepEqual(state.importGets, []);

  state.connections = [{ id: "inactive", provider: id, name: "Inactive", isActive: false }];
  await page.reload();
  await page.getByText("Add a connection to enable importing models.", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: /Import from \/models/ }).isDisabled(), true);

  state.connections.push({ id: "first-default", provider: id, name: "Default active" }, { id: "second-active", provider: id, name: "Second active", isActive: true });
  state.custom.push({ providerAlias: id, id: "existing", name: "Preserve metadata", type: "llm", opencode: { limit: { context: 123456 } } });
  state.aliases["legacy-alias"] = `${id}/legacy`;
  await page.reload();
  await page.locator("code").filter({ hasText: "display-prefix/existing" }).waitFor();
  assert.equal(await page.getByText("Add a connection to enable importing models.").count(), 0);
  state.importData = { models: [{ id: "existing" }, { id: "legacy" }, { id: "fresh" }, { id: "fresh" }, { name: "name-fallback" }, { model: "model-fallback" }, { id: "manual-added" }, { id: "" }, { id: "  \uFEFF " }, {}, { id: 123 }] };
  state.holdImport = true;
  importButton = page.getByRole("button", { name: /Import from \/models/ });
  await importButton.click();
  await waitForFixture(() => typeof state.releaseImport === "function");
  const importing = page.getByRole("button", { name: /Importing/ });
  assert.equal(await importing.isDisabled(), true);
  await importing.evaluate(button => button.click());
  assert.deepEqual(state.importGets, ["/api/providers/first-default/models"], "only the first active connection is fetched and concurrent import is blocked");
  await page.getByLabel("Model ID", { exact: true }).fill("manual-added");
  await page.getByRole("button", { name: /^add Add$/ }).click();
  await page.locator("code").filter({ hasText: "display-prefix/manual-added" }).waitFor();
  state.holdImport = false;
  state.releaseImport();
  await page.getByText("Imported 3 model(s).", { exact: true }).waitFor();
  for (const model of ["fresh", "name-fallback", "model-fallback"]) await page.locator("code").filter({ hasText: `display-prefix/${model}` }).waitFor();
  const writes = () => state.writes.filter(write => write.path === "/api/models/custom" && write.method === "POST");
  assert.deepEqual(writes().map(write => write.body), ["manual-added", "fresh", "name-fallback", "model-fallback"].map(model => ({ providerAlias: id, id: model, type: "llm" })));
  assert.deepEqual(state.custom.find(model => model.id === "existing").opencode, { limit: { context: 123456 } });
  assert.equal(state.aliases["legacy-alias"], `${id}/legacy`);

  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.getByText("No new models were added.", { exact: true }).waitFor();
  assert.equal(writes().length, 4);
  state.importData = { models: [{ id: "concurrent-model" }] };
  state.concurrentCustomId = "concurrent-model";
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.locator("code").filter({ hasText: "display-prefix/concurrent-model" }).waitFor();
  assert.equal(writes().length, 5);
  assert.deepEqual(state.custom.find(model => model.id === "concurrent-model").opencode, { limit: { context: 654321 } }, "an added:false race must preserve metadata and refresh the existing row");
  state.importData = { models: [{ id: "uncertain-model" }] };
  state.malformedCustomId = "uncertain-model";
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.getByText("Failed to import models", { exact: true }).waitFor();
  await page.locator("code").filter({ hasText: "display-prefix/uncertain-model" }).waitFor();
  assert.equal(writes().length, 6, "a malformed write reply reports an error but still reconciles saved rows");
  state.importData = { models: [] };
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.getByText("No models returned from /models.", { exact: true }).waitFor();
  state.importStatus = 503;
  state.importData = { error: "Synthetic models failure" };
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.getByText("Synthetic models failure", { exact: true }).waitFor();
  assert.equal(writes().length, 6, "empty and failed responses do not write models");

  state.importStatus = 200;
  state.importData = { models: [{ id: "partial-one" }, { id: "failed-id" }, { id: "partial-two" }] };
  state.rejectCustomIds = ["failed-id"];
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.getByText("Imported 2 model(s); failed to import 1.", { exact: true }).waitFor();
  assert.ok(state.custom.some(model => model.id === "partial-one"));
  assert.ok(state.custom.some(model => model.id === "partial-two"));
  assert.ok(!state.custom.some(model => model.id === "failed-id"));
  state.rejectCustomIds = [];

  await page.getByText("Failed to import models", { exact: true }).waitFor({ state: "detached" });
  const failImport = route => route.abort("failed");
  await page.route("**/api/providers/first-default/models", failImport);
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.getByText("Failed to import models", { exact: true }).waitFor();
  await page.unroute("**/api/providers/first-default/models", failImport);

  // A user deletion while discovery is pending must not be silently undone
  // by recreating the same ID without its former metadata.
  state.custom.push({ providerAlias: id, id: "deleted-during-import", name: "User metadata", type: "llm", opencode: { limit: { context: 345678 } } });
  await page.reload();
  const deletedModel = page.getByText("deleted-during-import", { exact: true }).locator("xpath=../..");
  await deletedModel.waitFor();
  state.importData = { models: [{ id: "deleted-during-import" }] };
  state.holdImport = true;
  delete state.releaseImport;
  const beforeDeletedImport = writes().length;
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await waitForFixture(() => typeof state.releaseImport === "function");
  await deletedModel.getByTitle("Remove model").click();
  await deletedModel.waitFor({ state: "detached" });
  state.holdImport = false;
  state.releaseImport();
  await page.getByRole("button", { name: /Import from \/models/ }).waitFor();
  assert.equal(writes().length, beforeDeletedImport, "an ID present at click must remain excluded after a user deletion");
  assert.equal(state.custom.some(model => model.id === "deleted-during-import"), false);
  await page.getByText("No new models were added.", { exact: true }).waitFor();

  // A committed write with a failed final read must remain visibly uncertain
  // and recover via a read-only retry, not another model import/write.
  state.importData = { models: [{ id: "refresh-required" }] };
  state.customReadStatus = 503;
  const beforeRefreshFailure = writes().length;
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  const refreshAlert = page.getByRole("alert").filter({ hasText: "Could not refresh saved models" });
  await refreshAlert.waitFor();
  assert.equal(writes().length, beforeRefreshFailure + 1);
  assert.ok(state.custom.some(model => model.id === "refresh-required"), "the synthetic backend committed the write");
  assert.equal(await page.locator("code").filter({ hasText: "display-prefix/refresh-required" }).count(), 0);
  assert.equal(await page.getByText("Imported 1 model(s).", { exact: true }).count(), 0, "a stale list must not report visible success");
  assert.equal(await page.getByRole("button", { name: /Import from \/models/ }).isDisabled(), true);
  const beforeRetryRead = state.customReads;
  await refreshAlert.getByRole("button", { name: "Retry" }).click();
  await waitForFixture(() => state.customReads > beforeRetryRead);
  await refreshAlert.getByRole("button", { name: "Retry" }).waitFor();
  assert.equal(writes().length, beforeRefreshFailure + 1, "retry never repeats writes");
  state.customReadStatus = 0;
  await refreshAlert.getByRole("button", { name: "Retry" }).click();
  await refreshAlert.waitFor({ state: "detached" });
  await page.locator("code").filter({ hasText: "display-prefix/refresh-required" }).waitFor();
  assert.equal(writes().length, beforeRefreshFailure + 1);

  state.importData = { models: [{ id: "stale-partial-ok" }, { id: "stale-partial-fail" }] };
  state.rejectCustomIds = ["stale-partial-fail"];
  state.customReadStatus = 503;
  const beforePartialRefresh = writes().length;
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await refreshAlert.waitFor();
  await page.getByText("Imported 1 model(s); failed to import 1.", { exact: true }).waitFor();
  assert.equal(writes().length, beforePartialRefresh + 2);
  state.customReadStatus = 0;
  state.rejectCustomIds = [];
  await refreshAlert.getByRole("button", { name: "Retry" }).click();
  await refreshAlert.waitFor({ state: "detached" });
  await page.locator("code").filter({ hasText: "display-prefix/stale-partial-ok" }).waitFor();
  assert.equal(writes().length, beforePartialRefresh + 2);

  state.importData = { models: [{ id: "refresh-unauthorized" }] };
  state.customReadStatus = 401;
  const beforeRefreshUnauthorized = writes().length;
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.waitForURL(`${origin}/login`);
  assert.equal(writes().length, beforeRefreshUnauthorized + 1);
  assert.ok(state.custom.some(model => model.id === "refresh-unauthorized"));
  state.customReadStatus = 0;
  state.expired = false;
  await page.goto(`${origin}/dashboard/providers/${id}`);
  await page.getByRole("button", { name: /Import from \/models/ }).waitFor();

  const beforeUnauthorized = writes().length;
  state.importStatus = 401;
  state.importData = { error: "expired" };
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.waitForURL(`${origin}/login`);
  assert.equal(writes().length, beforeUnauthorized);

  state.expired = false;
  state.importStatus = 200;
  state.importData = { models: [{ id: "before-401" }, { id: "needs-auth" }, { id: "after-401" }] };
  state.unauthorizedCustomId = "needs-auth";
  await page.goto(`${origin}/dashboard/providers/${id}`);
  await page.getByRole("button", { name: /Import from \/models/ }).click();
  await page.waitForURL(`${origin}/login`);
  assert.ok(state.custom.some(model => model.id === "before-401"));
  assert.ok(!writes().some(write => write.body.id === "after-401"), "no additional writes follow a 401");
}

async function runCompatibleNodeFixture(page, origin) {
  await runUuidCompatibleNodeFixture(page, origin);
  state.writes.length = 0;
  const openId = "openai-compatible-fixture";
  const anthropicId = "anthropic-compatible-fixture";
  state.nodes.push({ id: anthropicId, name: "Fixture Anthropic", type: "anthropic-compatible", prefix: "anthropic-prefix", baseUrl: "https://anthropic.example.invalid/v1" });
  state.connections.push({ id: "node-connection", provider: openId, name: "Preserved account", authType: "apikey", isActive: true });
  state.custom.push({ providerAlias: openId, id: "node-model", type: "llm", name: "Preserved model", opencode: { limit: { context: 123456 } } });
  const nodeWrites = () => state.writes.filter(write => write.path.startsWith("/api/provider-nodes/"));
  const details = title => page.getByRole("heading", { name: title }).locator("xpath=../..");
  const edit = title => details(title).getByRole("button", { name: /^edit Edit$/ });
  const remove = title => details(title).getByRole("button", { name: /^delete Delete$/ });
  await page.goto(`${origin}/dashboard/providers/${openId}`);
  await page.getByRole("heading", { name: "Fixture Node" }).waitFor();
  await page.locator("code").filter({ hasText: "fixture-prefix/node-model" }).waitFor();
  await edit("OpenAI Compatible Details").click();
  let dialog = page.getByRole("dialog", { name: "Edit OpenAI Compatible" });
  assert.equal(await dialog.getByLabel("Name", { exact: true }).inputValue(), "Fixture Node");
  assert.equal(await dialog.getByLabel("Prefix", { exact: true }).inputValue(), "fixture-prefix");
  assert.equal(await dialog.getByLabel("Base URL", { exact: true }).inputValue(), "https://example.invalid/v1");
  assert.equal(await dialog.getByLabel("API Type", { exact: true }).inputValue(), "responses");
  assert.equal(await dialog.getByLabel("API Key (for Check)").inputValue(), "");
  assert.equal(await dialog.getByRole("button", { name: "Check", exact: true }).isDisabled(), true);
  await dialog.getByLabel("Name", { exact: true }).fill("\uFEFF ");
  assert.equal(await dialog.getByRole("button", { name: "Save", exact: true }).isDisabled(), true);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(nodeWrites().length, 0, "cancel never writes the node");
  await edit("OpenAI Compatible Details").click();
  dialog = page.getByRole("dialog", { name: "Edit OpenAI Compatible" });
  await dialog.getByLabel("Name", { exact: true }).fill("  Renamed Node  ");
  await dialog.getByLabel("Prefix", { exact: true }).fill("new-route");
  await dialog.getByLabel("Base URL", { exact: true }).fill("https://updated.example.invalid/v1/");
  await dialog.getByLabel("API Type", { exact: true }).selectOption("chat");
  await dialog.getByLabel("API Key (for Check)").fill("synthetic-check-key");
  await dialog.getByLabel("Model ID (optional)").fill("\uFEFF fixture-model \uFEFF");
  state.holdNodeCheck = true;
  await dialog.getByRole("button", { name: "Check", exact: true }).click();
  await waitForFixture(() => typeof state.releaseNodeCheck === "function");
  await dialog.getByLabel("API Key (for Check)").fill("synthetic-replacement-key");
  state.nodeCheckValid = false;
  state.releaseNodeCheck();
  await dialog.getByRole("button", { name: "Check", exact: true }).click();
  await dialog.getByText("Invalid", { exact: true }).waitFor();
  await page.waitForTimeout(150);
  assert.equal(await dialog.getByText("Valid", { exact: true }).count(), 0, "stale check results cannot validate an edited key");
  assert.deepEqual(nodeWrites().at(-1).body, { baseUrl: "https://updated.example.invalid/v1/", apiKey: "synthetic-replacement-key", type: "openai-compatible", modelId: "fixture-model" });
  state.nodeCheckValid = true;
  await dialog.getByRole("button", { name: "Check", exact: true }).click();
  await dialog.getByText("Valid", { exact: true }).waitFor();
  state.rejectNodeSave = true;
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await dialog.getByRole("alert").getByText("Synthetic node update rejected").waitFor();
  assert.equal(state.nodes.find(node => node.id === openId).name, "Fixture Node");
  assert.equal(await dialog.getByLabel("Name", { exact: true }).inputValue(), "  Renamed Node  ");
  state.rejectNodeSave = false;
  const abortSave = route => route.request().method() === "PUT" ? route.abort() : route.continue();
  await page.route(`**/api/provider-nodes/${openId}`, abortSave);
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await dialog.getByRole("alert").getByText("Failed to update compatible node").waitFor();
  await page.unroute(`**/api/provider-nodes/${openId}`, abortSave);
  state.holdNodeSave = true;
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await waitForFixture(() => typeof state.releaseNodeSave === "function");
  assert.equal(await dialog.getByRole("button", { name: "Saving..." }).isDisabled(), true);
  const pendingWrites = nodeWrites().filter(write => write.method === "PUT").length;
  await dialog.getByRole("button", { name: "Close" }).click();
  await dialog.waitFor({ state: "detached" });
  const editButton = edit("OpenAI Compatible Details");
  assert.equal(await editButton.isDisabled(), true, "a pending save must prevent reopening stale fields");
  await editButton.click({ force: true });
  assert.equal(await page.getByRole("dialog", { name: "Edit OpenAI Compatible" }).count(), 0);
  assert.equal(nodeWrites().filter(write => write.method === "PUT").length, pendingWrites, "the dismissed save must not cause a second write");
  state.releaseNodeSave();
  await page.locator("main h1").filter({ hasText: "Renamed Node" }).waitFor();
  const save = nodeWrites().filter(write => write.method === "PUT").at(-1);
  assert.equal(save.path, `/api/provider-nodes/${openId}`);
  assert.deepEqual(save.body, { name: "  Renamed Node  ", prefix: "new-route", baseUrl: "https://updated.example.invalid/v1/", apiType: "chat" });
  await editButton.click();
  const reopened = page.getByRole("dialog", { name: "Edit OpenAI Compatible" });
  assert.equal(await reopened.getByLabel("Name", { exact: true }).inputValue(), "  Renamed Node  ");
  assert.equal(await reopened.getByLabel("Prefix", { exact: true }).inputValue(), "new-route");
  assert.equal(await reopened.getByLabel("API Type", { exact: true }).inputValue(), "chat");
  await reopened.getByLabel("Base URL", { exact: true }).fill("https://followup.example.invalid/v1/");
  await reopened.getByRole("button", { name: "Save", exact: true }).click();
  await reopened.waitFor({ state: "detached" });
  assert.deepEqual(nodeWrites().filter(write => write.method === "PUT").at(-1).body, { name: "  Renamed Node  ", prefix: "new-route", baseUrl: "https://followup.example.invalid/v1/", apiType: "chat" }, "a later URL-only edit must retain the first saved fields");
  assert.deepEqual(state.nodes.find(node => node.id === openId), { id: openId, name: "  Renamed Node  ", type: "openai-compatible", prefix: "new-route", apiType: "chat", baseUrl: "https://followup.example.invalid/v1/" });
  await page.getByText("Chat Completions · https://followup.example.invalid/v1/chat/completions", { exact: true }).waitFor();
  await page.locator("code").filter({ hasText: "new-route/node-model" }).waitFor();
  assert.deepEqual(state.custom.find(item => item.id === "node-model").opencode, { limit: { context: 123456 } });
  assert.ok(state.connections.some(conn => conn.id === "node-connection"));
  await page.reload();
  await page.locator("main h1").filter({ hasText: "Renamed Node" }).waitFor();
  await edit("OpenAI Compatible Details").click();
  dialog = page.getByRole("dialog", { name: "Edit OpenAI Compatible" });
  assert.equal(await dialog.getByLabel("Prefix", { exact: true }).inputValue(), "new-route");
  assert.equal(await dialog.getByLabel("API Key (for Check)").inputValue(), "", "check keys are not retained on reopen");
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "detached" });

  await page.goto(`${origin}/dashboard/providers/${anthropicId}`);
  await page.getByRole("heading", { name: "Fixture Anthropic" }).waitFor();
  await edit("Anthropic Compatible Details").click();
  dialog = page.getByRole("dialog", { name: "Edit Anthropic Compatible" });
  assert.equal(await dialog.getByLabel("API Type", { exact: true }).count(), 0);
  await dialog.getByLabel("Name", { exact: true }).fill("Anthropic Renamed");
  await dialog.getByLabel("Prefix", { exact: true }).fill("claude-route");
  await dialog.getByLabel("Base URL", { exact: true }).fill("https://second.example.invalid/v1");
  await dialog.getByLabel("API Key (for Check)").fill("synthetic-anthropic-key");
  await dialog.getByRole("button", { name: "Check", exact: true }).click();
  await dialog.getByText("Valid", { exact: true }).waitFor();
  assert.deepEqual(nodeWrites().at(-1).body, { baseUrl: "https://second.example.invalid/v1", apiKey: "synthetic-anthropic-key", type: "anthropic-compatible" });
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  assert.deepEqual(nodeWrites().filter(write => write.method === "PUT").at(-1).body, { name: "Anthropic Renamed", prefix: "claude-route", baseUrl: "https://second.example.invalid/v1" });
  await page.getByText("Messages API · https://second.example.invalid/v1/messages", { exact: true }).waitFor();
  await edit("Anthropic Compatible Details").click();
  dialog = page.getByRole("dialog", { name: "Edit Anthropic Compatible" });
  await dialog.getByLabel("Name", { exact: true }).fill("Unauthorized change");
  state.expired = true;
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${origin}/login`);
  assert.equal(state.nodes.find(node => node.id === anthropicId).name, "Anthropic Renamed");
  state.expired = false;

  await page.goto(`${origin}/dashboard/providers/${openId}`);
  await remove("OpenAI Compatible Details").click();
  let confirm = page.getByRole("dialog", { name: "Delete compatible node" });
  await confirm.getByText("Delete this OpenAI Compatible node", { exact: false }).waitFor();
  await confirm.getByRole("button", { name: "Cancel", exact: true }).click();
  assert.equal(nodeWrites().filter(write => write.method === "DELETE").length, 0);
  await remove("OpenAI Compatible Details").click();
  confirm = page.getByRole("dialog", { name: "Delete compatible node" });
  state.rejectNodeDelete = true;
  await confirm.getByRole("button", { name: "Delete", exact: true }).click();
  await confirm.waitFor({ state: "detached" });
  await page.getByText("Failed to delete node", { exact: true }).waitFor();
  assert.ok(state.nodes.some(node => node.id === openId));
  assert.ok(state.connections.some(conn => conn.id === "node-connection"));
  state.rejectNodeDelete = false;
  state.holdNodeDelete = true;
  await remove("OpenAI Compatible Details").click();
  confirm = page.getByRole("dialog", { name: "Delete compatible node" });
  await confirm.getByRole("button", { name: "Delete", exact: true }).click();
  await waitForFixture(() => typeof state.releaseNodeDelete === "function");
  await confirm.getByRole("button", { name: "Close" }).click();
  await confirm.waitFor({ state: "detached" });
  state.releaseNodeDelete();
  await page.waitForURL(`${origin}/dashboard/providers`);
  assert.ok(!state.nodes.some(node => node.id === openId));
  assert.ok(state.nodes.some(node => node.id === anthropicId));
  assert.ok(state.connections.some(conn => conn.id === "node-connection"), "node deletion does not delete connections");
  assert.ok(state.custom.some(item => item.providerAlias === openId && item.id === "node-model"), "node deletion does not delete custom models");

  await page.goto(`${origin}/dashboard/providers/${anthropicId}`);
  await remove("Anthropic Compatible Details").click();
  confirm = page.getByRole("dialog", { name: "Delete compatible node" });
  state.releaseNodeDelete = undefined;
  state.holdNodeDelete = true;
  await confirm.getByRole("button", { name: "Delete", exact: true }).click();
  await waitForFixture(() => typeof state.releaseNodeDelete === "function");
  await confirm.getByRole("button", { name: "Close" }).click();
  await confirm.waitFor({ state: "detached" });
  state.expired = true;
  state.releaseNodeDelete();
  await page.waitForURL(`${origin}/login`);
  assert.ok(state.nodes.some(node => node.id === anthropicId), "a 401 never deletes a node");
}

async function main() {
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://${process.env.PROVIDER_POPUP_ONLY === "1" ? "localhost" : "127.0.0.1"}:${server.address().port}`;
  state.addCounter = 0;
  state.oauthPolls = 0;
  const browser = await chromium.launch({ executablePath: process.env.CHROME_BIN || undefined, chromiumSandbox: true, headless: true });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    const errors = []; page.on("pageerror", error => errors.push(String(error)));
    await page.context().route("**/*", route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    if (process.env.PROVIDER_XAI_FORCE_ONLY === "1") {
      await runXaiForceFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic xAI catalog force-refresh smoke passed");
      return;
    }
    if (process.env.PROVIDER_CONTEXT_ONLY === "1") {
      await runContextFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic provider context smoke passed");
      return;
    }
    if (process.env.PROVIDER_CODEX_CONTROLS_ONLY === "1") {
      await runCodexControlsFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic Codex detail controls smoke passed");
      return;
    }
    if (process.env.PROVIDER_AUTOPING_ONLY === "1") {
      await runAutoPingFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic provider auto-ping smoke passed");
      return;
    }
    if (process.env.PROVIDER_POPUP_ONLY === "1") {
      await runPopupFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic same-origin OAuth popup smoke passed");
      return;
    }
    if (process.env.PROVIDER_POPUP_PROBE === "1") {
      await runPopupProbe(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic retained-popup probe passed");
      return;
    }
    if (process.env.PROVIDER_CALLBACK_ONLY === "1") {
      await runCallbackFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic OAuth callback smoke passed");
      return;
    }
    if (process.env.PROVIDER_IMPORT_ONLY === "1") {
      await runCompatibleImportFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic compatible model import smoke passed");
      return;
    }
    if (process.env.PROVIDER_NODE_ONLY === "1") {
      await runCompatibleNodeFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic compatible node edit/delete smoke passed");
      return;
    }
    if (process.env.PROVIDER_AUTH_ONLY === "1") {
      await runAuthFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic provider authorization smoke passed");
      return;
    }
    if (process.env.PROVIDER_CONNECTION_ONLY === "1") {
      await runConnectionFixture(page, origin);
      assert.deepEqual(errors, []);
      console.log("synthetic provider connection smoke passed");
      return;
    }
    const code = id => page.locator("code").filter({ hasText: `kc/${id}` });
    const row = id => code(id).locator("xpath=../..");
    state.aliases["10"] = "kc/duplicate-alias";
    state.aliases["2"] = "kc/duplicate-alias";
    state.numericAliases = true;
    await page.goto(`${origin}/dashboard/providers/kilocode?keep=1#bookmark`);
    await page.locator("main").getByRole("heading", { name: "Kilo Code" }).waitFor();
    assert.equal(await page.getByRole("heading", { name: "Kilo Code" }).count(), 2, "provider breadcrumb and page heading");
    assert.equal(new URL(page.url()).search, "?keep=1");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    await code("fixture-standard").waitFor();
    assert.equal(await page.getByText("Other account").count(), 0);
    await page.getByText("Fixture account").waitFor();
    await page.getByRole("checkbox", { name: "Select connection Fixture account" }).waitFor();
    await page.getByText("~200 req/hr", { exact: true }).waitFor();
    await page.getByText("Kilo Code account (free) grants gateway access.").waitFor();
    await row("fixture-standard").getByText("My Standard").waitFor();
    await row("fixture-standard").getByText("128K ctx").waitFor();
    await row("fixture-standard").getByText("visibility", { exact: true }).waitFor();
    await row("fixture-standard").getByText("neurology", { exact: true }).waitFor();
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin });
    await row("fixture-standard").getByRole("button", { name: "Copy model" }).click();
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "kc/fixture-standard");
    state.custom.push({ providerAlias: "kc", id: "gpt-5-codex", type: "llm" });
    await page.reload();
    await code("gpt-5-codex").waitFor();
    const thinking = page.getByTitle("Appends (level) suffix to copied model names");
    await thinking.selectOption("high");
    await page.locator("code").filter({ hasText: "kc/gpt-5-codex(high)" }).waitFor();
    await row("gpt-5-codex").getByRole("button", { name: "Copy model" }).click();
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "kc/gpt-5-codex(high)");
    const modelsThinking = page.locator("select").filter({ has: page.getByRole("option", { name: "Thinking: High" }) });
    assert.equal(await modelsThinking.inputValue(), "high");
    await modelsThinking.selectOption("auto");
    assert.equal(await thinking.inputValue(), "auto");
    await row("gpt-5-codex").getByTitle("Remove custom model").click();
    await code("gpt-5-codex").waitFor({ state: "detached" });

    const fixtureRow = page.getByText("Fixture account", { exact: true }).locator("xpath=../../..");
    await fixtureRow.getByRole("button", { name: /edit Edit/ }).click();
    let edit = page.getByRole("dialog", { name: "Edit Connection" });
    await edit.getByLabel("Display Name").fill("Fixture label");
    assert.equal(await edit.getByLabel("API Key").count(), 0, "OAuth edits do not expose a credential input");
    await edit.getByRole("button", { name: "Save" }).click();
    await edit.waitFor({ state: "detached" });
    let update = state.writes.filter(w => w.method === "PUT" && w.path === "/api/providers/connection-one").at(-1).body;
    assert.deepEqual(update, { name: "Fixture account", email: "", displayName: "Fixture label", priority: 1, proxyPoolId: null });
    assert.equal(Object.hasOwn(update, "apiKey"), false);

    await page.getByRole("button", { name: /key API Key$/ }).click();
    let keyDialog = page.getByRole("dialog", { name: "Add Kilo Code API Key" });
    await keyDialog.getByLabel("Name").fill("Fixture key");
    await keyDialog.getByLabel("API Key").fill("fixture-only-key");
    state.rejectAdd = true;
    await keyDialog.getByRole("button", { name: "Save" }).click();
    await keyDialog.getByRole("alert").getByText("Synthetic add rejected").waitFor();
    assert.equal(await keyDialog.getByLabel("API Key").inputValue(), "fixture-only-key");
    state.rejectAdd = false;
    await keyDialog.getByRole("button", { name: "Save" }).click();
    await keyDialog.waitFor({ state: "detached" });
    await page.getByText("Fixture key", { exact: true }).waitFor();
    assert.deepEqual(state.writes.filter(w => w.path === "/api/providers" && w.method === "POST").at(-1).body, { provider: "kilocode", name: "Fixture key", apiKey: "fixture-only-key", priority: 1, proxyPoolId: null, testStatus: "active" });
    let keyRow = page.getByText("Fixture key", { exact: true }).locator("xpath=../../..");
    await keyRow.getByRole("button", { name: /edit Edit/ }).click();
    edit = page.getByRole("dialog", { name: "Edit Connection" });
    assert.equal(await edit.getByLabel("API Key").inputValue(), "", "stored secret is never prefilled");
    await edit.getByRole("button", { name: "Check" }).click();
    await edit.getByText("Saved key works").waitFor();
    assert.ok(state.writes.some(w => w.method === "POST" && w.path === "/api/providers/fixture-added-1/test"));
    await edit.getByLabel("Display Name").fill("Edited key label");
    await edit.getByRole("button", { name: "Save" }).click();
    await edit.waitFor({ state: "detached" });
    update = state.writes.filter(w => w.method === "PUT" && w.path === "/api/providers/fixture-added-1").at(-1).body;
    assert.equal(Object.hasOwn(update, "apiKey"), false, "blank replacement preserves stored secret");
    keyRow = page.getByText("Fixture key", { exact: true }).locator("xpath=../../..");
    await keyRow.getByRole("button", { name: /edit Edit/ }).click();
    edit = page.getByRole("dialog", { name: "Edit Connection" });
    state.validationValid = false;
    await edit.getByLabel("API Key").fill("fixture-invalid-key");
    await edit.getByRole("button", { name: "Save" }).click();
    await edit.waitFor({ state: "detached" });
    update = state.writes.filter(w => w.method === "PUT" && w.path === "/api/providers/fixture-added-1").at(-1).body;
    assert.equal(update.apiKey, "fixture-invalid-key");
    assert.equal(Object.hasOwn(update, "testStatus"), false, "failed validation must not mark key active");
    state.validationValid = true;
    keyRow = page.getByText("Fixture key", { exact: true }).locator("xpath=../../..");
    await keyRow.getByRole("button", { name: /edit Edit/ }).click();
    edit = page.getByRole("dialog", { name: "Edit Connection" });
    await edit.getByLabel("API Key").fill("fixture-replacement");
    await edit.getByRole("button", { name: "Save" }).click();
    await edit.waitFor({ state: "detached" });
    update = state.writes.filter(w => w.method === "PUT" && w.path === "/api/providers/fixture-added-1").at(-1).body;
    assert.equal(update.apiKey, "fixture-replacement");
    assert.equal(update.testStatus, "active");
    assert.equal(update.lastError, null);
    keyRow = page.getByText("Fixture key", { exact: true }).locator("xpath=../../..");
    await keyRow.getByRole("button", { name: /delete Delete/ }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).getByRole("button", { name: "Delete" }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).waitFor({ state: "detached" });
    await page.getByText("Fixture key", { exact: true }).waitFor({ state: "detached" });

    state.deviceFailure = true;
    await page.getByRole("button", { name: /lock OAuth$/ }).click();
    let oauth = page.getByRole("dialog", { name: "Connect Kilo Code" });
    await oauth.getByRole("alert").getByText("Synthetic device failure").waitFor();
    state.deviceFailure = false;
    await oauth.getByRole("button", { name: "Retry" }).click();
    await oauth.getByText("FIXTURE-123").waitFor();
    await waitForFixture(() => state.oauthPolls > 0);
    assert.deepEqual(state.writes.find(w => w.path === "/api/oauth/kilocode/poll").body, { deviceCode: "fixture-device-code", codeVerifier: "fixture-verifier", extraData: null });
    state.completeDevice = true;
    await oauth.waitFor({ state: "detached" });
    await page.getByText("Device account", { exact: true }).waitFor();
    const deviceRow = page.getByText("Device account", { exact: true }).locator("xpath=../../..");
    await deviceRow.getByRole("button", { name: /delete Delete/ }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).getByRole("button", { name: "Delete" }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).waitFor({ state: "detached" });
    state.completeDevice = false;
    await page.getByRole("button", { name: /lock OAuth$/ }).click();
    oauth = page.getByRole("dialog", { name: "Connect Kilo Code" });
    await oauth.getByText("FIXTURE-123").waitFor();
    await oauth.getByRole("button", { name: "Close" }).click();
    await oauth.waitFor({ state: "detached" });
    await new Promise(resolve => setTimeout(resolve, 60));
    const afterClosePolls = state.oauthPolls;
    await new Promise(resolve => setTimeout(resolve, 120));
    assert.equal(state.oauthPolls, afterClosePolls, "closing OAuth stops polling");
    assert.equal(await page.getByText("image-only").count(), 0);
    state.modelTestResult = { ok: false, error: "Synthetic model failure" };
    await row("fixture-standard").getByText("science", { exact: true }).locator("xpath=..").click();
    await page.getByText("Synthetic model failure", { exact: true }).waitFor();
    await row("fixture-standard").getByText("cancel", { exact: true }).waitFor();
    state.modelTestResult = { ok: true };
    await row("fixture-standard").getByText("science", { exact: true }).locator("xpath=..").click();
    await row("fixture-standard").getByText("check_circle", { exact: true }).waitFor();
    assert.equal(await page.getByText("Synthetic model failure", { exact: true }).count(), 0);
    await row("duplicate-alias").getByTitle("Remove custom model").click();
    await waitForFixture(() => state.writes.some(w => w.path === "/api/models/alias?alias=2" && w.method === "DELETE"));
    assert.equal(state.aliases["10"], "kc/duplicate-alias");
    await row("duplicate-alias").getByTitle("Remove custom model").click();
    await code("duplicate-alias").waitFor({ state: "detached" });
    assert.ok(state.writes.some(w => w.path === "/api/models/alias?alias=10" && w.method === "DELETE"));
    await page.getByTitle("Restore model").filter({ hasText: "custom-disabled" }).click();
    await row("custom-disabled").getByText("Custom Disabled").waitFor();
    await row("fixture-standard").getByTitle("Disable model").click();
    await page.getByTitle("Restore model").filter({ hasText: "fixture-standard" }).waitFor();
    assert.deepEqual(state.writes.find(w => w.method === "POST" && w.path === "/api/models/disabled").body, { providerAlias: "kc", ids: ["fixture-standard"] });
    await page.reload();
    await page.getByTitle("Restore model").filter({ hasText: "fixture-standard" }).click();
    await row("fixture-standard").getByText("My Standard").waitFor();
    await row("fixture-standard").getByRole("button", { name: "Remove from favorites" }).click();
    await row("fixture-standard").getByRole("button", { name: "Add to favorites" }).waitFor();
    await page.getByTitle("Show only free models").click();
    await page.waitForFunction(() => !Array.from(document.querySelectorAll("code")).some(el => el.textContent === "kc/fixture-standard"));
    assert.equal(await code("fixture-free").count(), 1);
    assert.equal(await code("custom-disabled").count(), 1, "custom models are not free-filtered");
    await page.reload();
    await code("fixture-free").waitFor();
    assert.equal(await code("fixture-standard").count(), 0, "free filter persists");
    await page.getByTitle("Show only free models").click();
    await code("fixture-standard").waitFor();
    await page.getByRole("button", { name: /Add Model/ }).click();
    let dialog = page.getByRole("dialog", { name: "Add Custom Model" });
    await dialog.getByPlaceholder("e.g. claude-opus-4-5").fill("  kc/new/model with space  ");
    await dialog.getByRole("button", { name: /Test/ }).click();
    await dialog.getByText("Model is reachable").waitFor();
    state.rejectCustom = true;
    await dialog.getByRole("button", { name: "Add Model" }).click();
    await dialog.getByRole("alert").getByText("Fixture rejection").waitFor();
    state.rejectCustom = false;
    await dialog.getByRole("button", { name: "Add Model" }).click();
    await dialog.waitFor({ state: "detached" });
    await code("new/model with space").waitFor();
    assert.deepEqual(state.writes.filter(w => w.path === "/api/models/custom" && w.method === "POST").at(-1).body, { providerAlias: "kc", id: "new/model with space", type: "llm" });
    assert.deepEqual(state.custom.find(m => m.id === "fixture-standard").opencode, { limit: { context: 123456 } }, "unrelated user metadata remains unchanged");
    await page.reload();
    await code("new/model with space").waitFor();
    await row("new/model with space").getByTitle("Remove custom model").click();
    await code("new/model with space").waitFor({ state: "detached" });
    assert.equal(state.writes.filter(w => w.method === "DELETE" && w.path.startsWith("/api/models/custom?")).at(-1).path, "/api/models/custom?providerAlias=kc&id=new%2Fmodel+with+space&type=llm");
    await row("legacy-model").getByTitle("Remove custom model").click();
    await code("legacy-model").waitFor({ state: "detached" });
    assert.ok(state.writes.some(w => w.method === "DELETE" && w.path === "/api/models/alias?alias=legacy"));
    assert.equal(state.aliases.builtin, "kc/fixture-standard");
    state.holdCustom = true;
    await page.getByRole("button", { name: /Add Model/ }).click();
    dialog = page.getByRole("dialog", { name: "Add Custom Model" });
    await dialog.getByPlaceholder("e.g. claude-opus-4-5").fill("kc/after-close");
    await dialog.getByRole("button", { name: "Add Model" }).click();
    await waitForFixture(() => typeof state.releaseCustom === "function");
    await dialog.getByRole("button", { name: "Close" }).click();
    await dialog.waitFor({ state: "detached" });
    state.releaseCustom();
    state.holdCustom = false;
    await code("after-close").waitFor();
    await row("after-close").getByTitle("Remove custom model").click();
    await code("after-close").waitFor({ state: "detached" });
    await page.getByRole("button", { name: /Disable All/ }).click();
    const confirm = page.getByRole("dialog", { name: "Disable all models" });
    await confirm.getByRole("button", { name: "Disable all" }).click();
    await page.getByTitle("Restore model").filter({ hasText: "fixture-standard" }).waitFor();
    assert.deepEqual(state.writes.filter(w => w.path === "/api/models/disabled" && w.method === "POST").at(-1).body, { providerAlias: "kc", ids: ["custom-disabled", "fixture-standard", "fixture-free"] });
    await page.getByRole("button", { name: /Active All/ }).click();
    await code("fixture-standard").waitFor();
    assert.deepEqual(state.disabled.kc, []);
    await page.getByRole("switch", { name: "Disable connection" }).click();
    await page.getByRole("switch", { name: "Enable connection" }).waitFor();
    assert.deepEqual(state.writes.filter(w => w.path === "/api/providers/connection-one" && w.method === "PUT").at(-1).body, { isActive: false });
    state.holdDelete = true;
    await page.getByRole("button", { name: /delete Delete/ }).click();
    const deleteDialog = page.getByRole("dialog", { name: "Delete connection" });
    await deleteDialog.getByRole("button", { name: "Delete" }).click();
    await waitForFixture(() => typeof state.releaseDelete === "function");
    await deleteDialog.getByRole("button", { name: "Close" }).click();
    await deleteDialog.waitFor({ state: "detached" });
    state.releaseDelete();
    state.holdDelete = false;
    await page.getByText("Fixture account").waitFor({ state: "detached" });
    assert.ok(state.writes.some(w => w.path === "/api/providers/connection-one" && w.method === "DELETE"));
    state.connections.push(
      { id: "bulk-one", provider: "kilocode", name: "Bulk One", isActive: true, priority: 0 },
      { id: "bulk-two", provider: "kilocode", name: "Bulk Two", isActive: true, priority: 1 },
    );
    state.pools = [{ id: "pool-a", name: "Pool A", isActive: true }, { id: "pool-b", name: "Pool B", isActive: true }];
    await page.reload();
    await page.getByRole("checkbox", { name: "Select connection Bulk One" }).waitFor();
    const bulkOneRow = page.getByText("Bulk One", { exact: true }).locator("xpath=../..");
    await bulkOneRow.getByText("keyboard_arrow_down", { exact: true }).locator("xpath=..").click();
    await waitForFixture(() => state.writes.filter(w => w.method === "PUT" && ["/api/providers/bulk-one", "/api/providers/bulk-two"].includes(w.path)).length >= 2);
    assert.deepEqual(Object.fromEntries(state.writes.filter(w => w.method === "PUT" && ["/api/providers/bulk-one", "/api/providers/bulk-two"].includes(w.path)).slice(-2).map(w => [w.path, w.body])), { "/api/providers/bulk-two": { priority: 0 }, "/api/providers/bulk-one": { priority: 1 } });
    await bulkOneRow.getByText("keyboard_arrow_up", { exact: true }).locator("xpath=..").click();
    await waitForFixture(() => state.writes.filter(w => w.method === "PUT" && ["/api/providers/bulk-one", "/api/providers/bulk-two"].includes(w.path)).length >= 4);
    await page.getByRole("button", { name: /Apply Proxy/ }).click();
    let proxyDialog = page.getByRole("dialog", { name: "Apply Proxy (2 connections)" });
    await proxyDialog.getByRole("button", { name: /One-to-one/ }).click();
    await proxyDialog.waitFor({ state: "detached" });
    assert.deepEqual(state.writes.filter(w => w.method === "PUT" && ["/api/providers/bulk-one", "/api/providers/bulk-two"].includes(w.path)).slice(-2).map(w => w.body), [{ proxyPoolId: "pool-a" }, { proxyPoolId: "pool-b" }]);
    await page.getByRole("button", { name: /Apply Proxy/ }).click();
    proxyDialog = page.getByRole("dialog", { name: "Apply Proxy (2 connections)" });
    await proxyDialog.getByRole("button", { name: /None \(unbind all\)/ }).click();
    await proxyDialog.waitFor({ state: "detached" });
    assert.deepEqual(state.writes.filter(w => w.method === "PUT" && ["/api/providers/bulk-one", "/api/providers/bulk-two"].includes(w.path)).slice(-2).map(w => w.body), [{ proxyPoolId: null }, { proxyPoolId: null }]);
    await page.getByRole("button", { name: /Test Connection One-by-One/ }).click();
    await page.getByText("Completed: 2", { exact: true }).waitFor();
    await page.getByText("failed: Synthetic connection failure", { exact: true }).waitFor();
    const connectionTests = state.writes.filter(w => /\/api\/providers\/bulk-(one|two)\/test/.test(w.path));
    assert.deepEqual(connectionTests.map(w => w.path), ["/api/providers/bulk-one/test", "/api/providers/bulk-two/test"]);
    assert.ok(connectionTests[1].at - connectionTests[0].at >= 900, "connection tests are paced");
    state.holdConnectionTest = true;
    await page.getByRole("button", { name: /Test Connection One-by-One/ }).click();
    await waitForFixture(() => typeof state.releaseConnectionTest === "function");
    await page.getByRole("button", { name: /Stop/ }).click();
    state.releaseConnectionTest(); state.holdConnectionTest = false;
    await page.getByText("Stopped", { exact: true }).waitFor();
    assert.equal(state.writes.filter(w => /\/api\/providers\/bulk-(one|two)\/test/.test(w.path)).length, 3, "stop prevents the next connection test");
    await page.getByRole("checkbox", { name: "Select All" }).check();
    assert.equal(await page.getByRole("checkbox", { name: "Select connection Bulk One" }).isChecked(), true);
    assert.equal(await page.getByRole("checkbox", { name: "Select connection Bulk Two" }).isChecked(), true);
    state.failDeleteId = "bulk-two";
    await page.getByRole("button", { name: /Delete Selected \(2\)/ }).click();
    let bulkDialog = page.getByRole("dialog", { name: "Delete 2 Connections" });
    await bulkDialog.getByRole("button", { name: "Delete" }).click();
    await bulkDialog.waitFor({ state: "detached" });
    assert.equal(await page.getByRole("checkbox", { name: "Select connection Bulk One" }).count(), 0);
    assert.equal(await page.getByRole("checkbox", { name: "Select connection Bulk Two" }).isChecked(), true, "failed delete stays visible and selected");
    state.failDeleteId = null;
    await page.getByRole("button", { name: /Delete Selected \(1\)/ }).click();
    bulkDialog = page.getByRole("dialog", { name: "Delete 1 Connection" });
    await bulkDialog.getByRole("button", { name: "Delete" }).click();
    await page.getByRole("checkbox", { name: "Select connection Bulk Two" }).waitFor({ state: "detached" });
    assert.deepEqual(state.writes.filter(w => w.method === "DELETE" && ["/api/providers/bulk-one", "/api/providers/bulk-two"].includes(w.path)).map(w => w.path), ["/api/providers/bulk-one", "/api/providers/bulk-two", "/api/providers/bulk-two"]);
    state.pools = [];
    state.holdCatalogOnce = true;
    await page.goto(`${origin}/dashboard/providers/codex`);
    await waitForFixture(() => typeof state.releaseCatalog === "function");
    await page.getByRole("switch", { name: "Disable connection" }).click();
    await page.locator("code").filter({ hasText: "cx/codex-inactive" }).waitFor();
    const staleCatalog = page.waitForResponse(response => response.url().endsWith("/api/catalog") && response.request().method() === "GET");
    state.releaseCatalog();
    await staleCatalog;
    await page.waitForTimeout(100);
    assert.equal(await page.locator("code").filter({ hasText: "cx/codex-active" }).count(), 0, "older catalog must not overwrite refresh");
    await page.locator("code").filter({ hasText: "cx/codex-base" }).waitFor();
    await page.locator("code").filter({ hasText: "cx/codex-only" }).waitFor();
    assert.equal(await page.getByText("Fixture account").count(), 0);
    await page.getByRole("button", { name: /delete Delete/ }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).getByRole("button", { name: "Delete" }).click();
    await page.locator("code").filter({ hasText: "cx/codex-removed" }).waitFor();
    for (const provider of ["commandcode", "a6api", "xai"]) {
      state.connections.push({ id: `dynamic-${provider}`, provider, name: `Dynamic ${provider}`, isActive: true });
      await page.goto(`${origin}/dashboard/providers/${provider}`);
      await page.locator("code").filter({ hasText: `${provider}/${provider}-active` }).waitFor();
      await page.getByRole("switch", { name: "Disable connection" }).click();
      await page.locator("code").filter({ hasText: `${provider}/${provider}-inactive` }).waitFor();
      await page.getByRole("button", { name: /delete Delete/ }).click();
      await page.getByRole("dialog", { name: "Delete connection" }).getByRole("button", { name: "Delete" }).click();
      await page.locator("code").filter({ hasText: `${provider}/${provider}-removed` }).waitFor();
    }
    await page.goto(`${origin}/dashboard/providers/claude`);
    await page.getByRole("button", { name: /Add Connection/ }).click();
    oauth = page.getByRole("dialog", { name: /Connect Claude/ });
    await oauth.getByLabel("Authorization URL").waitFor();
    await page.waitForFunction(() => document.querySelector('input[aria-label="Authorization URL"]')?.value.includes("fixture-authorize"));
    await oauth.getByLabel("Callback URL").fill("fixture-code#fixture-state");
    await oauth.getByRole("button", { name: "Connect", exact: true }).click();
    await oauth.waitFor({ state: "detached" });
    await page.getByText("Claude fixture", { exact: true }).waitFor();
    assert.deepEqual(state.writes.find(w => w.path === "/api/oauth/claude/exchange").body, { code: "fixture-code#fixture-state", redirectUri: `http://localhost:${server.address().port}/callback`, codeVerifier: "fixture-verifier", state: null });
    await page.goto(`${origin}/dashboard/providers/xai`);
    state.holdProxy = true;
    const stopsBeforeLateStart = state.proxyStops || 0;
    await page.getByRole("button", { name: /Grok Build OAuth/ }).click();
    oauth = page.getByRole("dialog", { name: "Connect Grok Build OAuth" });
    await waitForFixture(() => typeof state.releaseProxy === "function");
    await oauth.getByRole("button", { name: "Close" }).click();
    await oauth.waitFor({ state: "detached" });
    await waitForFixture(() => state.proxyStops === stopsBeforeLateStart + 1);
    state.releaseProxy(); state.holdProxy = false;
    await waitForFixture(() => state.proxyStops === stopsBeforeLateStart + 2 && state.proxyLive === false);
    await page.getByRole("button", { name: /Grok Build OAuth/ }).click();
    oauth = page.getByRole("dialog", { name: "Connect Grok Build OAuth" });
    await page.waitForFunction(() => document.querySelector('input[aria-label="Authorization URL"]')?.value.includes("fixture-authorize"));
    await oauth.getByLabel("Callback URL").fill("fixture-xai-code");
    await oauth.getByRole("button", { name: "Connect", exact: true }).click();
    await oauth.waitFor({ state: "detached" });
    await page.getByText("xAI fixture", { exact: true }).waitFor();
    assert.deepEqual(state.writes.find(w => w.path === "/api/oauth/xai/manual-code").body, { code: "fixture-xai-code", state: "fixture-state" });
    await waitForFixture(() => state.proxyStops > 0);
    state.holdLive = true;
    state.holdFavorites = true;
    await page.goto(`${origin}/dashboard/providers/openrouter`);
    await waitForFixture(() => typeof state.releaseLive === "function" && typeof state.releaseFavorites === "function");
    await page.locator("code").filter({ hasText: "openrouter/openrouter-base" }).waitFor();
    await page.getByRole("button", { name: /Add Model/ }).waitFor();
    const openrouterRow = page.locator("code").filter({ hasText: "openrouter/openrouter-base" }).locator("xpath=../..");
    await openrouterRow.getByRole("button", { name: "Add to favorites" }).click();
    await openrouterRow.getByRole("button", { name: "Remove from favorites" }).waitFor();
    state.releaseFavorites(); state.holdFavorites = false;
    state.releaseLive(); state.holdLive = false;
    await page.locator("code").filter({ hasText: "openrouter/fixture-paid" }).waitFor();
    await openrouterRow.getByRole("button", { name: "Remove from favorites" }).waitFor();
    await page.getByTitle("Show only free models").click();
    await page.locator("code").filter({ hasText: "openrouter/fixture:free" }).waitFor();
    assert.equal(await page.locator("code").filter({ hasText: "openrouter/fixture-paid" }).count(), 0);
    state.connections.push({ id: "compatible-connection", provider: "openai-compatible-fixture", name: "Compatible fixture", isActive: true });
    await page.goto(`${origin}/dashboard/providers/openai-compatible-fixture`);
    await page.getByRole("heading", { name: "Fixture Node" }).waitFor();
    await page.getByLabel("Model ID").fill("vendor/model");
    await page.locator("#new-compatible-model-input").locator("xpath=../..").getByRole("button", { name: /Add/ }).click();
    await page.locator("code").filter({ hasText: "fixture-prefix/vendor/model" }).waitFor();
    assert.deepEqual(state.writes.filter(w => w.path === "/api/models/custom" && w.method === "POST").at(-1).body, { providerAlias: "openai-compatible-fixture", id: "vendor/model", type: "llm" });
    await page.reload();
    await page.locator("code").filter({ hasText: "fixture-prefix/vendor/model" }).waitFor();
    state.holdModelTest = true;
    state.modelTestResult = { ok: false, error: "Synthetic compatible failure" };
    const modelTestCount = state.writes.filter(w => w.path === "/api/models/test").length;
    const compatibleTest = page.getByRole("button", { name: "Test model" });
    await compatibleTest.click();
    await waitForFixture(() => typeof state.releaseModelTest === "function");
    assert.equal(await compatibleTest.isDisabled(), true);
    await compatibleTest.evaluate(button => button.click());
    assert.equal(state.writes.filter(w => w.path === "/api/models/test").length, modelTestCount + 1);
    state.releaseModelTest(); state.holdModelTest = false;
    await page.getByTitle("Synthetic compatible failure").waitFor();
    assert.match(await page.getByTitle("Synthetic compatible failure").locator("xpath=..").getAttribute("class"), /border-red-500/);
    state.modelTestResult = { ok: true };
    await compatibleTest.click();
    await page.getByText("check_circle", { exact: true }).waitFor();
    await page.getByTitle("Remove model").click();
    await page.locator("code").filter({ hasText: "fixture-prefix/vendor/model" }).waitFor({ state: "detached" });
    await page.getByRole("checkbox", { name: "Select connection Compatible fixture" }).locator("xpath=../..").getByRole("button", { name: /delete Delete/ }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).getByRole("button", { name: "Delete" }).click();
    await page.getByRole("dialog", { name: "Delete connection" }).waitFor({ state: "detached" });
    await page.getByRole("heading", { name: "OpenAI Compatible Details" }).locator("xpath=../..").getByRole("button", { name: /Add API Key/ }).click();
    keyDialog = page.getByRole("dialog", { name: "Add Fixture Node API Key" });
    await keyDialog.getByLabel("Name").fill("Compatible key");
    await keyDialog.getByLabel("API Key").fill("fixture-compatible-key");
    await keyDialog.getByLabel("Default Model").fill("  fixture-model  ");
    await keyDialog.getByRole("button", { name: "Save" }).click();
    await keyDialog.waitFor({ state: "detached" });
    assert.equal(state.writes.filter(w => w.path === "/api/providers" && w.method === "POST").at(-1).body.defaultModel, "fixture-model");
    state.failCatalog = true;
    await page.goto(`${origin}/dashboard/providers/codex`);
    await page.getByRole("alert").getByText("Unable to load available models. Saved configuration was not changed.").waitFor();
    assert.equal(await page.getByRole("button", { name: "Disable All" }).count(), 0);
    state.failCatalog = false;
    await page.getByRole("button", { name: "Retry" }).click();
    await page.locator("code").filter({ hasText: "cx/codex-base" }).waitFor();
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole("heading", { name: "Available Models" }).waitFor();
    await page.getByRole("button", { name: /Add Model/ }).click();
    dialog = page.getByRole("dialog", { name: "Add Custom Model" });
    await dialog.getByPlaceholder("e.g. claude-opus-4-5").fill("expired-session-model");
    state.expired = true;
    await dialog.getByRole("button", { name: "Add Model" }).click();
    await page.waitForURL(`${origin}/login`);
    assert.equal(state.custom.some(m => m.id === "expired-session-model"), false);
    state.expired = false;
    state.connections.push({ id: "connection-race", provider: "kilocode", name: "Race account", authType: "apikey", isActive: true });
    state.holdToggle = true;
    await page.goto(`${origin}/dashboard/providers/kilocode`);
    await page.getByText("Race account").waitFor();
    await page.getByRole("switch", { name: "Disable connection" }).click();
    await waitForFixture(() => typeof state.releaseToggle === "function");
    await page.getByRole("button", { name: /delete Delete/ }).click();
    const raceDialog = page.getByRole("dialog", { name: "Delete connection" });
    await raceDialog.getByRole("button", { name: "Delete" }).click();
    await raceDialog.waitFor({ state: "detached" });
    await page.getByText("Race account").waitFor({ state: "detached" });
    state.expired = true;
    state.releaseToggle();
    state.holdToggle = false;
    await page.waitForURL(`${origin}/login`);
    await runAutoPingFixture(page, origin);
    await runXaiForceFixture(page, origin);
    assert.deepEqual(errors, []);
    console.log("synthetic provider detail smoke passed");
  } finally {
    for (const key of ["releaseCustom", "releaseDelete", "releaseToggle", "releaseCatalog", "releaseLive", "releaseFavorites", "releaseModelTest", "releaseConnectionTest", "releaseProxy", "releaseStop", "releaseNodeSave", "releaseImport", "releaseSettingsWrite", "releaseXaiNode", "releaseXaiForce"]) if (typeof state[key] === "function") state[key]();
    await browser.close(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  }
}
main().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
