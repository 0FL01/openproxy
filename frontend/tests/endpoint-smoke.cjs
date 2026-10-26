// Isolated synthetic HTTP backend. Never connects to an OpenProxy instance.
// Run with a sandbox-capable Chromium; the test must not disable the sandbox.
// The site-data regression needs full Chrome: headless-shell may ignore the
// profile's cookie-blocking preference instead of throwing SecurityError.
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(__dirname, "../target/dx/openproxy-dashboard/release/web/public");
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml" };
let state;
function reset() {
  state = {
    status: { requireLogin: true, authenticated: true },
    settings: { requireApiKey: true, requireLogin: true, hasPassword: true, enableTranslator: true },
    keys: [{ id: "one", name: "Fixture key", key: "fixture_key_1234", createdAt: "2024-01-02T12:00:00Z", isActive: true }],
    requests: [],
  };
}
reset();

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  if (url.pathname === "/i18n/literals/de.json") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify({ "Endpoint": "Endpunkt" }));
  }
  if (url.pathname.startsWith("/api/")) {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : null;
    state.requests.push({ path: url.pathname, method: req.method, body });
    const respond = (status, data, headers = {}) => {
      res.writeHead(status, { "content-type": "application/json", ...headers });
      res.end(JSON.stringify(data));
    };
    if (url.pathname === "/api/auth/status") return respond(state.statusCode || 200, state.status);
    if (url.pathname === "/api/auth/logout") { state.status.authenticated = false; return respond(200, {}); }
    if (url.pathname === "/api/build") return respond(200, { commit: "abcdef0123456789abcdef0123456789abcdef01" });
    if (url.pathname === "/api/settings" && req.method === "GET") return respond(state.settingsCode || 200, state.settings);
    if (url.pathname === "/api/settings" && req.method === "PATCH") {
      if (state.settingsFailure) return respond(500, {});
      Object.assign(state.settings, body);
      return respond(200, state.settings);
    }
    if (url.pathname === "/api/keys" && req.method === "GET") {
      if (state.invalidKeysJson) { res.writeHead(200, { "content-type": "application/json" }); return res.end("not-json"); }
      return respond(state.keysCode || 200, { keys: state.keys });
    }
    if (url.pathname === "/api/keys" && req.method === "POST") {
      const key = { id: `id-${state.keys.length}`, name: body.name, key: "fixture_created_key", createdAt: "2024-01-02T12:00:00Z", isActive: true };
      state.keys.push(key);
      return respond(200, { key: key.key });
    }
    if (url.pathname.startsWith("/api/keys/") && req.method === "PUT") {
      const key = state.keys.find((item) => item.id === url.pathname.split("/").pop());
      if (key) Object.assign(key, body);
      return respond(key ? 200 : 404, {});
    }
    if (url.pathname.startsWith("/api/keys/") && req.method === "DELETE") {
      state.keys = state.keys.filter((key) => key.id !== url.pathname.split("/").pop());
      return respond(200, {});
    }
    if (url.pathname === "/api/locale") return respond(200, {}, { "set-cookie": `locale=${body.locale}; Path=/` });
    if (url.pathname === "/api/shutdown") return respond(200, {});
    return respond(404, {});
  }
  const relative = ["/", "/login", "/dashboard", "/dashboard/endpoint"].includes(url.pathname) ? "index.html" : url.pathname.slice(1);
  const file = path.resolve(publicDir, relative);
  if (!file.startsWith(publicDir + path.sep)) return res.writeHead(403).end();
  try {
    const body = await fs.readFile(file);
    res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" });
    res.end(body);
  } catch { res.writeHead(404).end(); }
});

