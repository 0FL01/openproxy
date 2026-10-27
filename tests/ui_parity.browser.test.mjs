import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { isDeepStrictEqual } from "node:util";

// Run inside the isolated .ui-browser Compose network, never against production.
// UI_PARITY_MODE=record captures synthetic reference images; compare is the default.
const enabled = process.env.RUN_UI_PARITY_TESTS === "1";
const mode = process.env.UI_PARITY_MODE || "compare";
const selectedScreen = process.env.UI_PARITY_SCREEN;
const selectedSection = process.env.UI_PARITY_SECTION;
const referenceDir = resolve(process.env.UI_REFERENCE_DIR || "tests/ui/reference");
const artifactDir = resolve(process.env.UI_ARTIFACT_DIR || ".ui-browser/artifacts");
const appUrl = "https://app:4623";
const fixtureCommit = "0123456789abcdef0123456789abcdef01234567";
const callbackCodeQuery = "?code=fixture-code&state=fixture-state";
const callbackErrorQuery = "?error=fixture-error&error_description=Fixture+authorization+denied&state=fixture-state";
const compatibleId = "openai-compatible-00000000-0000-4000-8000-000000000001";
const compatiblePath = `/dashboard/providers/${compatibleId}`;
const compatibleTemplate = "/dashboard/providers/_dynamic.html";
const compatiblePrefix = "fixture-openai";
const compatibleBaseUrl = "https://fixture.invalid/v1";
// Exact resources observed from the frozen Astro template in the isolated fixture.
const compatibleAstroAssets = new Set([
  "/favicon.svg", "/providers/oai-cc.png",
  "/_astro/AddApiKeyModal.BxlGGI0q.js", "/_astro/Badge.D746pWIl.js", "/_astro/Button.BFNTE38v.js",
  "/_astro/Card.BYEgKEve.js", "/_astro/DashboardLayout.CIFa2vft.js", "/_astro/Input.kBbyj-k0.js",
  "/_astro/LanguageSwitcher.G7CXSwPB.js", "/_astro/Loading.C8ysA_qN.js", "/_astro/Modal.Bka5iDlT.js",
  "/_astro/ProviderDetailPageClient.C-HQNUch.js", "/_astro/ProviderIcon.yxx1WU3b.js",
  "/_astro/RuntimeI18nProvider.9AAH-dgj.js", "/_astro/Select.BnSa43mO.js", "/_astro/ThemeProvider.CSeaVM1W.js",
  "/_astro/Toggle.DgexzGQH.js", "/_astro/__ui_browser/fonts.css", "/_astro/__ui_browser/fonts/instrument-serif.woff2",
  "/_astro/__ui_browser/fonts/inter.woff2", "/_astro/__ui_browser/fonts/jetbrains-mono.woff2",
  "/_astro/client.BYP9nLUz.js", "/_astro/cn.C9Gfr6mj.js", "/_astro/dashboard.BZnXx_Nc.css",
  "/_astro/headerSearchStore.CLCS9igz.js", "/_astro/index.D9_NN57j.js", "/_astro/index.DgRPLtSb.js",
  "/_astro/jsx-runtime.CnDyYxjH.js", "/_astro/material-symbols-outlined.CqIkmgaP.woff2",
  "/_astro/runtime.CEWCR3UM.js", "/_astro/themeStore.y6YYrgKI.js", "/_astro/useAutoPingSettings.C0vN0LDE.js",
  "/_astro/useCopyToClipboard.CtuHBHI_.js",
]);

const screens = [
  { section: "login", name: "ready", hasPassword: true },
  { section: "login", name: "persisted-dark", hasPassword: true, storedTheme: "dark", colorScheme: "light" },
  { section: "login", name: "no-password", hasPassword: false },
  { section: "login", name: "error-fallback", hasPassword: true, login: "error" },
  { section: "login", name: "bad-password", hasPassword: true, login: "hint" },
  { section: "login", name: "password-change-form", hasPassword: false, login: "change" },
  { section: "endpoint", name: "ready" },
  { section: "endpoint", name: "scroll-bottom", action: "scroll-bottom" },
  { section: "endpoint", name: "mobile-sidebar", action: "sidebar", mobileOnly: true },
  { section: "endpoint", name: "no-api-key-required", insecure: true },
  { section: "endpoint", name: "key-paused", paused: true },
  { section: "endpoint", name: "show-key", action: "show" },
  { section: "endpoint", name: "create-key-dialog", action: "create" },
  { section: "endpoint", name: "created-key", action: "created" },
  { section: "endpoint", name: "delete-key-dialog", action: "delete" },
  { section: "endpoint", name: "pause-key-dialog", action: "pause" },
  { section: "providers", name: "ready" },
  { section: "providers", name: "search-match", action: "search-match", desktopOnly: true },
  { section: "providers", name: "search-empty", action: "search-empty", desktopOnly: true },
  { section: "providers", name: "free-filter", action: "free-filter" },
  { section: "providers", name: "scroll-api-keys", action: "scroll-api-keys" },
  { section: "providers", name: "show-all-scroll-bottom", action: "show-all-scroll-bottom" },
  { section: "providers", name: "add-openai-dialog", action: "add-openai-dialog" },
  { section: "providers", name: "add-openai-dialog-scroll-bottom", action: "add-openai-dialog-scroll-bottom" },
  { section: "providers-new", name: "ready" },
  { section: "providers-new", name: "search-match", action: "search-match" },
  { section: "providers-new", name: "search-retains-selection", action: "search-retains-selection" },
  { section: "providers-new", name: "oauth", action: "oauth" },
  { section: "providers-new", name: "no-auth", action: "no-auth" },
  { section: "providers-new", name: "api-key", action: "api-key" },
  { section: "providers-new", name: "api-key-scroll-bottom", action: "api-key-scroll-bottom", mobileOnly: true },
  { section: "providers-new", name: "compatible", action: "compatible" },
  { section: "provider-kilocode", name: "ready" },
  { section: "provider-kilocode", name: "available-models", action: "available-models" },
  { section: "provider-kilocode", name: "disable-model", action: "disable-model" },
  { section: "provider-kilocode", name: "restore-model", action: "restore-model" },
  { section: "provider-kilocode", name: "free-only", action: "free-only" },
  { section: "provider-kilocode", name: "add-model-dialog", action: "add-model-dialog" },
  { section: "provider-kilocode", name: "add-model-saved", action: "add-model-saved" },
  { section: "provider-kilocode", name: "disable-all-dialog", action: "disable-all-dialog" },
  { section: "provider-compatible", name: "ready" },
  { section: "provider-compatible", name: "edit-dialog", action: "edit-dialog" },
  { section: "provider-compatible", name: "edit-dialog-scroll-bottom", action: "edit-dialog-scroll-bottom" },
  { section: "provider-compatible", name: "delete-dialog", action: "delete-dialog" },
  { section: "callback", name: "code", query: callbackCodeQuery, colorScheme: "light" },
  { section: "callback", name: "error", query: callbackErrorQuery, colorScheme: "light" },
  { section: "callback", name: "no-auth-data", query: "", colorScheme: "light" },
  { section: "callback", name: "manual-copy", query: callbackCodeQuery, manual: true, colorScheme: "light" },
  { section: "callback", name: "manual-copied", query: callbackCodeQuery, manual: true, action: "copied", colorScheme: "light" },
];
const viewports = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
];

