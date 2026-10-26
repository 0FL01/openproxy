// Synthetic same-origin fixture only; no OpenProxy instance or provider traffic.
// Run with full sandbox-capable Chrome after the documented release build.
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(__dirname, "../target/dx/openproxy-dashboard/release/web/public");
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };
const state = { requests: [], status: { requireLogin: true, authenticated: true }, mode: "success" };
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  if (url.pathname.startsWith("/api/")) {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : null;
    state.requests.push({ path: url.pathname, method: req.method, body });
    const respond = (status, data) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(data)); };
    if (url.pathname === "/api/auth/status") return respond(state.statusCode || 200, state.status);
    if (url.pathname === "/api/build") return respond(200, {});
    if (url.pathname === "/api/settings") return respond(200, { enableTranslator: false });
    if (url.pathname === "/api/providers" && req.method === "GET") return respond(200, { connections: [] });
    if (url.pathname === "/api/provider-nodes") return respond(200, { nodes: [] });
    if (url.pathname === "/api/proxy-pools") return respond(200, { proxyPools: [] });
    if (url.pathname === "/api/providers" && req.method === "POST") {
      if (state.mode === "held") { state.entered(); await state.hold; }
      if (state.mode === "error") return respond(422, { error: "Synthetic save error" });
      if (state.mode === "invalid-error") { res.writeHead(500, { "content-type": "application/json" }); return res.end("not-json"); }
      if (state.mode === "unauthorized") return respond(401, { error: "Synthetic expired session" });
      return respond(201, {});
    }
    if (url.pathname === "/api/oauth/codex/start") { res.writeHead(200, { "content-type": "text/plain" }); return res.end("synthetic OAuth destination"); }
    return respond(404, {});
  }
  if (["/dashboard/providers/codex", "/dashboard/providers/opencode-zen", "/dashboard/providers/openai"].includes(url.pathname)) {
    res.writeHead(200, { "content-type": "text/plain" });
    return res.end("synthetic detail destination");
  }
  const relative = ["/", "/login", "/dashboard", "/dashboard/providers", "/dashboard/providers/new"].includes(url.pathname) ? "index.html" : url.pathname.slice(1);
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
    const go = async () => { await page.goto(`${origin}/dashboard/providers/new`); await page.getByRole("heading", { name: "Add New Provider" }).waitFor(); };
    state.statusCode = 503;
    await page.goto(`${origin}/dashboard/providers/new?keep=a%26b#bookmark`);
    await page.getByRole("heading", { name: "Unable to verify session" }).waitFor();
    assert.equal(new URL(page.url()).search, "?keep=a%26b");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    state.statusCode = 200;
    await page.getByRole("button", { name: "Retry" }).click();
    await page.getByRole("heading", { name: "Add New Provider" }).waitFor();
    assert.equal(new URL(page.url()).search, "?keep=a%26b");
    assert.equal(new URL(page.url()).hash, "#bookmark");
    const select = page.getByRole("combobox");
    const search = page.getByPlaceholder("Search providers...");
    assert.equal(await select.inputValue(), "");
    assert.equal(await page.getByRole("button", { name: "Connect Provider" }).isDisabled(), true);
    assert.equal(await page.getByRole("heading", { name: "Providers", exact: true }).count(), 1);
    assert.equal(await search.count(), 1); // New page does not register the separate header search.

    const registry = JSON.parse(await fs.readFile(path.resolve(__dirname, "../src/browser/provider_data.json"), "utf8"));
    const groups = [["oauth", "oauth", "OAuth"], ["free", "free", "Free"], ["freeTier", "freeTier", "Free Tier (API Key)"], ["apikey", "apiKey", "API Key"]];
    const expected = [{ value: "", text: "Choose a provider..." }];
    for (const [group, source, label] of groups) {
      const sorted = [...registry[source]].sort((a, b) => (a.priority ?? 999) - (b.priority ?? 999) || Number(!!b.noAuth) - Number(!!a.noAuth) || a.name.localeCompare(b.name));
      expected.push(...sorted.map(provider => ({ value: `${group}:${provider.id}`, text: `[${label}] ${provider.name}` })));
    }
    expected.push({ value: "compatible:openai-compatible", text: "[Compatible Endpoint] OpenAI Compatible (custom endpoint)" }, { value: "compatible:anthropic-compatible", text: "[Compatible Endpoint] Anthropic Compatible (custom endpoint)" });
    assert.deepEqual(await select.locator("option").evaluateAll(options => options.map(option => ({ value: option.value, text: option.textContent }))), expected);
    assert.equal(expected.length, 56); // placeholder + all 55 real options

    await search.fill("\uFEFFcOdEx\uFEFF");
    await page.getByText("1 of 55 providers").waitFor();
    assert.deepEqual(await page.getByText("1 of 55 providers").evaluate(element => [...element.childNodes].map(node => node.textContent)), ["1", " of ", "55", " providers"]);
    assert.deepEqual(await select.locator("option").evaluateAll(options => options.map(option => option.value)), ["", "oauth:codex"]);
    await select.selectOption("oauth:codex");
    await search.fill("no-such-provider");
    await page.getByText("0 of 55 providers").waitFor();
    assert.equal(await select.inputValue(), "oauth:codex");
    assert.deepEqual(await select.locator("option").evaluateAll(options => options.map(option => option.value)), ["", "oauth:codex"]);
    await page.getByText("Connect with your subscription account via browser OAuth.").waitFor();
    await page.getByText("You will be redirected to complete OAuth for OpenAI Codex.").waitFor();
    await page.getByRole("button", { name: "Start OAuth" }).click();
    await page.waitForURL(`${origin}/api/oauth/codex/start`);
    assert.ok(state.requests.some(request => request.path === "/api/oauth/codex/start"));

    await go();
    await select.selectOption("free:opencode-zen");
    await page.getByText("Free-tier OAuth / no-auth providers. · No auth required").waitFor();
    await page.getByRole("button", { name: "Open Provider" }).click();
    await page.waitForURL(`${origin}/dashboard/providers/opencode-zen`);

    await go();
    await select.selectOption("freeTier:opencode-zen");
    await page.getByLabel("API Key", { exact: true }).fill("fixture-ignored-for-no-auth");
    const postsBeforeNoAuth = state.requests.filter(request => request.method === "POST").length;
    await page.getByRole("button", { name: "Open Provider" }).click();
    await page.waitForURL(`${origin}/dashboard/providers/opencode-zen`);
    assert.equal(state.requests.filter(request => request.method === "POST").length, postsBeforeNoAuth);

    for (const [option, title] of [["compatible:openai-compatible", "Add OpenAI Compatible"], ["compatible:anthropic-compatible", "Add Anthropic Compatible"]]) {
      await go();
      await select.selectOption(option);
      await page.getByRole("button", { name: "Open Compatible Setup" }).click();
      await page.getByRole("dialog", { name: title }).waitFor();
      assert.equal(new URL(page.url()).pathname, "/dashboard/providers");
    }

    // A blank key goes to the existing list modal, not a provider POST.
    await go();
    // ProviderIcon intentionally keeps its fallback after a failed image when
    // another provider is selected, just like the unkeyed reference component.
    await select.selectOption("apikey:a6api");
    await page.getByText("A6", { exact: true }).waitFor();
    await select.selectOption("apikey:openai");
    await page.getByText("OA", { exact: true }).waitFor();
    assert.equal(await page.getByRole("link", { name: "Get API key" }).getAttribute("href"), "https://platform.openai.com/api-keys");
    await page.getByLabel("API Key", { exact: true }).fill(" \uFEFF ");
    await page.getByRole("button", { name: "Add Key on List" }).click();
    await page.getByRole("dialog", { name: "Add OpenAI API Key" }).waitFor();
    assert.equal(state.requests.filter(request => request.method === "POST").length, postsBeforeNoAuth);

    // Changing provider clears both entered fields while retaining the filter.
    await go();
    await select.selectOption("apikey:openai");
    await page.getByLabel("Display Name").fill("Fixture name");
    await page.getByLabel("API Key", { exact: true }).fill("fixture-value");
    await page.getByRole("button", { name: "Save API Key" }).waitFor();
    await search.fill("\uFEFFdeepseek\uFEFF");
    assert.equal(await select.inputValue(), "apikey:openai");
    await select.selectOption("apikey:deepseek");
    assert.equal(await search.inputValue(), "\uFEFFdeepseek\uFEFF");
    assert.equal(await page.getByLabel("Display Name").inputValue(), "");
    assert.equal(await page.getByLabel("API Key", { exact: true }).inputValue(), "");

    // Failed saves preserve the form, show the backend message, and do not redirect.
    await search.fill("");
    await select.selectOption("apikey:openai");
    await page.getByLabel("Display Name").fill(" \uFEFFFixture name\uFEFF ");
    await page.getByLabel("API Key", { exact: true }).fill(" \uFEFFfixture-value\uFEFF ");
    state.mode = "error";
    await page.getByRole("button", { name: "Save API Key" }).click();
    await page.getByText("Synthetic save error").waitFor();
    assert.equal(new URL(page.url()).pathname, "/dashboard/providers/new");
    assert.equal(await page.getByLabel("API Key", { exact: true }).inputValue(), " \uFEFFfixture-value\uFEFF ");
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers" && request.method === "POST").at(-1).body, { provider: "openai", name: "Fixture name", apiKey: "fixture-value", priority: 1, testStatus: "unknown" });
    state.mode = "invalid-error";
    await page.getByRole("button", { name: "Save API Key" }).click();
    await page.getByText("Failed to add provider key").waitFor();
    await page.getByRole("button", { name: "Dismiss notification" }).first().click();
    await page.getByRole("button", { name: "Dismiss notification" }).first().click();
    const abortSave = route => route.request().method() === "POST" ? route.abort("failed") : route.continue();
    await page.route("**/api/providers", abortSave);
    await page.getByRole("button", { name: "Save API Key" }).click();
    await page.getByText("Failed to add provider key").waitFor();
    await page.unroute("**/api/providers", abortSave);
    assert.equal(new URL(page.url()).pathname, "/dashboard/providers/new");
    assert.equal(await page.getByLabel("API Key", { exact: true }).inputValue(), " \uFEFFfixture-value\uFEFF ");
    assert.equal(state.requests.some(request => request.path === "/api/providers/validate"), false);

    // Pending save is disabled and reports Saving; an empty name defaults to API Key.
    await page.getByLabel("Display Name").fill("  ");
    let release;
    let enter;
    state.hold = new Promise(resolve => { release = resolve; });
    const entered = new Promise(resolve => { enter = resolve; });
    state.entered = enter;
    state.mode = "held";
    await page.getByRole("button", { name: "Save API Key" }).click();
    await entered;
    assert.equal(await page.getByRole("button", { name: /Saving/ }).isDisabled(), true);
    assert.equal(await page.getByText("progress_activity", { exact: true }).count(), 1);
    release();
    await page.waitForURL(`${origin}/dashboard/providers/openai`);
    assert.deepEqual(state.requests.filter(request => request.path === "/api/providers" && request.method === "POST").at(-1).body, { provider: "openai", name: "API Key", apiKey: "fixture-value", priority: 1, testStatus: "unknown" });

    // A same-origin 401 follows the existing shared session-recheck contract.
    state.mode = "success";
    await go();
    await select.selectOption("apikey:openai");
    await page.getByLabel("API Key", { exact: true }).fill("fixture-expired");
    state.mode = "unauthorized";
    state.status.authenticated = false;
    const redirect = page.waitForURL(`${origin}/login`);
    await page.getByRole("button", { name: "Save API Key" }).click();
    await redirect;

    state.status.authenticated = true;
    state.mode = "success";
    await page.context().addCookies([{ name: "locale", value: "ru", url: origin }]);
    const catalog = page.waitForResponse(response => response.url().endsWith("/i18n/literals/ru.json"));
    await go();
    assert.equal((await catalog).status(), 200);
    await page.getByRole("heading", { name: "Провайдеры", exact: true }).waitFor();
    await select.selectOption("apikey:openai");
    await page.getByText("Ключ API", { exact: true }).waitFor();
    await select.selectOption("oauth:codex");
    await select.selectOption("apikey:openai");
    await page.getByText("Ключ API", { exact: true }).waitFor();
    await page.setViewportSize({ width: 375, height: 812 });
    assert.equal(await page.getByRole("button", { name: "Open navigation" }).isVisible(), true);
    assert.equal(await page.getByRole("button", { name: "Add Key on List" }).evaluate(element => element.classList.contains("flex-1") && element.classList.contains("w-full")), true);
    assert.deepEqual(errors, []);
    console.log("new provider synthetic smoke passed");
  } finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
}
main().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
