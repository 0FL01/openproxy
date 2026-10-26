// Synthetic same-origin provider list fixture. No OpenProxy instance or provider traffic.
// Run with full sandbox-capable Chrome: CHROME_BIN=/path/to/chrome node tests/providers-smoke.cjs
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(__dirname, "../target/dx/openproxy-dashboard/release/web/public");
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
const state = {
  requests: [],
  status: { requireLogin: true, authenticated: true },
  connections: [
    { id: "oauth-one", provider: "codex", authType: "oauth", isActive: true, testStatus: "active" },
    { id: "oauth-two", provider: "codex", authType: "oauth", isActive: true, testStatus: "error", lastErrorType: "upstream_auth_error" },
    { id: "key-one", provider: "openai", authType: "apikey", isActive: false, testStatus: "unknown" },
  ],
  nodes: [
    { id: "openai-compatible-fixture", name: "Fixture Compatible", type: "openai-compatible", apiType: "responses" },
    { id: "anthropic-compatible-fixture", name: "Fixture Messages", type: "anthropic-compatible" },
  ],
};
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  if (url.pathname.startsWith("/api/")) {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : null;
    state.requests.push({ path: url.pathname, query: url.search, method: req.method, body });
    const respond = (status, data) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(data)); };
    if (url.pathname === "/api/auth/status") return respond(state.statusCode || 200, state.status);
    if (url.pathname === "/api/build") return respond(200, {});
    if (url.pathname === "/api/settings") return respond(200, { enableTranslator: false });
    if (url.pathname === "/api/providers" && req.method === "GET") return respond(200, state.malformedProviders ? {} : { connections: state.connections });
    if (url.pathname === "/api/providers/validate" && req.method === "POST") return respond(200, { valid: body.apiKey !== "fixture-invalid" });
    if (url.pathname === "/api/providers" && req.method === "POST") {
      const account = { id: `new-${state.connections.length}`, authType: "apikey", isActive: true, ...body };
      state.connections.push(account);
      return respond(201, { connection: account });
    }
    if (url.pathname.startsWith("/api/providers/") && req.method === "PUT") {
      if (state.denyPuts) return respond(401, { error: "synthetic expired session" });
      const account = state.connections.find(item => item.id === decodeURIComponent(url.pathname.split("/").pop()));
      if (account) Object.assign(account, body);
      return respond(account ? 200 : 404, {});
    }
    if (url.pathname === "/api/provider-nodes" && req.method === "GET") return respond(200, { nodes: state.nodes });
    if (url.pathname === "/api/proxy-pools") return respond(200, { proxyPools: [{ id: "fixture-pool", name: "Fixture pool" }] });
    if (url.pathname === "/api/provider-nodes/validate") return respond(200, { valid: true, method: "chat" });
    if (url.pathname === "/api/provider-nodes" && req.method === "POST") {
      const node = { id: `${body.type}-created`, ...body };
      state.nodes.push(node);
      return respond(201, { node });
    }
    if (url.pathname === "/api/providers/test-batch") {
      if (body.mode === "free") return respond(200, { mode: "free", summary: { passed: 0, failed: 0, total: 0 }, results: [] });
      return respond(200, { mode: body.mode, summary: { passed: 1, failed: 0, total: 1 }, results: [{ connectionId: "oauth-one", connectionName: "Fixture account", provider: "codex", valid: true, latencyMs: 42 }] });
    }
    return respond(404, {});
  }
  if (url.pathname === "/dashboard/providers/codex") {
    res.writeHead(200, { "content-type": "text/plain" });
    return res.end("synthetic navigation destination");
  }
  const relative = ["/", "/login", "/dashboard", "/dashboard/endpoint", "/dashboard/providers"].includes(url.pathname) ? "index.html" : url.pathname.slice(1);
  const file = path.resolve(publicDir, relative);
  if (!file.startsWith(publicDir + path.sep)) return res.writeHead(403).end();
  try { const content = await fs.readFile(file); res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" }); res.end(content); }
  catch { res.writeHead(404).end(); }
});