test("dashboard states match the synthetic visual reference", { skip: !enabled, timeout: 480_000 }, async (t) => {
  assert(["record", "compare"].includes(mode), "UI_PARITY_MODE must be record or compare");
  assert(!selectedScreen || screens.some((screen) => `${screen.section}-${screen.name}` === selectedScreen), "unknown UI_PARITY_SCREEN");
  assert(!selectedSection || screens.some((screen) => screen.section === selectedSection), "unknown UI_PARITY_SECTION");
  assert(!selectedScreen || !selectedSection || screens.some((screen) => screen.section === selectedSection && `${screen.section}-${screen.name}` === selectedScreen), "UI_PARITY_SCREEN and UI_PARITY_SECTION disagree");
  // Dioxus hashes change between builds. Compare runs may declare their exact,
  // same-origin static asset paths rather than broadening the network allowlist.
  const compatibleCandidateAssets = mode === "compare" ? JSON.parse(process.env.UI_PARITY_COMPATIBLE_ASSETS || "[]") : [];
  assert(Array.isArray(compatibleCandidateAssets) && compatibleCandidateAssets.every((path) =>
    typeof path === "string" && /^\/(?:assets|_astro|i18n)\/[a-zA-Z0-9_./-]+$/.test(path) && new URL(path, appUrl).pathname === path),
  "UI_PARITY_COMPATIBLE_ASSETS must be a JSON array of exact same-origin static paths");
  const { chromium } = await import(process.env.DASHBOARD_PLAYWRIGHT_MODULE || "playwright");
  // Partial rasterization caused nondeterministic antialiasing at rounded edges.
  const launchArgs = ["--force-color-profile=srgb", "--disable-partial-raster"];
  const browser = await chromium.launch({
    headless: true,
    chromiumSandbox: true,
    ...(process.env.DASHBOARD_CHROMIUM_EXECUTABLE ? { executablePath: process.env.DASHBOARD_CHROMIUM_EXECUTABLE } : {}),
    args: launchArgs,
  });
  const scratch = await mkdtemp(join(tmpdir(), "openproxy-ui-parity-"));

  try {
    await mkdir(referenceDir, { recursive: true });
    await mkdir(artifactDir, { recursive: true });
    const sourcePath = join(referenceDir, "source.json");
    const browserIdentity = { version: browser.version(), args: launchArgs };
    const source = existsSync(sourcePath)
      ? JSON.parse(await readFile(sourcePath, "utf8"))
      : { frontend: "astro", browser: browserIdentity, routes: {} };
    assert.equal(source.frontend, "astro", "reference source must be Astro");
    assert.deepEqual(source.browser, browserIdentity, "reference browser or raster settings differ; record a new reference");
    if (mode === "compare") assert(existsSync(sourcePath), "Astro reference metadata is missing");
    for (const viewport of viewports) {
      for (const theme of ["light", "dark"]) {
        for (const screen of screens) {
          if (selectedScreen && `${screen.section}-${screen.name}` !== selectedScreen) continue;
          if (selectedSection && screen.section !== selectedSection) continue;
          if (screen.mobileOnly && viewport.name !== "mobile") continue;
          if (screen.desktopOnly && viewport.name !== "desktop") continue;
          if (screen.colorScheme && screen.colorScheme !== theme) continue;
          const name = `${screen.section}-${screen.name}-${viewport.name}-${theme}`;
          await t.test(name, async () => {
            assert(mode !== "record" || process.env.UI_PARITY_REPLACE === "1" || !existsSync(join(referenceDir, `${name}.png`)), "reference already exists; set UI_PARITY_REPLACE=1 deliberately to replace it");
            const capture = async (colorScheme = theme) => {
              const context = await browser.newContext({
                viewport: { width: viewport.width, height: viewport.height },
                deviceScaleFactor: 1,
                colorScheme,
                locale: "en-US",
                timezoneId: "UTC",
                serviceWorkers: "block",
              });
              const errors = [];
              const writes = [];
              const unexpected = [];
              let scenarioReady = false;
              let verifyAfterCapture = null;
              const kiloWrites = {
                "disable-model": [{ method: "POST", path: "/api/models/disabled", body: { providerAlias: "kc", ids: ["fixture-standard"] } }],
                "restore-model": [{ method: "DELETE", path: "/api/models/disabled?providerAlias=kc&id=fixture-custom-disabled", body: null }],
                "free-only": [{ method: "PUT", path: "/api/providers/filters", body: { alias: "kc", freeOnly: true } }],
                "add-model-saved": [{ method: "POST", path: "/api/models/custom", body: { providerAlias: "kc", id: "fixture-new-model", type: "llm" } }],
              };
              const expectedWrites = screen.section === "provider-kilocode" ? (kiloWrites[screen.action] || []) : screen.login
                ? [{ method: "POST", path: "/api/auth/login", body: { password: "fixture-password" } }]
                : screen.action === "created"
                  ? [{ method: "POST", path: "/api/keys", body: { name: "Fixture new" } }]
                  : [];
              try {
                const page = await context.newPage();
                if (screen.storedTheme) {
                  await page.addInitScript((theme) => localStorage.setItem("theme", JSON.stringify({ state: { theme }, version: 0 })), screen.storedTheme);
                }
                if (screen.manual) {
                  await page.addInitScript((copy) => {
                    // Model a popup with no working relay or opener, without real credentials.
                    Object.defineProperty(window, "BroadcastChannel", { configurable: true, value: undefined });
                    Object.defineProperty(window, "localStorage", {
                      configurable: true,
                      get() { throw new DOMException("Fixture storage unavailable", "SecurityError"); },
                    });
                    if (copy) Object.defineProperty(navigator.clipboard, "writeText", {
                      configurable: true,
                      value: async (value) => { window.__fixtureCopied = value; },
                    });
                  }, screen.action === "copied");
                }
                let created = false;
                let authenticated = screen.section !== "login";
                let kiloDisabled = ["fixture-custom-disabled", "fixture-disabled"];
                let kiloCustom = [
                  { providerAlias: "kc", id: "fixture-custom", name: "Fixture Custom", type: "llm" },
                  { providerAlias: "kc", id: "fixture-custom-disabled", name: "Fixture Disabled Custom", type: "llm" },
                ];
                let kiloFreeOnly = false;
                page.on("pageerror", (error) => errors.push(error.message));
                await page.clock.setFixedTime(new Date("2026-10-01T12:00:00Z"));
                if (["callback", "provider-compatible"].includes(screen.section)) await context.routeWebSocket("**/*", (socket) => {
                  unexpected.push("WebSocket blocked");
                  socket.close();
                });
                await context.route("**/*", async (route) => {
                  const request = route.request();
                  const url = new URL(request.url());
                  const path = url.pathname;
                  const method = request.method();
                  const assets = screen.section === "callback"
                    ? ((path === "/callback" && url.search === screen.query) || (mode === "compare" && !url.search && (path === "/favicon.svg" || path.startsWith("/_astro/") || path.startsWith("/assets/"))))
                    : screen.section === "provider-compatible"
                    ? (!url.search && (path === compatiblePath || compatibleAstroAssets.has(path) || (mode === "compare" && compatibleCandidateAssets.includes(path))))
                    : (!["providers-new", "provider-kilocode"].includes(screen.section) && (path === "/login" || path === "/dashboard/endpoint")) || path === "/favicon.svg" || path.startsWith("/_astro/") || path.startsWith("/assets/") || path.startsWith("/i18n/") ||
                    (screen.section === "providers" && (path === "/dashboard/providers" || /^\/providers\/[a-z0-9_-]+\.png$/.test(path))) ||
                    (screen.section === "providers-new" && !url.search && ["/dashboard/providers/new", "/providers/codex.png", "/providers/opencode-zen.png", "/providers/openai.png"].includes(path)) ||
                    (screen.section === "provider-kilocode" && !url.search && ["/dashboard/providers/kilocode", "/providers/kilocode.png"].includes(path));
                  if (url.origin === appUrl && method === "GET" && assets) return route.continue();
                  const providerGet = screen.section === "providers" && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true")
                  );
                  const providerNewGet = screen.section === "providers-new" && method === "GET" && !url.search &&
                    ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog"].includes(path);
                  const kiloGet = screen.section === "provider-kilocode" && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/kilo/free-models", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", "?providerAlias=kc"].includes(url.search))
                  );
                  const compatibleGet = screen.section === "provider-compatible" && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", `?providerAlias=${encodeURIComponent(compatibleId)}`].includes(url.search))
                  );
                  const kiloMutation = screen.section === "provider-kilocode" && method !== "GET" &&
                    expectedWrites.some((write) => write.method === method && write.path === `${path}${url.search}`);
                  const valid = url.origin === appUrl && (providerGet || providerNewGet || kiloGet || kiloMutation || compatibleGet || (!["providers", "providers-new", "provider-kilocode", "provider-compatible", "callback"].includes(screen.section) && (
                    (method === "GET" && ["/api/auth/status", "/api/settings", "/api/keys", "/api/build", "/api/catalog"].includes(path)) ||
                    (method === "POST" && path === "/api/auth/login" && !!screen.login) ||
                    (method === "POST" && path === "/api/keys" && screen.action === "created")
                  )));
                  if (!valid) {
                    unexpected.push(`${method} ${url.origin}${path}`);
                    return route.abort("blockedbyclient");
                  }
                  if (method !== "GET") {
                    const write = { method, path: screen.section === "provider-kilocode" ? `${path}${url.search}` : path, body: request.postDataJSON() };
                    if (screen.section === "provider-kilocode" && !isDeepStrictEqual(write, expectedWrites[writes.length])) {
                      unexpected.push(`unexpected mutation ${method} ${url.origin}${path}${url.search}`);
                      return route.abort("blockedbyclient");
                    }
                    writes.push(write);
                  }
                  let status = 200;
                  let body = {};
                  const headers = {};
                  if (path === "/api/auth/status") {
                    body = { requireLogin: true, authenticated, hasPassword: screen.hasPassword ?? true };
                  } else if (path === "/api/auth/login") {
                    if (screen.login === "change") {
                      authenticated = true;
                      headers["set-cookie"] = "auth_token=synthetic-session; Path=/; HttpOnly; Secure; SameSite=Lax";
                      body = { success: true, mustChangePassword: true };
                    }
                    else {
                      status = 401;
                      body = screen.login === "hint"
                        ? { error: "Invalid password", resetHint: "Forgot password? Run `openproxy auth reset-password` on the host to restore the generated initial password." }
                        : { error: "Invalid password. Please try again." };
                    }
                  } else if (path === "/api/settings") {
                    body = { requireLogin: true, requireApiKey: !screen.insecure, hasPassword: true };
                  } else if (path === "/api/build") {
                    body = { commit: ["provider-kilocode", "provider-compatible"].includes(screen.section) ? null : fixtureCommit };
                  } else if (path === "/api/catalog") {
                    body = screen.section === "provider-kilocode"
                      ? { providerIdToAlias: { kilocode: "kc" }, providerModels: [{ alias: "kc", models: [
                        { id: "fixture-standard", name: "Fixture Standard", kind: "llm", contextWindow: 128000 },
                        { id: "fixture-vision", name: "Fixture Vision", kind: "llm", contextWindow: 256000 },
                        { id: "fixture-disabled", name: "Fixture Disabled", kind: "llm" },
                      ] }] }
                      : { providerIdToAlias: {}, providerModels: [], xaiCatalog: { status: "ready" } };
                  } else if (path === "/api/providers") {
                    body = screen.section === "provider-compatible" ? { connections: [] } : screen.section === "provider-kilocode" ? { connections: [
                      { id: "fixture-kilo-connection", provider: "kilocode", name: "Fixture connection", authType: "oauth", testStatus: "active", isActive: true, priority: 1 },
                    ] } : { connections: [
                      { id: "fixture-compatible-connection", provider: "openai-compatible-fixture", authType: "apikey", testStatus: "active", isActive: true },
                      { id: "fixture-codex-connection", provider: "codex", authType: "oauth", testStatus: "success", isActive: true },
                      { id: "fixture-openai-connection", provider: "openai", authType: "apikey", testStatus: "active", isActive: true },
                    ] };
                  } else if (path === "/api/provider-nodes") {
                    body = screen.section === "provider-compatible" ? { nodes: [{
                      id: compatibleId, type: "openai-compatible", name: "Fixture Compatible", prefix: compatiblePrefix,
                      apiType: "chat", baseUrl: compatibleBaseUrl, createdAt: "2026-10-01T12:00:00Z", updatedAt: "2026-10-01T12:00:00Z",
                    }] } : screen.section === "provider-kilocode" ? { nodes: [] } : { nodes: [
                      { id: "openai-compatible-fixture", type: "openai-compatible", name: "Fixture Connected", apiType: "chat" },
                      { id: "anthropic-compatible-fixture", type: "anthropic-compatible", name: "Fixture Disconnected" },
                    ] };
                  } else if (path === "/api/proxy-pools") {
                    body = { proxyPools: [] };
                  } else if (path === "/api/providers/kilo/free-models") {
                    body = { models: [{ id: "fixture-free", name: "Fixture Free" }] };
                  } else if (path === "/api/providers/filters") {
                    if (method === "PUT") {
                      kiloFreeOnly = request.postDataJSON().freeOnly;
                      return route.fulfill({ status: 204 });
                    }
                    body = screen.section === "provider-compatible" ? { filters: {} } : { filters: { kc: { freeOnly: kiloFreeOnly } } };
                  } else if (path === "/api/models/disabled") {
                    if (method === "POST") kiloDisabled = [...new Set([...kiloDisabled, ...request.postDataJSON().ids])].sort();
                    else if (method === "DELETE") kiloDisabled = kiloDisabled.filter((id) => id !== url.searchParams.get("id"));
                    body = method === "GET" ? { ids: url.searchParams.get("providerAlias") === "kc" ? kiloDisabled : [] } : { success: true };
                  } else if (path === "/api/models/custom") {
                    if (method === "POST") kiloCustom = [...kiloCustom, request.postDataJSON()];
                    body = screen.section === "provider-compatible" ? { models: [{ providerAlias: compatibleId, id: "fixture-model", type: "llm" }] }
                      : method === "GET" ? { models: kiloCustom } : { success: true, added: true };
                  } else if (path === "/api/models/alias") {
                    body = { aliases: {} };
                  } else if (path === "/api/models/favorites") {
                    body = screen.section === "provider-compatible" ? { favorites: {} } : { favorites: { kc: ["fixture-standard"] } };
                  } else if (path === "/api/models") {
                    body = screen.section === "provider-compatible" ? { models: [] }
                      : { models: [{ provider: "kilocode", model: "fixture-vision", fullModel: "kilocode/fixture-vision", caps: { vision: true, reasoning: true } }] };
                  } else if (path === "/api/keys") {
                    if (request.method() === "POST") {
                      created = true;
                      body = { key: "fixture-token-new" };
                    } else {
                      body = { keys: [
                        { id: "fixture", name: "Fixture key", key: "fixture-token-123", createdAt: "2026-10-01T12:00:00Z", isActive: !screen.paused },
                        ...(screen.action === "scroll-bottom" ? Array.from({ length: 10 }, (_, index) => ({
                          id: `scroll-${index}`, name: `Fixture scroll ${index}`, key: `fixture-token-scroll-${index}`,
                          createdAt: "2026-10-01T12:00:00Z", isActive: true,
                        })) : []),
                        ...(created ? [{ id: "new", name: "Fixture new", key: "fixture-token-new", createdAt: "2026-10-01T12:00:00Z", isActive: true }] : []),
                      ] };
                    }
                  }
                  await route.fulfill({ status, headers, json: body });
                });

                const path = screen.section === "login" ? "/login" : screen.section === "providers" ? "/dashboard/providers" : screen.section === "providers-new" ? "/dashboard/providers/new" : screen.section === "provider-kilocode" ? "/dashboard/providers/kilocode" : screen.section === "provider-compatible" ? compatiblePath : screen.section === "callback" ? "/callback" : "/dashboard/endpoint";
                const sourceRoute = screen.section === "provider-compatible" ? compatibleTemplate : path;
                const response = await page.goto(`${appUrl}${path}${screen.query || ""}`, { waitUntil: "domcontentloaded" });
                assert(response?.ok(), `failed to load ${path}`);
                if (mode === "record") {
                  const html = await response.body();
                  if (screen.section === "callback") {
                    assert(source.routes[path], "standalone Astro callback hash must be pinned before recording");
                    for (const marker of ["<title>OpenProxy — OAuth Callback</title>", 'id="card"', 'id="spinner"', 'id="title"', 'id="message"', "new URLSearchParams(window.location.search)", "oauth_callback", "No Auth Data Found"]) {
                      assert(html.includes(marker), "record mode requires the standalone Astro callback markup");
                    }
                  } else {
                    const component = screen.section === "login" ? "LoginPageClient." : screen.section === "providers" ? "ProvidersPageClient." : screen.section === "providers-new" ? "ProvidersNewPageClient." : ["provider-kilocode", "provider-compatible"].includes(screen.section) ? "ProviderDetailPageClient." : "EndpointPageClient.";
                    assert(html.includes(`component-url="/_astro/${component}`), "record mode requires the Astro reference");
                    if (screen.section === "provider-compatible") assert(source.routes[sourceRoute], "dynamic Astro provider template hash must be pinned before recording");
                  }
                  const hash = createHash("sha256").update(html).digest("hex");
                  assert(!source.routes[sourceRoute] || source.routes[sourceRoute] === hash, `Astro reference changed for ${sourceRoute}`);
                  source.routes[sourceRoute] = hash;
                  await writeFile(sourcePath, JSON.stringify(source, null, 2) + "\n");
                } else {
                  assert(source.routes[sourceRoute], `missing Astro reference identity for ${sourceRoute}`);
                  assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "compare mode requires the Dioxus candidate");
                }
                if (screen.section === "login") await page.getByRole("heading", { name: "OpenProxy" }).waitFor();
                else if (screen.section === "providers") await page.getByRole("heading", { name: "Custom Providers (OpenAI/Anthropic Compatible)" }).waitFor();
                else if (screen.section === "providers-new") await page.getByRole("heading", { name: "Add New Provider" }).waitFor();
                else if (screen.section === "provider-kilocode") await page.getByRole("checkbox", { name: "Select connection Fixture connection" }).waitFor();
                else if (screen.section === "provider-compatible") await page.getByRole("heading", { name: "OpenAI Compatible Details", exact: true }).waitFor();
                else if (screen.section === "callback") await page.getByRole("heading", { name: screen.name === "error" ? "Authentication Failed" : screen.name === "no-auth-data" ? "No Auth Data Found" : "Authentication Complete", exact: true }).waitFor();
                else await page.getByText("Fixture key", { exact: true }).waitFor();
                await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}" });
                if (screen.section === "callback") {
                  await page.evaluate(async () => { await document.fonts.ready; });
                  const cdp = await context.newCDPSession(page);
                  try {
                    await cdp.send("DOM.enable");
                    await cdp.send("CSS.enable");
                    const { root } = await cdp.send("DOM.getDocument");
                    for (const selector of ["h1", "p", ...(screen.manual ? ["button"] : [])]) {
                      const { nodeId } = await cdp.send("DOM.querySelector", { nodeId: root.nodeId, selector });
                      assert(nodeId, `callback ${selector} must exist for font verification`);
                      const { fonts } = await cdp.send("CSS.getPlatformFontsForNode", { nodeId });
                      assert(fonts.length > 0 && fonts.every((font) => font.familyName === "Liberation Sans" && !font.isCustomFont && font.glyphCount > 0), `callback ${selector} must use the pinned system font`);
                    }
                  } finally {
                    await cdp.detach();
                  }
                } else {
                  const missingFonts = await page.evaluate(async () => {
                    await document.fonts.ready;
                    const fonts = [
                      ...[400, 500, 600, 700].map((weight) => `${weight} 14px Inter`),
                      '400 14px "Instrument Serif"',
                      ...[400, 500, 600].map((weight) => `${weight} 14px "JetBrains Mono"`),
                      '400 14px "Material Symbols Outlined"',
                    ];
                    const loaded = await Promise.all(fonts.map((font) => document.fonts.load(font, "OpenProxy")));
                    return fonts.filter((_, index) => !loaded[index].length || loaded[index].some((font) => font.status !== "loaded"));
                  });
                  assert.deepEqual(missingFonts, [], "reference fonts must be loaded, not silently substituted");
                }
                if (screen.section === "login") {
                  if (screen.login) {
                    await page.getByPlaceholder("Enter your password").fill("fixture-password");
                    await page.getByRole("button", { name: "Sign In" }).click();
                    if (screen.login === "change") await page.getByText("Choose a new password before continuing").waitFor();
                    else if (screen.login === "hint") await page.getByText("openproxy auth reset-password").waitFor();
                    else await page.getByText("Invalid password. Please try again.").waitFor();
                  } else if (!screen.hasPassword) {
                    await page.getByText("Security risk: no password set.", { exact: false }).waitFor();
                  }
                } else if (screen.section === "callback") {
                  assert.equal(await page.evaluate(() => window.opener), null, "callback fixture must have no opener");
                  if (screen.name === "error") {
                    await page.getByText("Fixture authorization denied", { exact: true }).waitFor();
                  } else if (screen.name === "no-auth-data") {
                    await page.getByText("The URL did not contain OAuth parameters.", { exact: false }).waitFor();
                    await page.getByText("Callback URL", { exact: true }).waitFor();
                  } else {
                    await page.getByText("You can close this window and return to the dashboard.", { exact: true }).waitFor();
                  }
                  if (screen.manual) {
                    assert(await page.evaluate(() => {
                      try { void window.localStorage; return false; }
                      catch { return typeof window.BroadcastChannel === "undefined"; }
                    }), "manual fallback requires both relay methods to be unavailable");
                    const fullUrl = `${appUrl}/callback${screen.query}`;
                    await page.getByText(`Full URL: ${fullUrl}`, { exact: true }).waitFor();
                    const copy = page.getByRole("button", { name: "Copy URL", exact: true });
                    await copy.waitFor();
                    if (screen.action === "copied") {
                      await copy.click();
                      await page.getByRole("button", { name: "Copied!", exact: true }).waitFor();
                      assert.equal(await page.evaluate(() => window.__fixtureCopied), fullUrl);
                    }
                  } else {
                    assert(await page.evaluate(() => typeof window.BroadcastChannel === "function" && typeof window.localStorage.setItem === "function"), "normal callback relays must be available");
                    assert.equal(await page.getByRole("button", { name: "Copy URL", exact: true }).count(), 0);
                  }
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "provider-compatible") {
                  const detailsHeading = page.getByRole("heading", { name: "OpenAI Compatible Details", exact: true });
                  const details = detailsHeading.locator("../..");
                  await page.getByRole("heading", { name: "Fixture Compatible", exact: true }).waitFor();
                  await page.getByText("0 connections", { exact: true }).waitFor();
                  await page.getByText("No connections yet", { exact: true }).waitFor();
                  await page.getByText(`Chat Completions · ${compatibleBaseUrl}/chat/completions`, { exact: true }).waitFor();
                  await page.locator("code").filter({ hasText: `${compatiblePrefix}/fixture-model` }).waitFor();
                  assert(await details.getByRole("button", { name: /Add API Key/ }).isEnabled());
                  await page.waitForLoadState("networkidle");
                  const icon = page.getByRole("img", { name: "Fixture Compatible", exact: true });
                  await icon.waitFor();
                  assert(await icon.evaluate((element) => element.complete && element.naturalWidth > 0), "compatible icon must load");
                  if (screen.action === "edit-dialog" || screen.action === "edit-dialog-scroll-bottom") {
                    await details.getByRole("button", { name: /Edit/ }).click();
                    const heading = page.getByRole("heading", { name: "Edit OpenAI Compatible", exact: true });
                    await heading.waitFor();
                    const modal = heading.locator("../../..");
                    assert.equal(await modal.getByPlaceholder("OpenAI Compatible (Prod)").inputValue(), "Fixture Compatible");
                    assert.equal(await modal.getByPlaceholder("oc-prod").inputValue(), compatiblePrefix);
                    assert.equal(await modal.getByRole("combobox").inputValue(), "chat");
                    assert.equal(await modal.getByPlaceholder("https://api.openai.com/v1").inputValue(), compatibleBaseUrl);
                    assert.equal(await modal.locator('input[type="password"]').inputValue(), "");
                    assert.equal(await modal.getByPlaceholder("e.g. my-model-id").inputValue(), "");
                    assert(await modal.getByRole("button", { name: "Check", exact: true }).isDisabled());
                    assert(await modal.getByRole("button", { name: "Save", exact: true }).isEnabled());
                    if (screen.action === "edit-dialog-scroll-bottom") {
                      const body = modal.locator(".overflow-y-auto");
                      const scrollTop = await body.evaluate((element) => {
                        element.scrollTop = element.scrollHeight;
                        return element.scrollTop;
                      });
                      assert(scrollTop > 0, "edit dialog must overflow");
                      assert(await modal.getByRole("button", { name: "Save", exact: true }).evaluate((element) => {
                        const rect = element.getBoundingClientRect();
                        const parent = element.closest(".overflow-y-auto").getBoundingClientRect();
                        return rect.top >= parent.top && rect.bottom <= parent.bottom;
                      }), "edit actions must be visible after scrolling");
                    }
                  } else if (screen.action === "delete-dialog") {
                    await details.getByRole("button", { name: /Delete/ }).click();
                    const heading = page.getByRole("heading", { name: "Delete compatible node", exact: true });
                    await heading.waitFor();
                    const modal = heading.locator("../../..");
                    await modal.getByText("Delete this OpenAI Compatible node", { exact: false }).waitFor();
                    await modal.locator("code").filter({ hasText: /^Fixture Compatible$/ }).waitFor();
                    assert(await modal.getByRole("button", { name: "Delete", exact: true }).isEnabled());
                  }
                  await page.mouse.move(1, 1);
                  await page.waitForFunction(() => Array.from(document.images).every((image) => image.complete));
                  await page.evaluate(async () => {
                    await Promise.all(Array.from(document.images, (image) => image.decode().catch(() => {})));
                    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
                  });
                } else if (screen.section === "provider-kilocode") {
                  const modelCode = (id) => page.locator("code").filter({ hasText: new RegExp(`^kc/${id}$`) });
                  const modelRow = (id) => page.locator("div.group").filter({ has: modelCode(id) });
                  const restored = (id) => page.getByTitle("Restore model").filter({ hasText: id });
                  const modelsHeading = page.getByRole("heading", { name: "Available Models", exact: true });
                  const waitBase = async () => {
                    await page.getByRole("checkbox", { name: "Select connection Fixture connection" }).waitFor();
                    await modelsHeading.waitFor();
                    await modelCode("fixture-free").waitFor();
                    await modelCode("fixture-custom").waitFor();
                  };
                  const scrollModels = async () => {
                    const scrollTop = await page.locator("main .overflow-y-auto").evaluate((element) => {
                      element.scrollTop = element.scrollHeight;
                      return element.scrollTop;
                    });
                    assert(scrollTop > 0, "detail page must scroll to Available Models");
                    assert(await modelsHeading.evaluate((element) => {
                      const rect = element.getBoundingClientRect();
                      const parent = element.closest(".overflow-y-auto").getBoundingClientRect();
                      return rect.top >= parent.top && rect.bottom <= parent.bottom;
                    }), "Available Models heading must be visible after scrolling");
                  };
                  const waitWrite = (method, path) => page.waitForResponse((response) => {
                    const url = new URL(response.url());
                    return response.request().method() === method && `${url.pathname}${url.search}` === path;
                  });
                  const waitBuild = async () => {
                    if (viewport.name === "desktop") await page.waitForFunction(() =>
                      Array.from(document.querySelectorAll('[aria-label="Build commit unavailable"]')).some((element) => {
                        const rect = element.getBoundingClientRect();
                        return rect.width > 0 && rect.height > 0 && rect.left >= 0 && rect.right <= innerWidth &&
                          rect.top >= 0 && rect.bottom <= innerHeight && getComputedStyle(element).visibility !== "hidden";
                      }));
                  };
                  const reloadDetail = async () => {
                    // The screenshot is already captured; verify read-back on a fresh document.
                    await page.waitForLoadState("networkidle");
                    assert((await page.reload({ waitUntil: "domcontentloaded" }))?.ok());
                    await page.waitForLoadState("networkidle");
                  };
                  await waitBuild();
                  await waitBase();
                  await modelCode("fixture-standard").waitFor();
                  await modelRow("fixture-standard").getByRole("button", { name: "Remove from favorites" }).waitFor();
                  await modelRow("fixture-standard").getByText("128K ctx", { exact: true }).waitFor();
                  await modelRow("fixture-vision").getByText("visibility", { exact: true }).waitFor();
                  await modelRow("fixture-vision").getByText("neurology", { exact: true }).waitFor();
                  await modelRow("fixture-vision").getByText("256K ctx", { exact: true }).waitFor();
                  await restored("fixture-disabled").waitFor();
                  await restored("fixture-custom-disabled").waitFor();
                  await page.getByTitle("Show only free models").waitFor();
                  if (screen.action) await scrollModels();
                  if (screen.action === "disable-model") {
                    const response = waitWrite("POST", "/api/models/disabled");
                    await modelRow("fixture-standard").hover();
                    await modelRow("fixture-standard").getByTitle("Disable model").click();
                    assert((await response).ok());
                    await restored("fixture-standard").waitFor();
                    await modelCode("fixture-standard").waitFor({ state: "detached" });
                    await restored("fixture-disabled").waitFor();
                    await restored("fixture-custom-disabled").waitFor();
                    await page.waitForLoadState("networkidle");
                    await scrollModels();
                    verifyAfterCapture = async () => {
                      await reloadDetail();
                      await waitBase();
                      await restored("fixture-standard").waitFor();
                      await restored("fixture-disabled").waitFor();
                      await restored("fixture-custom-disabled").waitFor();
                      assert.equal(await modelCode("fixture-standard").count(), 0);
                    };
                  } else if (screen.action === "restore-model") {
                    const response = waitWrite("DELETE", "/api/models/disabled?providerAlias=kc&id=fixture-custom-disabled");
                    await restored("fixture-custom-disabled").click();
                    assert((await response).ok());
                    await modelRow("fixture-custom-disabled").getByText("Fixture Disabled Custom", { exact: true }).waitFor();
                    await restored("fixture-disabled").waitFor();
                    assert.equal(await restored("fixture-custom-disabled").count(), 0);
                    await page.waitForLoadState("networkidle");
                    await scrollModels();
                    verifyAfterCapture = async () => {
                      await reloadDetail();
                      await waitBase();
                      await modelRow("fixture-custom-disabled").getByText("Fixture Disabled Custom", { exact: true }).waitFor();
                      await restored("fixture-disabled").waitFor();
                      assert.equal(await restored("fixture-custom-disabled").count(), 0);
                    };
                  } else if (screen.action === "free-only") {
                    const response = waitWrite("PUT", "/api/providers/filters");
                    await page.getByTitle("Show only free models").click();
                    assert((await response).ok());
                    await modelCode("fixture-standard").waitFor({ state: "detached" });
                    await restored("fixture-disabled").waitFor();
                    await restored("fixture-custom-disabled").waitFor();
                    assert.equal(await modelCode("fixture-vision").count(), 0);
                    assert.match(await page.getByTitle("Show only free models").getAttribute("class"), /\bborder-primary\b/);
                    await page.waitForLoadState("networkidle");
                    await scrollModels();
                    verifyAfterCapture = async () => {
                      await reloadDetail();
                      await waitBase();
                      await modelCode("fixture-standard").waitFor({ state: "detached" });
                      await restored("fixture-disabled").waitFor();
                      await restored("fixture-custom-disabled").waitFor();
                      assert.equal(await modelCode("fixture-vision").count(), 0);
                      assert.match(await page.getByTitle("Show only free models").getAttribute("class"), /\bborder-primary\b/);
                    };
                  } else if (screen.action === "add-model-dialog" || screen.action === "add-model-saved") {
                    await page.getByRole("button", { name: /Add Model/ }).click();
                    const heading = page.getByRole("heading", { name: "Add Custom Model", exact: true });
                    await heading.waitFor();
                    const modal = heading.locator("../../..");
                    await page.getByPlaceholder("e.g. claude-opus-4-5").fill("kc/fixture-new-model");
                    assert.equal(await modal.locator("code").first().textContent(), "fixture-new-model");
                    assert.equal(await modal.locator("code").last().textContent(), "kc/fixture-new-model");
                    const add = modal.getByRole("button", { name: "Add Model", exact: true });
                    assert(await add.isEnabled());
                    if (screen.action === "add-model-saved") {
                      const response = waitWrite("POST", "/api/models/custom");
                      await add.click();
                      assert((await response).ok());
                      await heading.waitFor({ state: "detached" });
                      await modelCode("fixture-new-model").waitFor();
                      await restored("fixture-disabled").waitFor();
                      await restored("fixture-custom-disabled").waitFor();
                      await page.waitForLoadState("networkidle");
                      await scrollModels();
                      verifyAfterCapture = async () => {
                        await reloadDetail();
                        await waitBase();
                        await modelCode("fixture-new-model").waitFor();
                        await restored("fixture-disabled").waitFor();
                        await restored("fixture-custom-disabled").waitFor();
                      };
                    }
                  } else if (screen.action === "disable-all-dialog") {
                    await page.getByRole("button", { name: /Disable All/ }).click();
                    await page.getByRole("heading", { name: "Disable all models", exact: true }).waitFor();
                    await page.getByText("Disable all 4 model(s) for this provider?").waitFor();
                  }
                  await page.mouse.move(1, 1);
                  await page.waitForFunction(() => Array.from(document.images).every((image) => image.complete));
                  await page.evaluate(async () => {
                    await Promise.all(Array.from(document.images, (image) => image.decode().catch(() => {})));
                    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
                  });
                  await waitBuild();
                } else if (screen.section === "providers-new") {
                  if (viewport.name === "desktop") await page.getByText(fixtureCommit.slice(0, 8), { exact: true }).first().waitFor();
                  const select = page.getByRole("combobox");
                  assert.equal(await select.inputValue(), "");
                  assert(await page.getByRole("button", { name: "Connect Provider" }).isDisabled());
                  if (screen.action === "search-match") {
                    await page.getByPlaceholder("Search providers...").fill("cOdEx");
                    await page.getByText(/^1 of \d+ providers$/).waitFor();
                    assert.deepEqual(await select.locator("option").evaluateAll((options) => options.map((option) => option.value)), ["", "oauth:codex"]);
                  } else if (screen.action === "search-retains-selection") {
                    await select.selectOption("oauth:codex");
                    await page.getByPlaceholder("Search providers...").fill("no-such-provider");
                    await page.getByText(/^0 of \d+ providers$/).waitFor();
                    assert.equal(await select.inputValue(), "oauth:codex");
                    assert.deepEqual(await select.locator("option").evaluateAll((options) => options.map((option) => option.value)), ["", "oauth:codex"]);
                    await page.getByRole("button", { name: "Start OAuth" }).waitFor();
                  } else if (screen.action === "oauth") {
                    await select.selectOption("oauth:codex");
                    await page.getByText("Connect with your subscription account via browser OAuth.").waitFor();
                    await page.getByText("You will be redirected to complete OAuth for", { exact: false }).waitFor();
                    await page.getByRole("button", { name: "Start OAuth" }).waitFor();
                  } else if (screen.action === "no-auth") {
                    await select.selectOption("free:opencode-zen");
                    await page.getByText("Free-tier OAuth / no-auth providers. · No auth required").waitFor();
                    await page.getByRole("button", { name: "Open Provider" }).waitFor();
                    assert.equal(await page.getByPlaceholder("sk-...").count(), 0);
                  } else if (screen.action === "api-key" || screen.action === "api-key-scroll-bottom") {
                    await select.selectOption("apikey:openai");
                    assert.equal(await page.getByPlaceholder("e.g. Production key").inputValue(), "");
                    assert.equal(await page.getByPlaceholder("sk-...").inputValue(), "");
                    await page.getByText("Leave blank to open the providers list with this provider pre-selected (?add=).").waitFor();
                    const addKey = page.getByRole("button", { name: "Add Key on List" });
                    await addKey.waitFor();
                    if (screen.action === "api-key-scroll-bottom") {
                      const scrollTop = await page.locator("main .overflow-y-auto").evaluate((element) => {
                        element.scrollTop = element.scrollHeight;
                        return element.scrollTop;
                      });
                      assert(scrollTop > 0, "mobile API-key form must overflow");
                      assert(await addKey.evaluate((element) => {
                        const rect = element.getBoundingClientRect();
                        const parent = element.closest(".overflow-y-auto").getBoundingClientRect();
                        return rect.top >= parent.top && rect.bottom <= parent.bottom;
                      }), "API-key action must be visible after scrolling");
                    }
                  } else if (screen.action === "compatible") {
                    await select.selectOption("compatible:openai-compatible");
                    await page.getByText("Add a custom OpenAI- or Anthropic-compatible base URL from the providers list.", { exact: false }).waitFor();
                    await page.getByRole("button", { name: "Open Compatible Setup" }).waitFor();
                  }
                  await page.waitForFunction(() => Array.from(document.images).every((image) => image.complete));
                  await page.evaluate(async () => {
                    await Promise.all(Array.from(document.images, (image) => image.decode().catch(() => {})));
                    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
                  });
                } else if (screen.section === "providers") {
                  if (viewport.name === "desktop") await page.getByText(fixtureCommit.slice(0, 8), { exact: true }).first().waitFor();
                  const connected = page.locator('a[href="/dashboard/providers/openai-compatible-fixture"]');
                  const disconnected = page.locator('a[href="/dashboard/providers/anthropic-compatible-fixture"]');
                  await connected.getByText("1 Connected", { exact: true }).waitFor();
                  await disconnected.getByText("No connections", { exact: true }).waitFor();
                  await page.locator('a[href="/dashboard/providers/codex"]').getByText("1 Connected", { exact: true }).waitFor();
                  await page.locator('a[href="/dashboard/providers/openai"]').getByText("1 Connected", { exact: true }).waitFor();
                  if (screen.action === "search-match" || screen.action === "search-empty") {
                    await page.getByPlaceholder("Search providers...").fill(screen.action === "search-match" ? "fIxTuRe" : "no-such-fixture-provider");
                    if (screen.action === "search-match") {
                      await connected.getByText("1 Connected", { exact: true }).waitFor();
                      await disconnected.getByText("No connections", { exact: true }).waitFor();
                      assert.equal(await page.getByRole("heading", { name: "OAuth Providers" }).count(), 0, "search must filter unrelated providers");
                    } else {
                      await page.getByText("No providers match your search", { exact: true }).waitFor();
                      assert.equal(await connected.count(), 0, "empty search must filter fixture providers");
                    }
                  } else if (screen.action === "free-filter") {
                    // A click is required here: Astro removes the checked input immediately.
                    await page.getByLabel("Show only free tier").click();
                    await page.getByRole("heading", { name: "Free Tier Providers" }).waitFor({ state: "detached" });
                    assert.equal(await page.getByLabel("Show only free tier").count(), 0, "Astro removes the filter with its section");
                    assert.equal(await page.locator('a[href="/dashboard/providers/codex"]').count(), 0, "free filter must exclude Codex");
                    await page.locator('a[href="/dashboard/providers/kilocode"]').first().waitFor();
                    await page.locator("main .overflow-y-auto").evaluate((element) => { element.scrollTop = 0; });
                  } else if (screen.action === "scroll-api-keys") {
                    await page.getByRole("heading", { name: "API Key Providers" }).evaluate((element) => element.scrollIntoView({ block: "start", behavior: "instant" }));
                    const scrollTop = await page.locator("main .overflow-y-auto").evaluate((element) => element.scrollTop);
                    assert(scrollTop > 0, "provider list must scroll to the API-key section");
                    assert(await page.locator('a[href="/dashboard/providers/openai"]').evaluate((element) => {
                      const rect = element.getBoundingClientRect();
                      const parent = element.closest(".overflow-y-auto").getBoundingClientRect();
                      return rect.top >= parent.top && rect.bottom <= parent.bottom;
                    }), "connected API-key provider must be in the viewport");
                    await page.getByRole("button", { name: /Show all \d+ providers/ }).waitFor();
                  } else if (screen.action === "show-all-scroll-bottom") {
                    const showAll = page.getByRole("button", { name: /Show all \d+ providers/ });
                    await showAll.click();
                    await showAll.waitFor({ state: "detached" });
                    const lastCard = page.locator('a[href="/dashboard/providers/xiaomi-tokenplan"]');
                    await lastCard.waitFor();
                    const scrollTop = await page.locator("main .overflow-y-auto").evaluate((element) => {
                      element.scrollTop = element.scrollHeight;
                      return element.scrollTop;
                    });
                    assert(scrollTop > 0, "expanded provider list must overflow");
                    assert(await lastCard.evaluate((element) => {
                      const rect = element.getBoundingClientRect();
                      return rect.bottom > 0 && rect.top < window.innerHeight;
                    }), "last expanded provider must be in the viewport");
                  } else if (screen.action === "add-openai-dialog" || screen.action === "add-openai-dialog-scroll-bottom") {
                    await page.getByRole("button", { name: /Add OpenAI Compatible/ }).click();
                    const heading = page.getByRole("heading", { name: "Add OpenAI Compatible", exact: true });
                    await heading.waitFor();
                    assert.equal(await page.getByPlaceholder("OpenAI Compatible (Prod)").inputValue(), "");
                    assert.equal(await page.getByPlaceholder("oc-prod").inputValue(), "");
                    assert.equal(await page.getByRole("combobox").inputValue(), "chat");
                    assert(await page.getByRole("button", { name: "Create", exact: true }).isDisabled());
                    assert(await page.getByRole("button", { name: "Check", exact: true }).isDisabled());
                    if (screen.action === "add-openai-dialog-scroll-bottom") {
                      const body = heading.locator("../../..").locator(".overflow-y-auto");
                      const scrollTop = await body.evaluate((element) => {
                        element.scrollTop = element.scrollHeight;
                        return element.scrollTop;
                      });
                      assert(scrollTop > 0, "compatible dialog must overflow");
                      const createVisible = await page.getByRole("button", { name: "Create", exact: true }).evaluate((element) => {
                        const rect = element.getBoundingClientRect();
                        const parent = element.closest(".overflow-y-auto").getBoundingClientRect();
                        return rect.top >= parent.top && rect.bottom <= parent.bottom;
                      });
                      assert(createVisible, "dialog actions must be visible after scrolling");
                    }
                  }
                  // The list contains both real local PNGs and intentional fallbacks for missing icons.
                  await page.waitForFunction(() => Array.from(document.images).every((image) => image.complete));
                  await page.evaluate(async () => {
                    await Promise.all(Array.from(document.images, (image) => image.decode().catch(() => {})));
                    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
                  });
                } else {
                  if (viewport.name === "desktop") await page.getByText(fixtureCommit.slice(0, 8), { exact: true }).first().waitFor();
                  if (screen.insecure) await page.getByText("Endpoint is exposed without an API key.").waitFor();
                  if (screen.paused) await page.getByText("Paused", { exact: true }).waitFor();
                  if (screen.action === "create" || screen.action === "created") {
                    await page.getByRole("button", { name: /Create Key/ }).click();
                    await page.getByRole("heading", { name: "Create API Key" }).waitFor();
                    if (screen.action === "created") {
                      await page.getByPlaceholder("Production Key").fill("Fixture new");
                      await page.getByRole("button", { name: "Create", exact: true }).click();
                      await page.getByRole("heading", { name: "API Key Created" }).waitFor();
                    }
                  } else if (screen.action === "scroll-bottom") {
                    const scrollTop = await page.locator("main .overflow-y-auto").evaluate((element) => {
                      element.scrollTop = element.scrollHeight;
                      return element.scrollTop;
                    });
                    assert(scrollTop > 0, "scroll-bottom fixture must overflow");
                  } else if (screen.action === "sidebar") {
                    await page.locator("header button").filter({ has: page.getByText("menu", { exact: true }) }).click();
                    await page.getByRole("link", { name: /Providers/ }).waitFor();
                    await page.getByText(fixtureCommit.slice(0, 8), { exact: true }).last().waitFor();
                  } else if (screen.action) {
                    const row = page.getByText("Fixture key", { exact: true }).locator("../..");
                    await row.hover();
                    if (screen.action === "show") {
                      await page.getByTitle("Show key").click();
                      await page.getByText("fixture-token-123", { exact: true }).waitFor();
                    } else if (screen.action === "delete") {
                      await row.getByRole("button", { name: /delete/i }).click();
                      await page.getByRole("heading", { name: "Delete API key" }).waitFor();
                    } else {
                      await row.getByRole("switch").click();
                      await page.getByRole("heading", { name: "Pause API key" }).waitFor();
                    }
                  }
                }

                const image = await page.screenshot({ fullPage: true, animations: "disabled" });
                if (verifyAfterCapture) await verifyAfterCapture();
                scenarioReady = true;
                return image;
              } finally {
                await context.close();
                assert.deepEqual(unexpected, [], "unexpected network requests were blocked");
                if (scenarioReady) assert.deepEqual(writes, expectedWrites, "unexpected API mutation");
                assert.deepEqual(errors, [], "page must not throw");
              }
            };

            const image = await capture();
            const other = mode === "record" ? await capture() : await readFile(join(referenceDir, `${name}.png`));
            const first = join(scratch, `${name}.png`);
            const second = join(scratch, `${name}-second.png`);
            const diff = join(artifactDir, `${name}-diff.png`);
            await writeFile(first, image);
            await writeFile(second, other);
            if (mode === "compare") await writeFile(join(artifactDir, `${name}-actual.png`), image);

            assert.deepEqual([image.readUInt32BE(16), image.readUInt32BE(20)], [other.readUInt32BE(16), other.readUInt32BE(20)], "screenshot dimensions differ");
            const comparison = spawnSync("compare", ["-metric", "AE", first, second, diff], { encoding: "utf8" });
            if (comparison.error) throw comparison.error;
            assert(comparison.status === 0 || comparison.status === 1, `ImageMagick compare failed: ${comparison.stderr}`);
            const differingPixels = Number(comparison.stderr.trim());
            if (mode === "record" && differingPixels !== 0) {
              await writeFile(join(artifactDir, `${name}-first.png`), image);
              await writeFile(join(artifactDir, `${name}-second.png`), other);
            }
            assert.equal(differingPixels, 0, `${name}: differing pixels; see ${diff}`);
            if (screen.section === "callback") {
              // Astro's standalone callback is always dark. Verify both OS color
              // schemes against the same unmasked reference, not duplicate PNGs.
              const dark = await capture("dark");
              assert.deepEqual([dark.readUInt32BE(16), dark.readUInt32BE(20)], [other.readUInt32BE(16), other.readUInt32BE(20)], "dark callback dimensions differ");
              const darkPath = join(scratch, `${name}-dark.png`);
              const darkDiff = join(artifactDir, `${name}-dark-diff.png`);
              await writeFile(darkPath, dark);
              const darkComparison = spawnSync("compare", ["-metric", "AE", darkPath, second, darkDiff], { encoding: "utf8" });
              if (darkComparison.error) throw darkComparison.error;
              assert(darkComparison.status === 0 || darkComparison.status === 1, `ImageMagick compare failed: ${darkComparison.stderr}`);
              const darkPixels = Number(darkComparison.stderr.trim());
              if (darkPixels !== 0) await writeFile(join(artifactDir, `${name}-dark-actual.png`), dark);
              assert.equal(darkPixels, 0, `${name}: dark color scheme differs; see ${darkDiff}`);
              await rm(darkDiff, { force: true });
            }
            if (mode === "record") {
              await rm(diff, { force: true });
              await writeFile(join(referenceDir, `${name}.png`), image);
            }
          });
        }
      }
    }
  } finally {
    await browser.close();
    await rm(scratch, { recursive: true, force: true });
  }
});
