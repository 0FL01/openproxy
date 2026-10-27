// Synthetic same-origin fixture only. No OpenProxy instance or external services.
// Run after integrating the route and building into an isolated output directory:
// PROXY_POOLS_PUBLIC_DIR=/path/to/public CHROME_BIN=/path/to/chromium node tests/proxy-pools-smoke.cjs
// A sandbox-capable Chromium is required; the sandbox is never disabled.
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(process.env.PROXY_POOLS_PUBLIC_DIR || path.join(__dirname, "../target/dx/openproxy-dashboard/release/web/public"));
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
const pool = (id, name, extra = {}) => ({ id, name, proxyUrl: `http://${id}.example:8080/`, noProxy: "", isActive: true, strictProxy: false, testStatus: "unknown", lastTestedAt: null, lastError: null, boundConnectionCount: 0, type: "http", ...extra });
async function waitFor(predicate, message) {
  const deadline = Date.now() + 10000;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error(`Timed out: ${message}`);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
let state;
function reset() {
  state = {
    status: { requireLogin: true, authenticated: true },
    pools: [pool("one", "Proxy One", { boundConnectionCount: 2 }), pool("two", "Proxy Two", { isActive: false })],
    requests: [], counter: 0, failPut: false, failDeploy: false,
    dead: new Set(), inFlight: 0, maxInFlight: 0, started: 0,
    healthHold: null, healthRelease: null,
  };
}
reset();

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  if (url.pathname.startsWith("/api/")) {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    let body = null;
    try { body = raw ? JSON.parse(raw) : null; } catch { return res.writeHead(400).end(); }
    state.requests.push({ method: req.method, path: url.pathname, query: url.search, body });
    const respond = (status, data) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(data)); };
    if (url.pathname === "/api/auth/status") return respond(200, state.status);
    if (url.pathname === "/api/build") return respond(200, {});
    if (url.pathname === "/api/settings") return respond(200, { enableTranslator: false });
    if (url.pathname === "/api/proxy-pools/vercel-deploy" && req.method === "POST") {
      if (state.failDeploy) return respond(422, { error: "Synthetic deploy failure" });
      state.pools.push(pool("relay", body.projectName || "generated", { type: "vercel", proxyUrl: "https://relay.example/" }));
      return respond(201, { deployUrl: "https://relay.example/" });
    }
    if (url.pathname === "/api/proxy-pools" && req.method === "GET") return respond(200, { proxyPools: state.pools });
    if (url.pathname === "/api/proxy-pools" && req.method === "POST") {
      const item = pool(`new-${++state.counter}`, body.name, body);
      state.pools.push(item);
      return respond(201, { proxyPool: item });
    }
    const match = url.pathname.match(/^\/api\/proxy-pools\/([^/]+)(\/test)?$/);
    if (match) {
      const id = decodeURIComponent(match[1]);
      const item = state.pools.find((entry) => entry.id === id);
      if (!item) return respond(404, { error: "Missing fixture" });
      if (req.method === "POST" && match[2]) {
        state.inFlight++;
        state.started++;
        state.maxInFlight = Math.max(state.maxInFlight, state.inFlight);
        if (state.healthHold) await state.healthHold;
        await new Promise((resolve) => setTimeout(resolve, 20));
        const ok = !state.dead.has(id);
        item.testStatus = ok ? "active" : "error";
        item.lastTestedAt = "2024-01-02T12:00:00Z";
        state.inFlight--;
        return respond(200, { ok });
      }
      if (req.method === "PUT" && !match[2]) {
        if (state.failPut) { const status = state.failPut; state.failPut = false; return respond(status, { error: "Synthetic update failure" }); }
        Object.assign(item, body);
        return respond(200, { proxyPool: item });
      }
      if (req.method === "DELETE" && !match[2]) {
        if (item.boundConnectionCount) return respond(409, { error: "In use", boundConnectionCount: item.boundConnectionCount });
        state.pools = state.pools.filter((entry) => entry.id !== id);
        return respond(200, { success: true });
      }
    }
    return respond(404, { error: "Unexpected fixture route" });
  }
  const relative = ["/", "/login", "/dashboard", "/dashboard/proxy-pools"].includes(url.pathname) ? "index.html" : url.pathname.slice(1);
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
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    // This also blocks the reference stylesheet's external font imports.
    await page.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    const go = async () => { await page.goto(`${origin}/dashboard/proxy-pools`); await page.getByRole("heading", { name: "Proxy Pools", exact: true }).last().waitFor(); };
    const requests = (method, url) => state.requests.filter((request) => request.method === method && request.path === url);
    const dialog = (title) => page.getByRole("dialog", { name: title });
    const clickConfirm = async (title) => { await dialog(title).getByRole("button", { name: "Confirm" }).click(); };

    state.pools = [];
    await go();
    await page.getByText("No proxy pool entries yet").waitFor();
    assert.equal(await page.getByRole("checkbox", { name: "Select all" }).count(), 0);
    await page.getByRole("button", { name: /Add Proxy Pool/ }).last().click();
    assert.equal(await dialog("Add Proxy Pool").getByRole("button", { name: "Save" }).isDisabled(), true);
    await page.keyboard.press("Escape");
    assert.equal(await dialog("Add Proxy Pool").count(), 0);
    reset();
    await go();
    await page.getByText("Proxy One", { exact: true }).waitFor();
    assert.equal(requests("GET", "/api/proxy-pools")[0].query, "?includeUsage=true");
    assert.equal(await page.getByText("Total: 2", { exact: true }).count(), 1);
    assert.equal(await page.getByText("Active: 1", { exact: true }).count(), 1);
    assert.equal(await page.getByText("2 bound", { exact: true }).count(), 1);
    assert.equal(await page.getByText("Last tested: Never", { exact: true }).count(), 2);

    // New and edited pools use trimmed fields and the exact CRUD payloads.
    await page.getByRole("button", { name: /Add Proxy Pool/ }).click();
    await page.getByLabel("Name", { exact: true }).fill("  New Pool  ");
    await page.getByLabel("Proxy URL").fill("  http://new.example:8080  ");
    await page.getByLabel("No Proxy").fill("  localhost,.internal  ");
    await dialog("Add Proxy Pool").getByRole("switch", { name: "Strict Proxy" }).click();
    await dialog("Add Proxy Pool").getByRole("button", { name: "Save" }).click();
    await page.getByText("Proxy pool created", { exact: true }).waitFor();
    assert.deepEqual(requests("POST", "/api/proxy-pools").at(-1).body, { name: "New Pool", proxyUrl: "http://new.example:8080", noProxy: "localhost,.internal", isActive: true, strictProxy: true });
    await page.getByRole("button", { name: "Edit New Pool" }).click();
    assert.equal(await page.getByLabel("Proxy URL").inputValue(), "http://new.example:8080");
    assert.equal(await dialog("Edit Proxy Pool").getByRole("switch", { name: "Strict Proxy" }).getAttribute("aria-checked"), "true");
    await page.getByLabel("Name", { exact: true }).fill(" Updated Pool ");
    await dialog("Edit Proxy Pool").getByRole("button", { name: "Save" }).click();
    await page.getByText("Proxy pool updated", { exact: true }).waitFor();
    assert.deepEqual(requests("PUT", "/api/proxy-pools/new-1").at(-1).body, { name: "Updated Pool", proxyUrl: "http://new.example:8080", noProxy: "localhost,.internal", isActive: true, strictProxy: true });

    // Optimistic toggle rolls back on a failed update, then persists on success.
    state.failPut = 500;
    await page.getByRole("switch", { name: "Disable Proxy One" }).click();
    await page.getByText("Failed to update active state").waitFor();
    assert.equal(await page.getByRole("switch", { name: "Disable Proxy One" }).getAttribute("aria-checked"), "true");
    await page.getByRole("switch", { name: "Disable Proxy One" }).click();
    await page.getByRole("switch", { name: "Enable Proxy One" }).waitFor();
    await waitFor(() => requests("PUT", "/api/proxy-pools/one").length === 2, "successful toggle request");
    assert.deepEqual(requests("PUT", "/api/proxy-pools/one").map((r) => r.body), [{ isActive: false }, { isActive: false }]);
    state.dead.add("two");
    await page.getByRole("button", { name: "Test proxy Proxy Two" }).click();
    await page.getByText("Proxy test failed", { exact: true }).waitFor();
    assert.equal(requests("POST", "/api/proxy-pools/two/test").length, 1);
    assert.equal(await page.getByText("Proxy Two", { exact: true }).locator("..").getByText("error", { exact: true }).count(), 1);

    // Deletion always confirms; a bound pool is retained and reports its count.
    await page.getByRole("button", { name: "Delete Proxy One" }).click();
    await page.keyboard.press("Escape");
    assert.equal(await dialog("Delete Proxy Pool").count(), 0);
    assert.equal(requests("DELETE", "/api/proxy-pools/one").length, 0);
    await page.getByRole("button", { name: "Delete Proxy One" }).click();
    await clickConfirm("Delete Proxy Pool");
    await page.getByText("Cannot delete: 2 connection(s) are still using this pool.").waitFor();
    assert.equal(await page.getByText("Proxy One", { exact: true }).count(), 1);
    await page.getByRole("button", { name: "Delete Proxy Two" }).click();
    await clickConfirm("Delete Proxy Pool");
    await page.getByText("Proxy Two", { exact: true }).waitFor({ state: "detached" });

    // Invalid import never sends credentials or partial writes; supported forms normalize and deduplicate.
    await page.getByRole("button", { name: "Batch Import" }).click();
    const batch = page.getByLabel("Paste Proxy List (One per line)");
    await batch.fill("http://fixture:secret@host.example:8080\nnot-supported");
    const beforeImport = requests("POST", "/api/proxy-pools").length;
    await dialog("Batch Import Proxies").getByRole("button", { name: "Import", exact: true }).click();
    await page.getByText("Invalid proxy format:", { exact: false }).waitFor();
    assert.equal(requests("POST", "/api/proxy-pools").length, beforeImport);
    assert.equal((await page.locator('[class*="z-[80]"]').innerText()).includes("secret"), false);
    await batch.fill("http://user:pass@a.example:8080\na.example:8080:user:pass\nb.example:9000:u@x:p#y");
    await dialog("Batch Import Proxies").getByRole("button", { name: "Import", exact: true }).click();
    await page.getByText("Batch import completed: Created 2, Skipped 1, Failed 0").waitFor();
    assert.deepEqual(requests("POST", "/api/proxy-pools").slice(beforeImport).map((r) => r.body), [
      { name: "Imported a.example:8080", proxyUrl: "http://user:pass@a.example:8080/", noProxy: "", isActive: true },
      { name: "Imported b.example:9000", proxyUrl: "http://u%40x:p%23y@b.example:9000/", noProxy: "", isActive: true },
    ]);

    // Bulk updates target selected pools only; bulk deletion reports blocked entries.
    await page.getByRole("checkbox", { name: "Select Proxy One" }).check();
    await page.getByRole("checkbox", { name: "Select Updated Pool" }).check();
    await page.getByRole("button", { name: "toggle_on Activate", exact: true }).click();
    await page.getByText("Activated 2", { exact: true }).waitFor();
    assert.deepEqual(requests("PUT", "/api/proxy-pools/one").at(-1).body, { isActive: true });
    assert.deepEqual(requests("PUT", "/api/proxy-pools/new-1").at(-1).body, { isActive: true });
    await page.getByRole("button", { name: "Delete", exact: false }).filter({ hasText: "Delete" }).first().click();
    await clickConfirm("Delete Proxy Pools");
    await page.getByText("Deleted 1, 1 bound", { exact: true }).waitFor();
    assert.equal(await page.getByText("Updated Pool", { exact: true }).count(), 0);
    assert.equal(await page.getByText("Proxy One", { exact: true }).count(), 1);

    // Ten-at-a-time health checks, visible progress, and explicit dead-pool confirmation.
    reset();
    state.pools = Array.from({ length: 12 }, (_, i) => pool(`health-${i}`, `Health ${i}`));
    state.dead = new Set(["health-1", "health-3"]);
    state.healthHold = new Promise((resolve) => { state.healthRelease = resolve; });
    let dispatched = 0;
    const countHealthRequest = (request) => { if (request.method() === "POST" && /\/api\/proxy-pools\/health-\d+\/test$/.test(request.url())) dispatched++; };
    page.on("request", countHealthRequest);
    await go();
    await page.getByRole("checkbox", { name: "Select all" }).check();
    await page.getByRole("button", { name: "Health Check", exact: false }).click();
    await page.waitForFunction(() => document.body.innerText.includes("Checking 0/12"));
    await waitFor(() => dispatched === 10, "ten dispatched health checks");
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.equal(dispatched, 10, "the remaining checks wait for a worker");
    assert.ok(state.maxInFlight > 1 && state.maxInFlight <= 10, "browser connections remain bounded");
    state.healthRelease();
    await dialog("Disable Dead Proxies").waitFor();
    assert.match(await dialog("Disable Dead Proxies").innerText(), /Alive: 10, Dead: 2/);
    assert.equal(state.requests.filter((r) => r.method === "PUT").length, 0);
    await clickConfirm("Disable Dead Proxies");
    await page.getByText("Disabled 2 dead proxies", { exact: true }).waitFor();
    assert.equal(dispatched, 12);
    page.off("request", countHealthRequest);
    assert.deepEqual(state.requests.filter((r) => r.method === "PUT").map((r) => [r.path, r.body]).sort((a, b) => a[0].localeCompare(b[0])), [["/api/proxy-pools/health-1", { isActive: false }], ["/api/proxy-pools/health-3", { isActive: false }]]);

    // Relay submission sends the one-shot token only to the local synthetic API.
    await page.getByRole("button", { name: /Deploy Relay/ }).click();
    await page.getByRole("button", { name: /Vercel Relay/ }).click();
    assert.equal(await page.getByLabel("Vercel API Token").getAttribute("type"), "password");
    assert.equal(await page.getByLabel("Project Name").inputValue(), "vercel-relay");
    await page.getByLabel("Vercel API Token").fill("fixture-token");
    await page.getByLabel("Project Name").fill("fixture-project");
    state.failDeploy = true;
    await dialog("Deploy Vercel Relay").getByRole("button", { name: "Deploy", exact: true }).click();
    await page.getByText("Synthetic deploy failure").waitFor();
    assert.equal(await page.getByLabel("Vercel API Token").inputValue(), "fixture-token");
    state.failDeploy = false;
    await dialog("Deploy Vercel Relay").getByRole("button", { name: "Deploy", exact: true }).click();
    await page.getByText("Deployed: https://relay.example/").waitFor();
    assert.deepEqual(requests("POST", "/api/proxy-pools/vercel-deploy").at(-1).body, { vercelToken: "fixture-token", projectName: "fixture-project" });
    await page.getByRole("button", { name: /Deploy Relay/ }).click();
    await page.getByRole("button", { name: /Vercel Relay/ }).click();
    assert.equal(await page.getByLabel("Vercel API Token").inputValue(), "");
    await dialog("Deploy Vercel Relay").getByRole("button", { name: "Cancel" }).click();

    // A 401 uses the shared recheck rather than treating every API 401 as logout.
    const checksBefore = requests("GET", "/api/auth/status").length;
    state.failPut = 401;
    await page.getByRole("switch", { name: "Disable Health 0" }).click();
    await waitFor(() => requests("GET", "/api/auth/status").length > checksBefore, "401 session recheck");
    assert.equal(requests("GET", "/api/auth/status").length, checksBefore + 1);
    assert.equal(new URL(page.url()).pathname, "/dashboard/proxy-pools");
    await page.getByRole("switch", { name: "Disable Health 0" }).waitFor();
    await page.waitForTimeout(200); // Allow the first shared recheck to settle.
    state.failPut = 401;
    state.status.authenticated = false;
    await page.getByRole("switch", { name: "Disable Health 0" }).click();
    await page.waitForURL(`${origin}/login`);
    assert.deepEqual(errors, []);
    console.log("proxy pools synthetic smoke passed");
  } finally {
    state.healthRelease?.();
    await browser.close();
    await new Promise((resolve) => server.close(resolve));
  }
}
main().catch((error) => { console.error(error); server.close(); process.exitCode = 1; });