async function main() {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({ executablePath: process.env.CHROME_BIN || undefined, chromiumSandbox: true, headless: true });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    const go = (url) => page.goto(`${origin}${url}`);

    // Failed and malformed session checks show retry, then gate all data requests.
    state.statusCode = 503;
    state.settings.requireLogin = false;
    state.settings.hasPassword = false;
    let releaseInitialStatus;
    let initialStatusEntered;
    const holdInitialStatus = new Promise((resolve) => { releaseInitialStatus = resolve; });
    const initialStatusStarted = new Promise((resolve) => { initialStatusEntered = resolve; });
    const initialStatusRoute = async (route) => {
      initialStatusEntered();
      await holdInitialStatus;
      await route.continue().catch(() => {});
    };
    await page.route("**/api/auth/status", initialStatusRoute);
    let skeletonElement;
    try {
      await go("/dashboard");
      await page.getByText("Checking session…").waitFor();
      await initialStatusStarted;
      const skeleton = page.locator("#dashboard-skeleton");
      await skeleton.waitFor({ state: "attached", timeout: 5000 });
      assert.equal(await skeleton.count(), 1);
      assert.deepEqual(await skeleton.evaluate((element) => ({
        parent: element.parentElement.id,
        hidden: element.getAttribute("aria-hidden"),
        children: [...element.children].map((child) => `${child.tagName}.${child.className}`),
        sidebar: [...element.querySelector(".ds-sidebar").children].map((child) => `${child.tagName}.${child.className}`),
        items: [...element.querySelector(".ds-sidebar-nav").children].map((child) => `${child.tagName}.${child.className}`),
        opacity: getComputedStyle(element).opacity,
        pointerEvents: getComputedStyle(element).pointerEvents,
      })), {
        parent: "main",
        hidden: "true",
        children: ["ASIDE.ds-sidebar", "MAIN.ds-main"],
        sidebar: ["DIV.ds-sidebar-head", "DIV.ds-sidebar-nav"],
        items: Array(7).fill("DIV.ds-sidebar-item"),
        opacity: "1",
        pointerEvents: "none",
      });
      skeletonElement = await skeleton.elementHandle();
    } finally {
      releaseInitialStatus();
      await page.unroute("**/api/auth/status", initialStatusRoute);
    }
    await page.getByText("Unable to verify session").waitFor();
    await page.waitForFunction(() => document.body.classList.contains("dashboard-ready") && getComputedStyle(document.querySelector("#dashboard-skeleton")).opacity === "0");
    assert.equal(await skeletonElement.evaluate((element) => element === document.querySelector("#dashboard-skeleton")), true);
    assert.equal(state.requests.some((r) => r.path === "/api/keys"), false);
    state.statusCode = 200;
    state.status = { requireLogin: "yes", authenticated: true };
    await page.getByRole("button", { name: "Retry" }).click();
    await page.getByText("Unable to verify session").waitFor();
    state.status = { requireLogin: true, authenticated: true };
    await page.getByRole("button", { name: "Retry" }).click();
    await page.getByText("Fixture key").first().waitFor();
    assert.equal(await skeletonElement.evaluate((element) => element === document.querySelector("#dashboard-skeleton")), true);
    assert.equal(await skeletonElement.evaluate((element) => getComputedStyle(element).opacity), "0");
    assert.equal(await page.title(), "OpenProxy - AI Infrastructure Management");
    assert.equal(await page.getByText("abcdef01").first().count(), 1);
    assert.equal(await page.getByRole("link", { name: /Translator/ }).first().count(), 1);
    await page.getByText("Require login is disabled", { exact: false }).waitFor();
    // Match the reference's empty language-switcher flex item and date text runs.
    assert.equal(await page.locator("#header-menu").evaluate((menu) => {
      const wrapper = menu.nextElementSibling;
      return wrapper?.tagName === "DIV" && wrapper.className === "" && wrapper.childNodes.length === 0;
    }), true);
    const dateRuns = await page.getByText(/^Created /).first().evaluate((element) => [...element.childNodes].map((node) => node.textContent));
    assert.deepEqual(dateRuns, ["Created ", await page.evaluate(() => new Date("2024-01-02T12:00:00Z").toLocaleDateString())]);

    // Read-only URL, masked value, show/hide, clipboard feedback.
    assert.equal(await page.getByLabel("Local endpoint URL").inputValue(), `${origin}/v1`);
    assert.equal(await page.getByText("fixture_...", { exact: true }).count(), 1);
    await page.getByRole("button", { name: "Show key Fixture key" }).click();
    await page.getByText("fixture_key_1234", { exact: true }).waitFor();
    await page.getByRole("button", { name: "Copy key Fixture key" }).click();
    await page.getByRole("button", { name: "Copy key Fixture key" }).getByText("check").waitFor();
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "fixture_key_1234");
    await page.getByRole("button", { name: "Copy key Fixture key" }).getByText("content_copy").waitFor({ timeout: 4000 });
    await page.getByRole("button", { name: "Hide key Fixture key" }).click();

    // Saving settings is pessimistic; errors do not change the switch.
    const requireKey = page.getByRole("switch", { name: "Require API key" });
    state.settingsFailure = true;
    await requireKey.click();
    await page.getByText("Failed to save API key requirement").waitFor();
    assert.equal(await requireKey.getAttribute("aria-checked"), "true");
    state.settingsFailure = false;
    await requireKey.click();
    await page.getByText("Require API key is disabled", { exact: false }).waitFor();
    assert.equal(await requireKey.getAttribute("aria-checked"), "false");
    assert.deepEqual(state.requests.filter((r) => r.method === "PATCH").map((r) => r.body), [{ requireApiKey: false }, { requireApiKey: false }]);

    // Pause requires confirmation; resume is immediate. Escape cancels delete.
    await page.getByRole("switch", { name: "Pause key Fixture key" }).click();
    await page.getByRole("dialog", { name: "Pause API key" }).waitFor();
    assert.equal(state.requests.some((r) => r.method === "PUT"), false);
    await page.getByRole("button", { name: "Pause", exact: true }).click();
    await page.getByText("Paused", { exact: true }).waitFor();
    await page.getByRole("switch", { name: "Resume key Fixture key" }).click();
    await page.getByRole("switch", { name: "Pause key Fixture key" }).waitFor();
    assert.deepEqual(state.requests.filter((r) => r.method === "PUT").map((r) => r.body), [{ isActive: false }, { isActive: true }]);
    await page.getByRole("button", { name: "Delete key Fixture key" }).click();
    await page.keyboard.press("Escape");
    assert.equal(await page.getByRole("dialog", { name: "Delete API key" }).count(), 0);
    await page.getByRole("button", { name: "Delete key Fixture key" }).click();
    await page.getByRole("button", { name: "Delete", exact: true }).click();
    await page.getByText("API key deleted").waitFor();
    assert.equal(state.requests.filter((r) => r.method === "DELETE").length, 1);

    // Creation sends the untrimmed name and reveals the returned key only after POST.
    await page.getByRole("button", { name: /Create Key/ }).first().click();
    await page.getByLabel("Key Name").fill("  My fixture  ");
    await page.getByRole("button", { name: "Create", exact: true }).click();
    await page.getByRole("dialog", { name: "API Key Created" }).waitFor();
    assert.equal(await page.getByLabel("Created API key").inputValue(), "fixture_created_key");
    assert.deepEqual(state.requests.filter((r) => r.path === "/api/keys" && r.method === "POST").at(-1).body, { name: "  My fixture  " });
    await page.getByRole("button", { name: /Copy/ }).last().click();
    await page.getByRole("button", { name: /Copied!/ }).waitFor();
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "fixture_created_key");
    await page.getByRole("button", { name: "Done" }).click();

    // Root is direct rendering (no redirect); empty successful key list provisions once.
    reset();
    state.keys = [];
    await go("/");
    await page.getByText("Default Key").first().waitFor();
    assert.equal(new URL(page.url()).pathname, "/");
    assert.equal(await page.locator("#dashboard-skeleton").count(), 0);
    assert.deepEqual(state.requests.filter((r) => r.path === "/api/keys").map((r) => [r.method, r.body]), [["GET", null], ["POST", { name: "Default Key" }], ["GET", null]]);

    // Malformed key JSON must not be mistaken for an empty successful list.
    reset();
    state.invalidKeysJson = true;
    await go("/dashboard/endpoint");
    await page.getByText("No API keys yet").waitFor();
    assert.equal(await page.locator("#dashboard-skeleton").count(), 1);
    assert.equal(state.requests.some((r) => r.method === "POST" && r.path === "/api/keys"), false);

    // A non-session 401 rechecks auth and leaves the dashboard intact.
    reset();
    state.keysCode = 401;
    await go("/dashboard/endpoint");
    await page.getByText("No API keys yet").waitFor();
    await page.waitForFunction(() => document.body.classList.contains("dashboard-ready"));
    await new Promise((resolve) => setTimeout(resolve, 200));
    assert.equal(state.requests.filter((r) => r.path === "/api/auth/status").length, 2);
    assert.equal(state.requests.some((r) => r.method === "POST" && r.path === "/api/keys"), false);
    assert.equal(new URL(page.url()).pathname, "/dashboard/endpoint");

    // Simultaneous 401s share the same in-flight recheck.
    reset();
    state.keysCode = 401;
    state.settingsCode = 401;
    let sessionChecks = 0;
    let releaseCheck;
    let checkEntered;
    const checkStarted = new Promise((resolve) => { checkEntered = resolve; });
    const holdCheck = new Promise((resolve) => { releaseCheck = resolve; });
    const sessionRoute = async (route) => {
      sessionChecks += 1;
      if (sessionChecks > 1) { checkEntered(); await holdCheck; }
      await route.fulfill({ json: { requireLogin: true, authenticated: true } }).catch(() => {});
    };
    await page.route("**/api/auth/status", sessionRoute);
    await go("/dashboard/endpoint");
    await checkStarted;
    await page.getByText("No API keys yet").waitFor();
    await new Promise((resolve) => setTimeout(resolve, 200));
    assert.equal(state.requests.filter((r) => r.path === "/api/settings" && r.method === "GET").length, 3);
    assert.equal(sessionChecks, 2);
    releaseCheck();
    await page.unroute("**/api/auth/status", sessionRoute);

    // A 401 recheck must survive removal of the key row that initiated it.
    reset();
    state.keys[0].isActive = false;
    let releaseDelete;
    let releaseRaceCheck;
    const heldDelete = new Promise((resolve) => { releaseDelete = resolve; });
    const heldRaceCheck = new Promise((resolve) => { releaseRaceCheck = resolve; });
    let raceChecks = 0;
    let resumeBody;
    const failedChecks = [];
    const recordFailure = (request) => {
      if (new URL(request.url()).pathname === "/api/auth/status") failedChecks.push(request.failure()?.errorText);
    };
    const raceStatus = async (route) => {
      raceChecks += 1;
      if (raceChecks === 2) await heldRaceCheck;
      await route.fulfill({ json: { requireLogin: true, authenticated: raceChecks === 1 } }).catch(() => {});
    };
    const raceKey = async (route) => {
      if (route.request().method() === "DELETE") {
        await heldDelete;
        return route.fulfill({ json: { success: true } }).catch(() => {});
      }
      resumeBody = route.request().postDataJSON();
      return route.fulfill({ status: 401, json: { error: "Unauthorized" } });
    };
    page.on("requestfailed", recordFailure);
    await page.route("**/api/auth/status", raceStatus);
    await page.route("**/api/keys/one", raceKey);
    try {
      await go("/dashboard/endpoint");
      await page.getByText("Fixture key", { exact: true }).waitFor();
      await page.getByRole("button", { name: "Delete key Fixture key" }).click();
      const deleteStarted = page.waitForRequest((request) => request.method() === "DELETE" && new URL(request.url()).pathname === "/api/keys/one");
      await page.getByRole("button", { name: "Delete", exact: true }).click();
      await deleteStarted;
      await page.keyboard.press("Escape");
      await page.getByRole("dialog", { name: "Delete API key" }).waitFor({ state: "hidden" });
      const recheckStarted = page.waitForRequest((request) => new URL(request.url()).pathname === "/api/auth/status");
      await page.getByRole("switch", { name: "Resume key Fixture key" }).click();
      await recheckStarted;
      releaseDelete();
      await page.getByText("Fixture key", { exact: true }).waitFor({ state: "detached" });
      await page.waitForTimeout(150);
      assert.deepEqual(failedChecks, []);
      assert.deepEqual(resumeBody, { isActive: true });
      releaseRaceCheck();
      await page.waitForURL(`${origin}/login`);
      assert.equal(raceChecks >= 2, true);
    } finally {
      releaseDelete();
      releaseRaceCheck();
      page.off("requestfailed", recordFailure);
      await page.unroute("**/api/keys/one", raceKey);
      await page.unroute("**/api/auth/status", raceStatus);
    }

    // Mobile sidebar, theme, language menu, and shutdown use only the fixture.
    reset();
    await page.setViewportSize({ width: 390, height: 844 });
    await go("/dashboard/endpoint");
    await page.getByText("Fixture key").first().waitFor();
    await page.getByRole("button", { name: "Open navigation" }).click();
    const mobileSidebar = page.locator(".translate-x-0").locator("aside");
    assert.equal(await mobileSidebar.isVisible(), true);
    await mobileSidebar.getByRole("button", { name: /Shutdown/ }).click();
    await page.getByRole("dialog", { name: "Close Proxy" }).waitFor();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.locator(".fixed.inset-0.z-40").click({ position: { x: 375, y: 400 } });
    await page.getByRole("button", { name: /Switch to (light|dark) mode/ }).click();
    assert.ok(["light", "dark"].includes(JSON.parse(await page.evaluate(() => localStorage.theme)).state.theme));
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    await page.getByRole("button", { name: /English/ }).click();
    await page.getByRole("dialog", { name: "Select Language" }).waitFor();
    await page.getByRole("button", { name: /Deutsch/ }).click();
    await page.waitForFunction(() => document.cookie.includes("locale=de"));
    await page.getByRole("heading", { name: "Endpunkt", exact: true }).waitFor();
    assert.deepEqual(state.requests.find((r) => r.path === "/api/locale").body, { locale: "de" });
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    await page.getByRole("button", { name: /Deutsch/ }).click();
    // Russian comes from the candidate's built public assets, not a fixture.
    const russianCatalog = page.waitForResponse((response) => response.url() === `${origin}/i18n/literals/ru.json`);
    await page.getByRole("dialog", { name: "Select Language" }).getByRole("button", { name: /Русский/ }).click();
    const catalogResponse = await russianCatalog;
    assert.equal(catalogResponse.status(), 200);
    assert.equal((await catalogResponse.json()).Endpoint, "Конечная точка");
    await page.getByRole("heading", { name: "Конечная точка", exact: true }).waitFor();
    assert.deepEqual(
      await page.getByText(/^Создано/).first().evaluate((element) => [...element.childNodes].map((node) => node.textContent)),
      ["Создано", dateRuns[1]],
    );
    assert.deepEqual(state.requests.filter((r) => r.path === "/api/locale").at(-1).body, { locale: "ru" });
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    await page.getByRole("button", { name: /Русский/ }).click();
    await page.getByRole("dialog", { name: "Select Language" }).getByRole("button", { name: /English/ }).click();
    await page.getByRole("heading", { name: "Endpoint", exact: true }).waitFor();
    assert.deepEqual(await page.getByText(/^Created /).first().evaluate((element) => [...element.childNodes].map((node) => node.textContent)), dateRuns);
    await page.getByRole("button", { name: "Open navigation" }).click();
    await mobileSidebar.getByRole("button", { name: /Shutdown/ }).click();
    await page.getByRole("button", { name: "Close", exact: true }).last().click();
    await page.getByText("Server Disconnected").waitFor();
    assert.equal(state.requests.filter((r) => r.path === "/api/shutdown").length, 1);

    // Chrome's real blocked-site-data SecurityError must not prevent theme
    // selection in the supported no-login mode.
    reset();
    state.status.requireLogin = false;
    state.settings.requireLogin = false;
    const profile = await fs.mkdtemp(path.join(os.homedir(), ".openproxy-theme-"));
    let blocked;
    try {
      await fs.mkdir(path.join(profile, "Default"));
      await fs.writeFile(path.join(profile, "Default", "Preferences"), JSON.stringify({ profile: { default_content_setting_values: { cookies: 2 } } }));
      blocked = await chromium.launchPersistentContext(profile, { executablePath: process.env.CHROME_BIN || undefined, chromiumSandbox: true, headless: true, colorScheme: "light" });
      const blockedPage = await blocked.newPage();
      const blockedErrors = [];
      blockedPage.on("pageerror", (error) => blockedErrors.push(String(error)));
      await blockedPage.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
      await blockedPage.goto(`${origin}/dashboard/endpoint`);
      await blockedPage.getByText("Fixture key", { exact: true }).waitFor();
      assert.equal(await blockedPage.evaluate(() => { try { void window.localStorage; return "available"; } catch (error) { return error.name; } }), "SecurityError", "use full Chrome; headless-shell may ignore the site-data preference");
      const darkClass = () => blockedPage.evaluate(() => document.documentElement.classList.contains("dark"));
      assert.equal(await darkClass(), false);
      await blockedPage.emulateMedia({ colorScheme: "dark" });
      await blockedPage.waitForFunction(() => document.documentElement.classList.contains("dark"));
      await blockedPage.getByRole("button", { name: "Switch to light mode" }).waitFor();
      await blockedPage.emulateMedia({ colorScheme: "light" });
      await blockedPage.waitForFunction(() => !document.documentElement.classList.contains("dark"));
      await blockedPage.getByRole("button", { name: "Switch to dark mode" }).click();
      await blockedPage.getByRole("button", { name: "Switch to light mode" }).waitFor();
      assert.equal(await darkClass(), true);
      assert.equal(await blockedPage.evaluate(() => document.documentElement.style.colorScheme), "dark");
      await blockedPage.emulateMedia({ colorScheme: "dark" });
      await blockedPage.emulateMedia({ colorScheme: "light" });
      await blockedPage.waitForTimeout(100);
      assert.equal(await darkClass(), true);
      await blockedPage.getByRole("button", { name: "Switch to light mode" }).click();
      await blockedPage.getByRole("button", { name: "Switch to dark mode" }).waitFor();
      assert.equal(await darkClass(), false);
      assert.equal(await blockedPage.evaluate(() => document.documentElement.style.colorScheme), "light");
      assert.deepEqual(blockedErrors, []);
    } finally {
      if (blocked) await blocked.close();
      await fs.rm(profile, { recursive: true, force: true });
    }

    const persisted = await browser.newContext({ colorScheme: "light" });
    try {
      await persisted.addInitScript(() => localStorage.setItem("theme", JSON.stringify({ state: { theme: "dark" }, version: 0 })));
      const persistedPage = await persisted.newPage();
      await persistedPage.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
      await persistedPage.goto(`${origin}/dashboard/endpoint`);
      await persistedPage.getByRole("button", { name: "Switch to light mode" }).waitFor();
      assert.equal(await persistedPage.evaluate(() => document.documentElement.classList.contains("dark")), true);
      await persistedPage.getByRole("button", { name: "Switch to light mode" }).click();
      await persistedPage.getByRole("button", { name: "Switch to dark mode" }).waitFor();
      assert.equal(await persistedPage.evaluate(() => JSON.parse(localStorage.theme).state.theme), "light");
    } finally { await persisted.close(); }

    // 127.0.0.2 is still an isolated loopback fixture, but is deliberately
    // not in the reference UI's exact loopback allowlist.
    reset();
    state.settings.requireApiKey = false;
    const remoteServer = http.createServer(server.listeners("request")[0]);
    try {
      await new Promise((resolve) => remoteServer.listen(0, "127.0.0.2", resolve));
      const remoteOrigin = `http://127.0.0.2:${remoteServer.address().port}`;
      const remotePage = await context.newPage();
      await remotePage.route("**/*", (route) => new URL(route.request().url()).origin === remoteOrigin ? route.continue() : route.abort());
      await remotePage.goto(`${remoteOrigin}/dashboard/endpoint`);
      await remotePage.getByText("Endpoint is exposed without an API key.").waitFor();
      await remotePage.getByRole("link", { name: "Enable", exact: true }).click();
      assert.equal(new URL(remotePage.url()).hash, "");
      await remotePage.close();
    } finally { remoteServer.close(); }

    // Logout uses the backend before navigating; an unauthenticated initial
    // session is redirected before any key data can be requested.
    reset();
    await go("/dashboard");
    await page.getByText("Fixture key").first().waitFor();
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    await page.getByRole("button", { name: /Logout/ }).click();
    await page.waitForURL(`${origin}/login`);
    await page.getByLabel(/^Password\s*\*$/).waitFor();
    assert.equal(await page.locator("#dashboard-skeleton").count(), 0);
    assert.equal(state.requests.filter((r) => r.path === "/api/auth/logout" && r.method === "POST").length, 1);
    reset();
    state.status.authenticated = false;
    await go("/dashboard");
    await page.waitForURL(`${origin}/login`);
    assert.equal(state.requests.some((r) => r.path === "/api/keys"), false);

    // Session expiry on API 401 navigates using the status response.
    reset();
    state.keysCode = 401;
    let checks = 0;
    await page.route("**/api/auth/status", async (route) => {
      checks += 1;
      await route.fulfill({ json: { requireLogin: true, authenticated: checks === 1 } });
    });
    await go("/dashboard/endpoint");
    await page.waitForURL(`${origin}/login`);
    assert.equal(checks >= 2, true);
    assert.deepEqual(errors, []);
    await context.close();
    console.log("Synthetic endpoint browser smoke passed");
  } finally {
    await browser.close();
    server.close();
  }
}

main().catch((error) => { console.error(error); server.close(); process.exitCode = 1; });
