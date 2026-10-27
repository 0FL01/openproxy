// Focused Quota deletion-feedback regression; only synthetic, same-origin APIs.
// QUOTA_PUBLIC_DIR=/isolated/build/public CHROME_BIN=/path/to/chrome node tests/quota-smoke.cjs
// Set QUOTA_ASTRO_REFERENCE=1 to run the same assertions against local Astro dist.
// Chromium's sandbox stays enabled; no OpenProxy instance or live provider is used.
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

assert(process.env.QUOTA_PUBLIC_DIR, "QUOTA_PUBLIC_DIR must point to an isolated build");
const publicDir = path.resolve(process.env.QUOTA_PUBLIC_DIR);
const astro = process.env.QUOTA_ASTRO_REFERENCE === "1";
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
const connection = { id: "fixture-github", provider: "github", name: "Deletion fixture", authType: "oauth", isActive: true };
const state = { connections: [connection], reads: 0, unexpected: [] };
let releaseDelete, deleteStarted;
const deleteHold = new Promise((resolve) => { releaseDelete = resolve; });
const deleteEntered = new Promise((resolve) => { deleteStarted = resolve; });
let failDelete = true;

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  const respond = (status, data) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(data)); };
  if (url.pathname.startsWith("/api/")) {
    if (req.method === "GET" && url.pathname === "/api/auth/status") return respond(200, { requireLogin: true, authenticated: true });
    if (req.method === "GET" && url.pathname === "/api/build") return respond(200, {});
    if (req.method === "GET" && url.pathname === "/api/settings") return respond(200, { requireLogin: true, hasPassword: true, enableTranslator: false, codexAutoPing: { enabled: false, connections: {} }, glmAutoPing: { enabled: false, connections: {} } });
    if (req.method === "GET" && url.pathname === "/api/catalog") return respond(200, { providerIdToAlias: {}, providerModels: [] });
    if (req.method === "GET" && url.pathname === "/api/proxy-pools" && url.search === "?isActive=true") return respond(200, { proxyPools: [] });
    if (req.method === "GET" && url.pathname === "/api/providers") { state.reads++; return respond(200, { connections: state.connections }); }
    // fetch_github_quota -> build_quota_entry -> /api/usage envelope.
    if (req.method === "GET" && url.pathname === "/api/usage/fixture-github") return respond(200, { connectionId: connection.id, message: "", quotas: { chat: { used: 25, total: 100, remaining: 75, remainingPercentage: 75, resetAt: null, unlimited: false } } });
    if (req.method === "DELETE" && url.pathname === "/api/providers/fixture-github") {
      deleteStarted();
      await deleteHold;
      if (failDelete) return respond(500, { error: "Synthetic deletion failure" });
      state.connections = [];
      return respond(200, { message: "Connection deleted successfully" });
    }
    state.unexpected.push(`${req.method} ${url.pathname}${url.search}`);
    return respond(404, { error: "Unexpected fixture API" });
  }
  const relative = url.pathname === "/dashboard/quota" ? (astro ? "dashboard/quota.html" : "index.html") : url.pathname.slice(1);
  const file = path.resolve(publicDir, relative);
  if (!file.startsWith(publicDir + path.sep)) return res.writeHead(403).end();
  try { const content = await fs.readFile(file); res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" }); res.end(content); }
  catch { res.writeHead(404).end(); }
});

async function main() {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({ executablePath: process.env.CHROME_BIN || undefined, chromiumSandbox: true, headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    const errors = [], writes = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    page.on("request", (request) => {
      const url = new URL(request.url());
      if (url.pathname.startsWith("/api/") && request.method() !== "GET") writes.push({ method: request.method(), path: url.pathname, body: request.postData() });
    });
    await page.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    await page.routeWebSocket("**/*", (socket) => socket.close());
    await page.addInitScript(() => localStorage.setItem("quotaAutoRefresh", "false"));
    await page.goto(`${origin}/dashboard/quota`);
    await page.getByText("Deletion fixture", { exact: true }).waitFor();
    await page.getByText("25 / 100", { exact: true }).waitFor();
    const heading = page.getByRole("heading", { name: "Delete connection", exact: true });
    const modal = page.locator("div.relative.w-full").filter({ has: heading });
    const openDelete = () => page.getByTitle("Delete connection", { exact: true }).click();
    const expectToast = async (message, icon) => {
      const text = page.getByText(message, { exact: true });
      await text.waitFor({ timeout: 3000 });
      const toast = text.locator("xpath=ancestor::div[contains(@class, 'shadow-modal')][1]");
      assert.equal(await toast.getByText(icon, { exact: true }).count(), 1);
      await toast.getByRole("button", { name: "Dismiss notification" }).click();
    };

    // Cancellation sends no mutation. Pending deletion cannot be cancelled and
    // must not announce an outcome before the synthetic response arrives.
    await openDelete();
    await modal.getByRole("button", { name: "Cancel", exact: true }).click();
    assert.equal(writes.length, 0);
    await openDelete();
    await modal.getByRole("button", { name: "Delete", exact: true }).click();
    await deleteEntered;
    assert.equal(await modal.getByRole("button", { name: "Cancel", exact: true }).isDisabled(), true);
    assert.equal(await page.getByRole("button", { name: "Dismiss notification" }).count(), 0);
    releaseDelete();
    await heading.waitFor({ state: "detached" });
    await expectToast("Failed to delete connection", "error");
    assert.equal(await page.getByText("Deletion fixture", { exact: true }).count(), 1);
    assert.equal(state.reads, 1, "failed deletion must not refetch or discard connections");

    // A transport failure has the same feedback and leaves the account intact.
    const abortDelete = (route) => route.request().method() === "DELETE" ? route.abort("failed") : route.continue();
    await page.route("**/api/providers/fixture-github", abortDelete);
    await openDelete();
    await modal.getByRole("button", { name: "Delete", exact: true }).click();
    await heading.waitFor({ state: "detached" });
    await expectToast("Failed to delete connection", "error");
    assert.equal(await page.getByText("Deletion fixture", { exact: true }).count(), 1);
    assert.equal(state.reads, 1);
    await page.unroute("**/api/providers/fixture-github", abortDelete);

    // A successful retry reconciles the list and reports success, including
    // deletion of the last eligible account (the quota page's empty branch).
    failDelete = false;
    await openDelete();
    await modal.getByRole("button", { name: "Delete", exact: true }).click();
    await page.getByRole("heading", { name: "No Providers Connected", exact: true }).waitFor();
    await expectToast("Connection deleted", "check_circle");
    assert.equal(await page.getByText("Deletion fixture", { exact: true }).count(), 0);
    assert.equal(state.reads, 2, "successful deletion reconciles the connection list once");
    assert.deepEqual(writes, Array.from({ length: 3 }, () => ({ method: "DELETE", path: "/api/providers/fixture-github", body: null })), "each confirmed attempt sends exactly one DELETE and no other write");
    assert.deepEqual(state.unexpected, []);
    assert.deepEqual(errors, []);
    console.log(`quota deletion feedback: PASS (${astro ? "Astro reference" : "Dioxus"}; HTTP failure, transport failure, success)`);
  } finally { releaseDelete(); await browser.close(); }
}

main().catch((error) => { console.error(error); process.exitCode = 1; }).finally(() => server.close());