async function main() {
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({ executablePath: process.env.CHROME_BIN || undefined, chromiumSandbox: true, headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    const errors = [];
    page.on("pageerror", error => errors.push(String(error)));
    await page.route("**/*", route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    await page.goto(`${origin}/dashboard/providers`);
    await page.getByRole("heading", { name: "OAuth Providers" }).waitFor();
    assert.equal(await page.getByRole("heading", { name: "Providers", exact: true }).count(), 1);
    for (const [endpoint, query] of [["/api/providers", ""], ["/api/provider-nodes", ""], ["/api/proxy-pools", "?isActive=true"]]) {
      assert.equal(state.requests.filter(request => request.path === endpoint && request.query === query && request.method === "GET").length, 1, endpoint);
    }
    const codex = page.locator('a[href="/dashboard/providers/codex"]');
    await codex.getByText("1 Connected").waitFor();
    await codex.getByText("1 Error (AUTH)").waitFor();
    assert.equal(await codex.locator("img").getAttribute("src"), "/providers/codex.png");
    const openai = page.locator('a[href="/dashboard/providers/openai"]');
    await openai.getByText("Disabled").waitFor();
    assert.equal(await page.locator('a[href="/dashboard/providers/openai-compatible-fixture"]').getByText("Responses").count(), 1);
    assert.equal(await page.locator('a[href="/dashboard/providers/anthropic-compatible-fixture"]').getByText("Messages", { exact: true }).count(), 1);

    // Provider switches change only matching authentication rows, never navigate.
    await codex.getByRole("switch", { name: "Disable provider" }).click();
    await codex.getByText("Disabled").waitFor();
    await page.waitForFunction(() => document.querySelector('a[href="/dashboard/providers/codex"] [role=switch][aria-checked=false]'));
    await codex.getByRole("switch", { name: "Enable provider" }).click();
    await codex.getByText("1 Connected").waitFor();
    await page.waitForTimeout(100);
    assert.deepEqual(state.requests.filter(request => request.method === "PUT").map(request => [request.path, request.body]), [
      ["/api/providers/oauth-one", { isActive: false }], ["/api/providers/oauth-two", { isActive: false }],
      ["/api/providers/oauth-one", { isActive: true }], ["/api/providers/oauth-two", { isActive: true }],
    ]);
    assert.equal(new URL(page.url()).pathname, "/dashboard/providers");

    const search = page.getByPlaceholder("Search providers...");
    await search.fill("  kimi  ");
    await page.getByRole("heading", { name: "Kimi", exact: true }).first().waitFor();
    assert.equal(await codex.count(), 0);
    await search.fill("\uFEFFkimi\uFEFF");
    await page.getByRole("heading", { name: "Kimi", exact: true }).first().waitFor();
    await search.fill("not-a-real-provider");
    await page.getByText("No providers match your search").waitFor();
    await page.getByRole("button", { name: "Clear search" }).click();
    const showAll = page.getByRole("button", { name: "Show all 35 providers" });
    assert.deepEqual(await showAll.evaluate(element => [...element.childNodes].filter(node => node.nodeType === Node.TEXT_NODE).map(node => node.textContent)), ["Show all ", "35", " providers"]);
    await showAll.click();
    await page.locator('a[href="/dashboard/providers/xiaomi-tokenplan"]').waitFor();
    // The reference hides the whole Free Tier section (including this checkbox) once selected.
    await page.getByLabel("Show only free tier").click();
    assert.equal(await page.locator('a[href="/dashboard/providers/codex"]').count(), 0);
    assert.ok(await page.locator('a[href="/dashboard/providers/kilocode"]').count() > 0);
    await page.reload();
    await page.getByRole("heading", { name: "OAuth Providers" }).waitFor();

    // Create and validate only against this synthetic same-origin fixture.
    await page.getByRole("button", { name: "Add OpenAI Compatible" }).click();
    const dialog = page.getByRole("dialog", { name: "Add OpenAI Compatible" });
    await dialog.getByLabel("Name", { exact: true }).fill("Synthetic Node");
    await dialog.getByLabel("Prefix", { exact: true }).fill("synthetic");
    await dialog.getByLabel("API Type", { exact: true }).selectOption("responses");
    await dialog.getByLabel("Base URL", { exact: true }).fill("https://fixture.invalid/v1");
    await dialog.getByLabel("API Key (for Check)").fill("fixture-only");
    await dialog.getByLabel("Model ID (optional)").fill(" model-fixture ");
    await dialog.getByRole("button", { name: "Check", exact: true }).click();
    await dialog.getByText("Valid", { exact: true }).waitFor();
    await dialog.getByRole("button", { name: "Create", exact: true }).click();
    await page.locator('a[href="/dashboard/providers/openai-compatible-created"]').getByText("Synthetic Node").waitFor();
    assert.deepEqual(state.requests.find(request => request.path === "/api/provider-nodes/validate").body, { baseUrl: "https://fixture.invalid/v1", apiKey: "fixture-only", type: "openai-compatible", modelId: "model-fixture" });
    assert.deepEqual(state.requests.find(request => request.path === "/api/provider-nodes" && request.method === "POST").body, { name: "Synthetic Node", prefix: "synthetic", apiType: "responses", baseUrl: "https://fixture.invalid/v1", type: "openai-compatible" });

    await page.getByRole("button", { name: "Test all OAuth connections" }).click();
    await page.getByText("Fixture account").waitFor();
    assert.deepEqual(state.requests.find(request => request.path === "/api/providers/test-batch").body, { mode: "oauth", providerId: null });
    await page.getByRole("button", { name: "Close test results" }).click();
    await page.getByRole("button", { name: "Test all Free provider connections" }).click();
    await page.getByText("Nothing to test").waitFor();
    assert.equal(await page.getByText("No free providers connected").count(), 1);
    await page.getByText("No active connections found for this group.").waitFor();
    await page.getByRole("button", { name: "Close test results" }).click();

    // Deep links consume only their own parameter, keep the others and never
    // make a live credential-validation request outside this fixture.
    await page.goto(`${origin}/dashboard/providers?compatible=anthropic&keep=a%26b#bookmark`);
    const anthropic = page.getByRole("dialog", { name: "Add Anthropic Compatible" });
    await anthropic.waitFor();
    assert.equal(new URL(page.url()).search, "?keep=a%26b");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    await anthropic.getByLabel("Name", { exact: true }).fill("Synthetic Messages");
    await anthropic.getByLabel("Prefix", { exact: true }).fill("synthetic-messages");
    await anthropic.getByRole("button", { name: "Create", exact: true }).click();
    await page.locator('a[href="/dashboard/providers/anthropic-compatible-created"]').getByText("Synthetic Messages").waitFor();
    assert.deepEqual(state.requests.filter(request => request.path === "/api/provider-nodes" && request.method === "POST").at(-1).body, { name: "Synthetic Messages", prefix: "synthetic-messages", baseUrl: "https://api.anthropic.com/v1", type: "anthropic-compatible" });

    await page.goto(`${origin}/dashboard/providers?add=openai&keep=1#bookmark`);
    const api = page.getByRole("dialog", { name: "Add OpenAI API Key" });
    await api.waitFor();
    assert.equal(new URL(page.url()).search, "?keep=1");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    assert.equal(await api.getByRole("button", { name: "Save", exact: true }).isDisabled(), true);
    await api.getByLabel("Name", { exact: true }).fill("Fixture account");
    await api.getByLabel("API Key", { exact: true }).fill("fixture-invalid");
    await api.getByLabel("Proxy Pool").selectOption("fixture-pool");
    await api.getByLabel("Priority").fill("2147483648");
    await api.getByRole("button", { name: "Save", exact: true }).click();
    await api.waitFor({ state: "detached" });
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers/validate").at(-1).body, { provider: "openai", apiKey: "fixture-invalid" });
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers" && request.method === "POST").at(-1).body, { provider: "openai", name: "Fixture account", apiKey: "fixture-invalid", priority: 2147483648, proxyPoolId: "fixture-pool", testStatus: "unknown" });
    await page.locator('a[href="/dashboard/providers/openai"]').getByText("2 Added").waitFor();

    await page.goto(`${origin}/dashboard/providers?add=azure`);
    const azure = page.getByRole("dialog", { name: "Add Azure OpenAI API Key" });
    await azure.waitFor();
    await azure.getByLabel("Name", { exact: true }).fill("Azure fixture");
    await azure.getByLabel("API Key", { exact: true }).fill("fixture-azure");
    assert.equal(await azure.getByRole("button", { name: "Save", exact: true }).isDisabled(), true);
    await azure.getByLabel("Azure Endpoint").fill("https://fixture.invalid");
    await azure.getByLabel("Deployment Name").fill("fake-deployment");
    await azure.getByLabel("Organization").fill("fake-org");
    await azure.getByRole("button", { name: "Save", exact: true }).click();
    await azure.waitFor({ state: "detached" });
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers" && request.method === "POST").at(-1).body, { provider: "azure", name: "Azure fixture", apiKey: "fixture-azure", priority: 1, proxyPoolId: null, testStatus: "active", providerSpecificData: { azureEndpoint: "https://fixture.invalid", apiVersion: "2024-10-01-preview", deployment: "fake-deployment", organization: "fake-org" } });

    await page.goto(`${origin}/dashboard/providers?add=cloudflare-ai`);
    const cloudflare = page.getByRole("dialog", { name: "Add Cloudflare API Key" });
    await cloudflare.waitFor();
    await cloudflare.getByLabel("Name", { exact: true }).fill("Cloudflare fixture");
    await cloudflare.getByLabel("API Key", { exact: true }).fill("fixture-cloudflare");
    assert.equal(await cloudflare.getByRole("button", { name: "Save", exact: true }).isDisabled(), true);
    await cloudflare.getByLabel("Account ID").fill("fixture-account-id");
    await cloudflare.getByRole("button", { name: "Save", exact: true }).click();
    await cloudflare.waitFor({ state: "detached" });
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers/validate").at(-1).body, { provider: "cloudflare-ai", apiKey: "fixture-cloudflare", providerSpecificData: { accountId: "fixture-account-id" } });

    await page.goto(`${origin}/dashboard/providers?add=xiaomi-tokenplan`);
    const xiaomi = page.getByRole("dialog", { name: "Add Xiaomi MiMo (Token Plan) API Key" });
    await xiaomi.waitFor();
    await xiaomi.getByLabel("Name", { exact: true }).fill("Xiaomi fixture");
    await xiaomi.getByLabel("API Key", { exact: true }).fill("fixture-xiaomi");
    await xiaomi.getByLabel("Region").selectOption("ams");
    await xiaomi.getByRole("button", { name: "Save", exact: true }).click();
    await xiaomi.waitFor({ state: "detached" });
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers/validate").at(-1).body, { provider: "xiaomi-tokenplan", apiKey: "fixture-xiaomi", providerSpecificData: { region: "ams" } });

    await page.goto(`${origin}/dashboard/providers?add=a6api`);
    const a6 = page.getByRole("dialog", { name: "Add A6API API Key" });
    await a6.waitFor();
    await a6.getByLabel("Name", { exact: true }).fill("A6 fixture");
    await a6.getByLabel("API Key", { exact: true }).fill("fixture-a6");
    const validationsBeforeA6 = state.requests.filter(request => request.path === "/api/providers/validate").length;
    await a6.getByRole("button", { name: "Save", exact: true }).click();
    await a6.waitFor({ state: "detached" });
    assert.equal(state.requests.filter(request => request.path === "/api/providers/validate").length, validationsBeforeA6);
    assert.equal(state.requests.filter(request => request.path === "/api/providers" && request.method === "POST").at(-1).body.testStatus, "active");

    await page.goto(`${origin}/dashboard/providers?add=deepseek`);
    const bulk = page.getByRole("dialog", { name: "Add DeepSeek API Key" });
    await bulk.waitFor();
    await bulk.getByRole("button", { name: "Bulk Add" }).click();
    await bulk.locator("textarea").fill("prod|fixture-bulk-a\nfixture-bulk-b");
    await bulk.getByRole("button", { name: "Add All Keys" }).click();
    await bulk.getByText("Added 2").waitFor();
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers" && request.method === "POST").slice(-2).map(request => request.body), [
      { provider: "deepseek", name: "prod 1", apiKey: "fixture-bulk-a", priority: 1, testStatus: "unknown" },
      { provider: "deepseek", name: "Key 2", apiKey: "fixture-bulk-b", priority: 1, testStatus: "unknown" },
    ]);
    await bulk.getByRole("button", { name: "Close", exact: true }).last().click();
    await page.locator('a[href="/dashboard/providers/deepseek"]').getByText("2 Added").waitFor();

    await page.goto(`${origin}/dashboard/providers?add=codex`);
    await page.waitForURL(`${origin}/dashboard/providers/codex`);

    // Failed session verification must not discard an unconsumed deep link.
    state.statusCode = 503;
    await page.goto(`${origin}/dashboard/providers?compatible=openai&keep=1#bookmark`);
    await page.getByText("Unable to verify session").waitFor();
    assert.equal(new URL(page.url()).search, "?compatible=openai&keep=1");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    state.statusCode = 200;
    await page.reload();
    const restored = page.getByRole("dialog", { name: "Add OpenAI Compatible" });
    await restored.waitFor();
    assert.equal(new URL(page.url()).search, "?keep=1");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    await restored.getByRole("button", { name: "Cancel" }).click();

    // Promise.all rejects immediately on a fetch failure even if another fetch
    // is still pending. That later 401 must still reach Dashboard's observer.
    let releaseNodes;
    let enteredNodes;
    const heldNodes = new Promise(resolve => { releaseNodes = resolve; });
    const nodesStarted = new Promise(resolve => { enteredNodes = resolve; });
    const failProviders = route => route.abort("failed");
    const delayedNodes = async route => {
      enteredNodes();
      await heldNodes;
      await route.fulfill({ status: 401, contentType: "application/json", body: "{}" }).catch(() => {});
    };
    await page.route("**/api/providers", failProviders);
    await page.route("**/api/provider-nodes", delayedNodes);
    try {
      await page.goto(`${origin}/dashboard/providers`);
      await nodesStarted;
      await page.getByRole("heading", { name: "OAuth Providers" }).waitFor({ timeout: 5000 });
      await page.locator('a[href="/dashboard/providers/codex"]').getByText("No connections").waitFor();
      assert.equal(await page.locator('a[href="/dashboard/providers/openai-compatible-fixture"]').count(), 0);
      const recheck = page.waitForResponse(response => response.url().endsWith("/api/auth/status"), { timeout: 5000 });
      releaseNodes();
      await recheck;
    } finally {
      releaseNodes();
      await page.unroute("**/api/providers", failProviders);
      await page.unroute("**/api/provider-nodes", delayedNodes);
    }

    // A required body parse error prevents applying the other valid responses.
    const badNodes = route => route.fulfill({ status: 200, contentType: "application/json", body: "not-json" });
    await page.route("**/api/provider-nodes", badNodes);
    try {
      await page.goto(`${origin}/dashboard/providers`);
      await page.getByRole("heading", { name: "OAuth Providers" }).waitFor();
      await page.locator('a[href="/dashboard/providers/codex"]').getByText("No connections").waitFor();
    } finally { await page.unroute("**/api/provider-nodes", badNodes); }

    // Pool JSON is optional in the reference; an invalid body keeps the other
    // valid data while presenting no pools in the add-key dialog.
    const badPools = route => route.fulfill({ status: 200, contentType: "application/json", body: "not-json" });
    await page.route("**/api/proxy-pools?*", badPools);
    try {
      await page.goto(`${origin}/dashboard/providers?add=openai`);
      const noPools = page.getByRole("dialog", { name: "Add OpenAI API Key" });
      await noPools.getByText("No active proxy pools available.", { exact: false }).waitFor();
      await page.locator('a[href="/dashboard/providers/codex"]').getByText("1 Connected").waitFor();
      await noPools.getByRole("button", { name: "Cancel" }).click();
    } finally { await page.unroute("**/api/proxy-pools?*", badPools); }

    const mutationsBeforeMalformed = state.requests.filter(request => request.method !== "GET").length;
    state.malformedProviders = true;
    await page.goto(`${origin}/dashboard/providers`);
    await page.getByRole("heading", { name: "OAuth Providers" }).waitFor();
    await page.locator('a[href="/dashboard/providers/codex"]').getByText("No connections").waitFor();
    assert.equal(state.requests.filter(request => request.method !== "GET").length, mutationsBeforeMalformed);
    state.malformedProviders = false;
    await page.goto(`${origin}/dashboard/providers`);
    await page.getByRole("heading", { name: "OAuth Providers" }).waitFor();
    await page.setViewportSize({ width: 375, height: 812 });
    assert.equal(await search.isVisible(), false);
    await page.getByRole("button", { name: "Open navigation" }).click();
    assert.equal(await page.getByRole("link", { name: /Providers/ }).count() > 0, true);
    await page.reload();
    await page.getByRole("heading", { name: "OAuth Providers" }).waitFor();
    state.denyPuts = true;
    state.status.authenticated = false;
    const redirect = page.waitForURL(`${origin}/login`);
    await page.locator('a[href="/dashboard/providers/codex"]').getByRole("switch", { name: "Disable provider" }).click();
    await redirect;
    assert.ok(state.requests.filter(request => request.path === "/api/auth/status").length >= 2);

    state.denyPuts = false;
    state.status.authenticated = true;
    await page.context().addCookies([{ name: "locale", value: "ru", url: origin }]);
    const catalog = page.waitForResponse(response => response.url().endsWith("/i18n/literals/ru.json"));
    await page.goto(`${origin}/dashboard/providers`);
    assert.equal((await catalog).status(), 200);
    await page.getByRole("heading", { name: "Провайдеры", exact: true }).waitFor();
    assert.deepEqual(errors, []);
    console.log("provider list synthetic smoke passed");
  } finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
}
main().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
