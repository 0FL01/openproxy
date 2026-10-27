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
const translatorFixtures = {
  populated: '{\n  "model": "fixture",\n  "count": 42,\n  "enabled": true\n}',
  wrapped: `{\n  "message": "${"x".repeat(200)}",\n  "count": 42\n}`,
};
const callbackCodeQuery = "?code=fixture-code&state=fixture-state";
const callbackErrorQuery = "?error=fixture-error&error_description=Fixture+authorization+denied&state=fixture-state";
const compatibleId = "openai-compatible-00000000-0000-4000-8000-000000000001";
const compatiblePath = `/dashboard/providers/${compatibleId}`;
const compatibleTemplate = "/dashboard/providers/_dynamic.html";
const compatiblePrefix = "fixture-openai";
const compatibleBaseUrl = "https://fixture.invalid/v1";
const codexPath = "/dashboard/providers/codex";
const codexConnection = "fixture-codex-connection";
const codexOtherConnection = "fixture-other-codex-connection";
const codexConfig = (on, otherOn = false) => ({
  enabled: on || otherOn,
  connections: { [codexConnection]: on, [codexOtherConnection]: otherOn },
});
// Exact shared resources observed from the frozen Astro detail template.
const detailAstroAssets = new Set([
  "/favicon.svg",
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
  { section: "provider-kilocode", name: "device-oauth", action: "device-oauth" },
  { section: "provider-kilocode", name: "device-oauth-error", action: "device-oauth-error" },
  { section: "provider-compatible", name: "ready" },
  { section: "provider-compatible", name: "edit-dialog", action: "edit-dialog" },
  { section: "provider-compatible", name: "edit-dialog-scroll-bottom", action: "edit-dialog-scroll-bottom" },
  { section: "provider-compatible", name: "delete-dialog", action: "delete-dialog" },
  { section: "provider-codex-autoping", name: "off" },
  { section: "provider-codex-autoping", name: "on" },
  { section: "provider-codex-bulk-add", name: "dialog" },
  { section: "provider-codex-oauth", name: "manual" },
  { section: "provider-openrouter", name: "suggested" },
  { section: "provider-xai", name: "ready" },
  { section: "provider-xai", name: "pending" },
  { section: "provider-xai", name: "failure" },
  { section: "provider-xai", name: "stale" },
  { section: "provider-xai", name: "unavailable" },
  { section: "provider-xai", name: "empty" },
  { section: "provider-xai", name: "no-connection" },
  { section: "provider-xai", name: "connection-error" },
  { section: "skills", name: "ready" },
  { section: "skills", name: "copied", action: "copied" },
  { section: "console-log", name: "empty" },
  { section: "console-log", name: "streaming" },
  { section: "profile", name: "security" },
  { section: "profile", name: "appearance" },
  { section: "profile", name: "language" },
  { section: "profile", name: "network" },
  { section: "profile", name: "observability" },
  { section: "profile", name: "database" },
  { section: "profile", name: "database-auth" },
  { section: "profile", name: "ready" },
  { section: "profile", name: "bottom" },
  { section: "db-backups", name: "ready" },
  { section: "db-backups", name: "empty" },
  { section: "db-backups", name: "restore" },
  { section: "db-backups", name: "auth" },
  { section: "logs", name: "ready" },
  { section: "logs", name: "empty" },
  { section: "logs", name: "drawer" },
  { section: "logs", name: "trace" },
  { section: "quota", name: "empty" },
  { section: "quota", name: "ready" },
  { section: "quota", name: "reset-times" },
  { section: "quota", name: "provider-formats" },
  { section: "quota", name: "provider-windows" },
  { section: "quota", name: "deepseek-balance" },
  { section: "quota", name: "kimi-coding" },
  { section: "quota", name: "glm-ping" },
  { section: "quota", name: "codex-ready" },
  { section: "quota", name: "codex-confirm" },
  { section: "quota", name: "codex-expiry" },
  { section: "proxy-pools", name: "empty" },
  { section: "proxy-pools", name: "ready" },
  { section: "proxy-pools", name: "selected" },
  { section: "proxy-pools", name: "add" },
  { section: "translator", name: "ready" },
  { section: "translator", name: "populated" },
  { section: "translator", name: "wrapped" },
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
  const detailCandidateAssets = mode === "compare" ? JSON.parse(process.env.UI_PARITY_DETAIL_ASSETS || process.env.UI_PARITY_COMPATIBLE_ASSETS || "[]") : [];
  assert(Array.isArray(detailCandidateAssets) && detailCandidateAssets.every((path) =>
    typeof path === "string" && /^\/(?:assets|_astro|i18n)\/[a-zA-Z0-9_./-]+$/.test(path) && new URL(path, appUrl).pathname === path),
  "UI_PARITY_DETAIL_ASSETS (or UI_PARITY_COMPATIBLE_ASSETS) must be a JSON array of exact same-origin static paths");
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
    let fixtureCaTrusted = false;
    for (const viewport of viewports) {
      for (const theme of ["light", "dark"]) {
        for (const screen of screens) {
          if (selectedScreen && `${screen.section}-${screen.name}` !== selectedScreen) continue;
          if (selectedSection && screen.section !== selectedSection) continue;
          if (screen.mobileOnly && viewport.name !== "mobile") continue;
          if (screen.desktopOnly && viewport.name !== "desktop") continue;
          if (screen.colorScheme && screen.colorScheme !== theme) continue;
          const name = `${screen.section}-${screen.name}-${viewport.name}-${theme}`;
          const isManualOAuth = screen.section === "provider-codex-oauth";
          const isCodexDetail = ["provider-codex-autoping", "provider-codex-bulk-add", "provider-codex-oauth"].includes(screen.section);
          const isKiloOAuthError = screen.action === "device-oauth-error";
          const isOpenRouter = screen.section === "provider-openrouter";
          const isXaiDetail = screen.section === "provider-xai";
          await t.test(name, async () => {
            assert(mode !== "record" || process.env.UI_PARITY_REPLACE === "1" || !existsSync(join(referenceDir, `${name}.png`)), "reference already exists; set UI_PARITY_REPLACE=1 deliberately to replace it");
            if ((isCodexDetail || isKiloOAuthError) && !fixtureCaTrusted) {
              // route.fetch below must validate the fixture's DNS-constrained CA.
              // Chrome already trusts it via the fixture's disposable NSS profile.
              const tls = await import("node:tls");
              tls.setDefaultCACertificates([...tls.getCACertificates("default"), await readFile("/opt/ui-browser/tls/ca.crt", "utf8")]);
              fixtureCaTrusted = true;
            }
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
              let interceptedCodexStop = 0;
              let deviceAuthAttempts = 0;
              let releaseXaiForce;
              let observeXaiForce;
              const xaiForceStarted = new Promise((resolve) => { observeXaiForce = resolve; });
              let scenarioReady = false;
              let verifyAfterCapture = null;
              const kiloWrites = {
                "disable-model": [{ method: "POST", path: "/api/models/disabled", body: { providerAlias: "kc", ids: ["fixture-standard"] } }],
                "restore-model": [{ method: "DELETE", path: "/api/models/disabled?providerAlias=kc&id=fixture-custom-disabled", body: null }],
                "free-only": [{ method: "PUT", path: "/api/providers/filters", body: { alias: "kc", freeOnly: true } }],
                "add-model-saved": [{ method: "POST", path: "/api/models/custom", body: { providerAlias: "kc", id: "fixture-new-model", type: "llm" } }],
              };
              const codexWrites = screen.name === "on" ? [true, false].map((on) => ({
                method: "PATCH", path: "/api/settings", body: { codexAutoPing: codexConfig(on) },
              })) : [];
              const profileWrites = {
                security: [
                { method: "POST", path: "/api/auth/password", body: { currentPassword: "fixture-current", newPassword: "fixture-new-pass" } },
                { method: "PATCH", path: "/api/settings", body: { requireLogin: false } },
                { method: "PATCH", path: "/api/settings", body: { requireLogin: true } },
                { method: "POST", path: "/api/auth/password", body: { currentPassword: "fixture-current", newPassword: "fixture-new-pass" } },
                ],
                language: [{ method: "POST", path: "/api/locale", body: { locale: "ru" } }],
                network: [
                  { method: "PATCH", path: "/api/settings", body: { outboundProxyEnabled: true } },
                  { method: "POST", path: "/api/settings/proxy-test", body: { proxyUrl: "http://127.0.0.1:7897" } },
                  { method: "PATCH", path: "/api/settings", body: { outboundProxyUrl: "http://127.0.0.1:7897", outboundNoProxy: "localhost" } },
                  { method: "PATCH", path: "/api/settings", body: { outboundProxyEnabled: false } },
                ],
                observability: [true, false].map((enabled) => ({ method: "PATCH", path: "/api/settings", body: { observabilityEnabled: enabled } })),
                database: [
                  { method: "POST", path: "/api/settings/database", body: { metadata: { fixture: true }, password: "fixture-current" } },
                  { method: "POST", path: "/api/shutdown", body: null },
                  { method: "POST", path: "/api/auth/logout", body: {} },
                ],
              };
              const backupWrites = screen.name === "ready" ? [
                { method: "PUT", path: "/api/db-backups", body: null },
                { method: "POST", path: "/api/db-backups/restore", body: { backupId: "fixture-1" } },
                { method: "DELETE", path: "/api/db-backups/fixture-1", body: null },
                { method: "DELETE", path: "/api/db-backups", body: {} },
                { method: "POST", path: "/api/db-backups/import", body: { filename: "fixture.json", content: '{"fixture":true}', password: "fixture-current" } },
              ] : [];
              const expectedWrites = screen.section === "translator" && translatorFixtures[screen.name] ? [{ method: "POST", path: "/api/translator/translate", body: { step: 1, body: JSON.parse(translatorFixtures[screen.name]) } }] : screen.section === "quota" && screen.name === "codex-ready" ? Array.from({ length: 2 }, () => ({ method: "POST", path: "/api/usage/fixture-codex/codex-reset-credits", body: null })) : screen.section === "quota" && screen.name === "glm-ping" ? [true, false, true].map((enabled) => ({ method: "PATCH", path: "/api/settings", body: { glmAutoPing: { enabled: true, connections: { "other-glm": true, "fixture-glm": enabled } } } })) : screen.section === "quota" && screen.name === "ready" ? [false, true].map((isActive) => ({ method: "PUT", path: "/api/providers/fixture-github", body: { isActive } })) : screen.section === "db-backups" ? backupWrites : screen.section === "profile" ? (profileWrites[screen.name] || []) : screen.section === "console-log" && screen.name === "streaming" ? [{ method: "DELETE", path: "/api/translator/console-logs", body: null }] : isOpenRouter ? [{ method: "POST", path: "/api/models/custom", body: { providerAlias: "openrouter", id: "sample/model:free", type: "llm" } }] : screen.section === "provider-kilocode" ? (kiloWrites[screen.action] || []) : screen.section === "provider-codex-autoping" ? codexWrites : screen.login
                ? [{ method: "POST", path: "/api/auth/login", body: { password: "fixture-password" } }]
                : screen.action === "created"
                  ? [{ method: "POST", path: "/api/keys", body: { name: "Fixture new" } }]
                  : [];
              try {
                const page = await context.newPage();
                if (screen.section === "profile" || screen.section === "db-backups") page.setDefaultTimeout(10000);
                if (screen.section === "skills") {
                  await page.addInitScript(() => {
                    Object.defineProperty(navigator.clipboard, "writeText", {
                      configurable: true,
                      value: async (value) => { window.__fixtureCopied = value; },
                    });
                  });
                }
                if (screen.section === "console-log") {
                  await page.addInitScript(() => {
                    const NativeEventSource = window.EventSource;
                    window.EventSource = class extends NativeEventSource {
                      constructor(...args) { super(...args); window.__consoleSource = this; }
                    };
                  });
                }
                if (screen.action === "device-oauth" || isKiloOAuthError) await page.addInitScript(() => {
                  window.open = (...args) => { window.__fixtureOpened = args; return null; };
                });
                if (isManualOAuth) await page.addInitScript(() => {
                  // Model an open popup without allowing navigation or network access.
                  window.open = (...args) => {
                    window.__fixtureOpened = args;
                    window.__fixturePopup = { opener: null, closed: false, location: { href: "", origin: "https://fixture.invalid", pathname: "/authorize", search: "" }, close() { this.closed = true; } };
                    return window.__fixturePopup;
                  };
                });
                if (screen.section === "quota") await page.addInitScript(() => localStorage.setItem("quotaAutoRefresh", "false"));
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
                let profileSettings = { requireLogin: true, hasPassword: true, outboundProxyEnabled: false, outboundProxyUrl: "", outboundNoProxy: "", observabilityEnabled: false };
                let profilePasswords = 0;
                let profileExports = [];
                let backupExports = [];
                let backups = screen.name === "empty" ? [] : [{ id: "fixture-1", filename: "fixture.json", createdAt: "2026-10-01T10:00:00Z", size: 2048, reason: "manual", providerCount: 2, apiKeyCount: 1 }];
                let kiloDisabled = ["fixture-custom-disabled", "fixture-disabled"];
                let kiloCustom = [
                  { providerAlias: "kc", id: "fixture-custom", name: "Fixture Custom", type: "llm" },
                  { providerAlias: "kc", id: "fixture-custom-disabled", name: "Fixture Disabled Custom", type: "llm" },
                ];
                let kiloFreeOnly = false;
                let routerCustom = [];
                let quotaConnections = ["ready", "reset-times"].includes(screen.name) ? [{ id: "fixture-github", provider: "github", name: "Fixture account", authType: "oauth", isActive: true }] : screen.name === "provider-formats" ? [{ id: "fixture-commandcode", provider: "commandcode", name: "Fixture account", email: "fixture@example.invalid", authType: "apikey", isActive: true }, { id: "fixture-a6", provider: "a6api", name: "Fixture A6", authType: "apikey", isActive: true }] : screen.name === "provider-windows" ? [{ id: "fixture-antigravity", provider: "antigravity", name: "Fixture account", authType: "oauth", isActive: true }, { id: "fixture-kimi", provider: "kimi", name: "Fixture Kimi", authType: "oauth", isActive: true }, { id: "fixture-claude", provider: "claude", name: "Fixture Claude", authType: "oauth", isActive: true }] : screen.name === "deepseek-balance" ? [{ id: "fixture-deepseek", provider: "deepseek", name: "Fixture account", authType: "apikey", isActive: true }] : screen.name === "kimi-coding" ? [{ id: "fixture-kimi-coding", provider: "kimi-coding", name: "Fixture account", authType: "oauth", isActive: true }] : screen.name === "glm-ping" ? [{ id: "fixture-glm", provider: "glm", name: "Fixture account", authType: "apikey", isActive: true }] : screen.section === "quota" && screen.name.startsWith("codex-") ? [{ id: "fixture-codex", provider: "codex", name: "Fixture account", authType: "oauth", isActive: true }] : [];
                let codexCredits = 1;
                let creditGetError = false;
                let creditPostError = false;
                let releaseCreditLookup;
                let markCreditLookupStarted;
                const creditLookupStarted = new Promise(resolve => { markCreditLookupStarted = resolve; });
                let releaseCodexReset;
                let markCodexResetStarted;
                const codexResetStarted = new Promise(resolve => { markCodexResetStarted = resolve; });
                let glmConfig = { enabled: true, connections: { "other-glm": true } };
                let quotaReads = 0;
                let quotaStatus = 200;
                let quotaClaudeUnauthorized = false;
                let quotaAntigravityOrder = false;
                const proxyPools = screen.name === "empty" ? [] : [
                  { id: "fixture-one", name: "Fixture Proxy", proxyUrl: "http://fixture.example.invalid:8080/", noProxy: "localhost", isActive: true, testStatus: "active", boundConnectionCount: 2, lastTestedAt: null },
                  { id: "fixture-two", name: "Disabled Proxy", proxyUrl: "http://other.example.invalid:8080/", noProxy: "", isActive: false, testStatus: "unknown", boundConnectionCount: 0, lastTestedAt: null },
                ];
                // The off case demonstrates that another enabled connection does
                // not turn this connection on; the on case exercises both writes.
                let currentCodexConfig = codexConfig(false, screen.name === "off");
                page.on("pageerror", (error) => errors.push(error.message));
                await page.clock.setFixedTime(new Date("2026-10-01T12:00:00Z"));
                if (["callback", "provider-compatible", "logs", "quota"].includes(screen.section) || isCodexDetail || isKiloOAuthError || isOpenRouter || isXaiDetail) await context.routeWebSocket("**/*", (socket) => {
                  unexpected.push("WebSocket blocked");
                  socket.close();
                });
                await context.route("**/*", async (route) => {
                  const request = route.request();
                  const url = new URL(request.url());
                  const path = url.pathname;
                  const method = request.method();
                  if (screen.section === "translator" && mode === "record" && method === "GET" && url.origin === "https://cdn.jsdelivr.net" && path.startsWith("/npm/monaco-editor@0.55.1/min/vs/") && !url.search) {
                    const directory = resolve(process.env.UI_MONACO_VS_DIR || "/monaco-vs");
                    const file = resolve(directory, path.slice("/npm/monaco-editor@0.55.1/min/vs/".length));
                    if (file.startsWith(directory + "/") && existsSync(file)) return route.fulfill({ path: file });
                  }
                  const assets = screen.section === "callback"
                    ? ((path === "/callback" && url.search === screen.query) || (mode === "compare" && !url.search && (path === "/favicon.svg" || path.startsWith("/_astro/") || path.startsWith("/assets/"))))
                    : screen.section === "provider-compatible" || isCodexDetail || isKiloOAuthError || isOpenRouter || isXaiDetail
                    ? (!url.search && (path === (screen.section === "provider-compatible" ? compatiblePath : isKiloOAuthError ? "/dashboard/providers/kilocode" : isOpenRouter ? "/dashboard/providers/openrouter" : isXaiDetail ? "/dashboard/providers/xai" : codexPath) || path === (screen.section === "provider-compatible" ? "/providers/oai-cc.png" : isKiloOAuthError ? "/providers/kilocode.png" : isOpenRouter ? "/providers/openrouter.png" : isXaiDetail ? "/providers/xai.png" : "/providers/codex.png") || detailAstroAssets.has(path) || (mode === "compare" && detailCandidateAssets.includes(path))))
                    : (!["providers-new", "provider-kilocode"].includes(screen.section) && (path === "/login" || path === "/dashboard/endpoint")) || path === "/favicon.svg" || path.startsWith("/_astro/") || path.startsWith("/assets/") || path.startsWith("/i18n/") ||
                    (screen.section === "providers" && (path === "/dashboard/providers" || /^\/providers\/[a-z0-9_-]+\.png$/.test(path))) ||
                    (screen.section === "providers-new" && !url.search && ["/dashboard/providers/new", "/providers/codex.png", "/providers/opencode-zen.png", "/providers/openai.png"].includes(path)) ||
                    (screen.section === "provider-kilocode" && !url.search && ["/dashboard/providers/kilocode", "/providers/kilocode.png"].includes(path)) ||
                    (screen.section === "skills" && !url.search && path === "/dashboard/skills") ||
                    (screen.section === "console-log" && !url.search && path === "/dashboard/console-log") ||
                    (screen.section === "profile" && !url.search && path === "/dashboard/profile") ||
                    (screen.section === "db-backups" && !url.search && path === "/dashboard/db-backups") ||
                    (screen.section === "logs" && !url.search && path === "/dashboard/logs") ||
                    (screen.section === "quota" && !url.search && ["/dashboard/quota", "/providers/github.png", "/providers/glm.png", "/providers/codex.png", "/providers/commandcode.png", "/providers/a6api.png", "/providers/antigravity.png", "/providers/kimi.png", "/providers/kimi-coding.png", "/providers/claude.png", "/providers/deepseek.png"].includes(path)) ||
                    (screen.section === "proxy-pools" && !url.search && path === "/dashboard/proxy-pools") ||
                    (screen.section === "translator" && !url.search && path === "/dashboard/translator");
                  if (url.origin === appUrl && method === "GET" && assets) {
                    if (!isCodexDetail && !isKiloOAuthError) return route.continue();
                    // Do not let even an allowed asset redirect around the
                    // network allowlist (especially to a side-effecting GET).
                    const response = await route.fetch({ maxRedirects: 0 });
                    if (response.status() >= 300 && response.status() < 400) {
                      unexpected.push(`blocked asset redirect ${method} ${url.origin}${path}${url.search}`);
                      return route.abort("blockedbyclient");
                    }
                    return route.fulfill({ response });
                  }
                  const providerGet = screen.section === "providers" && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true")
                  );
                  const providerNewGet = screen.section === "providers-new" && method === "GET" && !url.search &&
                    ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog"].includes(path);
                  const kiloGet = screen.section === "provider-kilocode" && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/kilo/free-models", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites"].includes(path)) ||
                    ((screen.action === "device-oauth" || isKiloOAuthError) && !url.search && path === "/api/oauth/kilocode/device-code") ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", "?providerAlias=kc"].includes(url.search))
                  );
                  const compatibleGet = screen.section === "provider-compatible" && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", `?providerAlias=${encodeURIComponent(compatibleId)}`].includes(url.search))
                  );
                  const codexGet = isCodexDetail && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites", "/api/oauth/codex/stop-proxy"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", "?providerAlias=cx"].includes(url.search)) ||
                    (isManualOAuth && path === "/api/oauth/codex/authorize" && [...url.searchParams].length === 1 && url.searchParams.get("redirect_uri") === "http://localhost:1455/auth/callback") ||
                    (isManualOAuth && path === "/api/oauth/codex/start-proxy" && [...url.searchParams].length === 4 && url.searchParams.get("app_port") === "4623" && url.searchParams.get("state") === "fixture-state" && url.searchParams.get("code_verifier") === "fixture-verifier" && url.searchParams.get("redirect_uri") === "http://localhost:1455/auth/callback")
                  );
                  const routerGet = isOpenRouter && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", "?providerAlias=openrouter"].includes(url.search)) ||
                    (path === "/api/providers/suggested-models" && url.searchParams.get("url") === "https://openrouter.ai/api/v1/models" && url.searchParams.get("type") === "openrouter-free" && [...url.searchParams].length === 2)
                  );
                  const xaiGet = isXaiDetail && method === "GET" && (
                    (!url.search && ["/api/auth/status", "/api/settings", "/api/build", "/api/catalog", "/api/providers", "/api/provider-nodes", "/api/providers/filters", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites", "/api/oauth/xai/stop-proxy", "/api/providers/fixture-xai/models"].includes(path)) ||
                    (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
                    (path === "/api/models/disabled" && ["?providerAlias=", "?providerAlias=xai"].includes(url.search))
                  );
                  const kiloMutation = screen.section === "provider-kilocode" && method !== "GET" &&
                    expectedWrites.some((write) => write.method === method && write.path === `${path}${url.search}`);
                  const codexMutation = screen.section === "provider-codex-autoping" && method !== "GET" &&
                    expectedWrites.some((write) => write.method === method && write.path === `${path}${url.search}`);
                  const routerMutation = isOpenRouter && method === "POST" && !url.search && path === "/api/models/custom";
                  const consoleRequest = screen.section === "console-log" && !url.search && ((method === "GET" && path === "/api/translator/console-logs/stream") || (method === "DELETE" && path === "/api/translator/console-logs" && screen.name === "streaming"));
                  const profileMutation = screen.section === "profile" && !url.search && ((method === "POST" && ["/api/auth/password", "/api/auth/logout", "/api/locale", "/api/settings/proxy-test", "/api/settings/database", "/api/shutdown"].includes(path)) || (method === "PATCH" && path === "/api/settings"));
                  const profileExport = screen.section === "profile" && screen.name === "database" && method === "GET" && !url.search && path === "/api/settings/database";
                  const backupRequest = screen.section === "db-backups" && !url.search && ((method === "GET" && ["/api/db-backups", "/api/db-backups/export"].includes(path)) || (screen.name === "ready" && ((method === "PUT" && path === "/api/db-backups") || (method === "DELETE" && ["/api/db-backups", "/api/db-backups/fixture-1"].includes(path)) || (method === "POST" && ["/api/db-backups/restore", "/api/db-backups/import"].includes(path)))));
                  const logRequest = screen.section === "logs" && method === "GET" && ["/api/request-logs", "/api/request-logs/tps-stats"].includes(path) && [...url.searchParams.keys()].every((key) => ["page", "pageSize", "apiKeyId", "status", "provider", "model", "hiddenProviders"].includes(key));
                  const quotaRequest = screen.section === "quota" && ((method === "GET" && ((!url.search && ["/api/providers", "/api/usage/fixture-github", "/api/usage/fixture-glm", "/api/usage/fixture-codex", "/api/usage/fixture-commandcode", "/api/usage/fixture-a6", "/api/usage/fixture-antigravity", "/api/usage/fixture-kimi", "/api/usage/fixture-kimi-coding", "/api/usage/fixture-claude", "/api/usage/fixture-deepseek", "/api/usage/fixture-codex/codex-reset-credits"].includes(path)) || (path === "/api/proxy-pools" && url.search === "?isActive=true"))) || (method === "POST" && !url.search && path === "/api/usage/fixture-codex/codex-reset-credits" && screen.name === "codex-ready") || (method === "PUT" && !url.search && path === "/api/providers/fixture-github") || (screen.name === "glm-ping" && method === "PATCH" && !url.search && path === "/api/settings"));
                  const proxyPoolsRequest = screen.section === "proxy-pools" && method === "GET" && path === "/api/proxy-pools" && url.search === "?includeUsage=true";
                  const translatorRequest = screen.section === "translator" && !!translatorFixtures[screen.name] && method === "POST" && path === "/api/translator/translate" && !url.search;
                  const valid = url.origin === appUrl && (providerGet || providerNewGet || kiloGet || kiloMutation || compatibleGet || codexGet || codexMutation || routerGet || routerMutation || xaiGet || consoleRequest || profileMutation || profileExport || backupRequest || logRequest || quotaRequest || proxyPoolsRequest || translatorRequest || (!isCodexDetail && !["providers", "providers-new", "provider-kilocode", "provider-compatible", "provider-openrouter", "provider-xai", "callback"].includes(screen.section) && (
                    (method === "GET" && ["/api/auth/status", "/api/settings", "/api/keys", "/api/build", "/api/catalog"].includes(path)) ||
                    (method === "POST" && path === "/api/auth/login" && !!screen.login) ||
                    (method === "POST" && path === "/api/keys" && screen.action === "created")
                  )));
                  if (!valid) {
                    unexpected.push(`${method} ${url.origin}${path}${url.search}`);
                    return route.abort("blockedbyclient");
                  }
                  if (method !== "GET") {
                    let requestBody;
                    try {
                      if (screen.section === "db-backups" && path === "/api/db-backups/import") {
                        const raw = request.postData() || "";
                        assert.match(request.headers()["content-type"], /^multipart\/form-data; boundary=/);
                        assert.match(raw, /name="file"; filename="fixture.json"/);
                        assert.match(raw, /\{"fixture":true\}/);
                        requestBody = { filename: "fixture.json", content: '{"fixture":true}', password: request.headers()["x-op-password"] };
                      } else requestBody = request.postDataJSON();
                    }
                    catch {
                      unexpected.push(`non-JSON mutation ${method} ${url.origin}${path}${url.search}`);
                      return route.abort("blockedbyclient");
                    }
                    const write = { method, path: ["provider-kilocode", "provider-codex-autoping"].includes(screen.section) ? `${path}${url.search}` : path, body: requestBody };
                    if ((["provider-kilocode", "provider-codex-autoping", "console-log", "profile", "db-backups", "quota", "translator"].includes(screen.section) || isOpenRouter) && !isDeepStrictEqual(write, expectedWrites[writes.length])) {
                      unexpected.push(`unexpected mutation ${method} ${url.origin}${path}${url.search}`);
                      return route.abort("blockedbyclient");
                    }
                    writes.push(write);
                  }
                  let status = 200;
                  let body = {};
                  const headers = {};
                  if (translatorRequest) return route.fulfill({ json: { success: false, error: "Synthetic detection failure" } });
                  if (isManualOAuth && path === "/api/oauth/codex/authorize") return route.fulfill({ json: { authUrl: "https://fixture.invalid/authorize?client=synthetic", state: "fixture-state", codeVerifier: "fixture-verifier" } });
                  if (isManualOAuth && path === "/api/oauth/codex/start-proxy") return route.fulfill({ json: { success: true, serverSide: false } });
                  if (isKiloOAuthError && path === "/api/oauth/kilocode/device-code") return route.fulfill({ status: 503, json: { error: ++deviceAuthAttempts === 1 ? "Synthetic device failure" : "Synthetic retry failure" } });
                  if (screen.action === "device-oauth" && path === "/api/oauth/kilocode/device-code") return route.fulfill({ json: {
                    device_code: "fixture-device", user_code: "ABCD-EFGH", verification_uri: "https://fixture.example.invalid/login",
                    verification_uri_complete: "https://fixture.example.invalid/login?user_code=ABCD-EFGH", interval: 300, expires_in: 1200,
                  } });
                  if (path === "/api/providers/fixture-xai/models") {
                    if (screen.name === "pending") await new Promise((resolve) => { releaseXaiForce = resolve; observeXaiForce(); });
                    return route.fulfill({ status: 503, json: { error: "Synthetic discovery failure" } });
                  }
                  if (screen.section === "quota" && path === "/api/usage/fixture-github") { quotaReads += 1; if (quotaStatus !== 200) return route.fulfill({ status: quotaStatus, json: { error: "Synthetic temporary outage" } }); return route.fulfill({ json: { plan: "pro", quotas: screen.name === "reset-times" ? { "Today": { used: 25, total: 100, resetAt: "2026-10-01T16:30:00Z" }, "Tomorrow": { used: 50, total: 100, resetAt: "2026-10-02T15:00:00Z", recurring: false }, "Expired": { used: 100, total: 100, resetAt: "2026-09-29T09:00:00Z" } } : { "Premium Requests": { used: 25, total: 100, resetAt: null } } } }); }
                  if (screen.section === "quota" && path === "/api/usage/fixture-commandcode") return route.fulfill({ json: { status: "partial", message: "Some Command Code credit fields are unavailable.", quotas: { monthly: { kind: "balance", label: "Monthly remaining", remaining: 1234.5, unit: "credit value" }, weekly: { kind: "window", label: "Weekly", used: 30, total: 100, remaining: 70, remainingPercentage: 70, resetAt: null, unit: "credit value" } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-a6") return route.fulfill({ json: { status: "available", message: "", quotas: { "API credits": { used: 0, total: 0, remaining: 0, remainingPercentage: 100, unit: "USD", unlimited: true, recurring: false } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-antigravity") return route.fulfill({ json: { plan: "pro", quotas: quotaAntigravityOrder ? {
                    "claude-sonnet-4-6": { displayName: "Alpha", used: 25, total: 100, remainingPercentage: 75 },
                    "gemini-3.7-flash-high": { displayName: "Zulu", used: 60, total: 100, remainingPercentage: 40 },
                  } : { "claude-sonnet-4-6": { displayName: "Claude Sonnet", used: 0, total: 100, remainingPercentage: 35, resetAt: "2026-10-02T15:00:00Z" } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-kimi") return route.fulfill({ json: { plan: "standard", quotas: { weekly: { used: 40, total: 100, remainingPercentage: 60, resetAt: null, unlimited: false } } } });
                  // fetch_kimi_oauth_usage -> kimi_make_quota -> OAuth /api/usage envelope.
                  if (screen.section === "quota" && path === "/api/usage/fixture-kimi-coding") return route.fulfill({ json: { connectionId: "fixture-kimi-coding", message: "", plan: "Kimi Coding", quotas: { Weekly: { used: 40, total: 100, remainingPercentage: 60, resetAt: null, unlimited: false } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-claude") return quotaClaudeUnauthorized ? route.fulfill({ status: 401, json: { error: "Synthetic authorization error" } }) : route.fulfill({ json: { message: "Observed from synthetic traffic", quotas: { session: { used: 10, total: 100, remainingPercentage: 90, resetAt: null } } } });
                  // Mirrors fetch_deepseek_usage and the API-key /api/usage envelope.
                  if (screen.section === "quota" && path === "/api/usage/fixture-deepseek") return route.fulfill({ json: { connectionId: "fixture-deepseek", message: "", quotas: { "Balance (CNY)": { used: 0, total: 0, remainingPercentage: 0, resetAt: null, unlimited: false }, "Balance (USD)": { used: 0, total: 12.5, remainingPercentage: 100, resetAt: null, unlimited: true } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-glm") return route.fulfill({ json: { plan: "pro", quotas: { "Premium Requests": { used: 25, total: 100, resetAt: null } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-codex") return route.fulfill({ json: { plan: "pro", resetCredits: { availableCount: codexCredits }, quotas: { "Premium Requests": { used: 25, total: 100, resetAt: null } } } });
                  if (screen.section === "quota" && path === "/api/usage/fixture-codex/codex-reset-credits") {
                    if (method === "POST") { if (creditPostError) return route.fulfill({ status: 429, json: { error: "Synthetic reset error" } }); markCodexResetStarted(); await new Promise(resolve => { releaseCodexReset = resolve; }); codexCredits = 0; return route.fulfill({ json: { reset: true } }); }
                    if (creditGetError) { markCreditLookupStarted(); await new Promise(resolve => { releaseCreditLookup = resolve; }); return route.fulfill({ status: 503, json: { error: "Synthetic credit error" } }); }
                    return route.fulfill({ json: { availableCount: codexCredits, credits: [{ status: "available", grantedAt: null, expiresAt: null }, { status: "used", grantedAt: null, expiresAt: null }] } });
                  }
                  if (screen.section === "quota" && path === "/api/providers/fixture-github") { quotaConnections[0].isActive = request.postDataJSON().isActive; return route.fulfill({ json: { success: true } }); }
                  if (isXaiDetail && path === "/api/providers" && screen.name === "connection-error") {
                    return route.fulfill({ status: 503, json: { error: "Synthetic connection failure" } });
                  }
                  if (path === "/api/translator/console-logs/stream") {
                    const logs = screen.name === "streaming" ? Array.from({ length: 201 }, (_, index) => `[fixture] [INFO] line ${index}`) : [];
                    const events = [{ type: "init", logs }, ...(screen.name === "streaming" ? [{ type: "line", line: "[fixture] [ERROR] latest line" }] : [])];
                    return route.fulfill({ status: 200, headers: { "content-type": "text/event-stream" }, body: `retry: 15000\n\n${events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("")}` });
                  }
                  if (screen.section === "db-backups" && path === "/api/db-backups/export") {
                    const pass = request.headers()["x-op-password"];
                    backupExports.push(pass);
                    if (pass !== "fixture-current") return route.fulfill({ status: 401, json: { error: "Invalid password" } });
                    return route.fulfill({ status: 200, headers: { "content-type": "application/json", "content-disposition": 'attachment; filename="fixture-export.json"' }, body: '{"fixture":true}' });
                  }
                  if (screen.section === "db-backups" && path === "/api/db-backups") {
                    if (method === "GET") return route.fulfill({ json: { backups, maxFiles: 24, retentionDays: 7, autoDisabled: false } });
                    if (method === "PUT") return route.fulfill({ json: { created: true, backup: { id: "fixture-new" } } });
                    return route.fulfill({ json: { result: { deletedFiles: 0, keptFiles: 1 } } });
                  }
                  if (screen.section === "db-backups" && path === "/api/db-backups/fixture-1") { backups = []; return route.fulfill({ json: { deleted: true } }); }
                  if (screen.section === "db-backups" && path === "/api/db-backups/restore") return route.fulfill({ json: { restored: true, providerCount: 2, apiKeyCount: 1 } });
                  if (screen.section === "db-backups" && path === "/api/db-backups/import") return route.fulfill({ json: { imported: true, providerCount: 2, apiKeyCount: 1 } });
                  if (logRequest) {
                    const size = Number(url.searchParams.get("pageSize"));
                    const page = Number(url.searchParams.get("page"));
                    assert([10, 20, 50].includes(size) && page >= 1);
                    const records = screen.name === "empty" ? [] : [
                      { requestId: "request-1", timestamp: "2026-10-01T11:30:00Z", route: "/v1/chat/completions", status: "success", statusCode: 200, model: "fixture-model", provider: "fixture-provider", durationMs: 1800, inputTokens: 1000, outputTokens: 240, cachedTokens: 50, reasoningTokens: 20, totalTokens: 1240, tokensPerSecond: 20, generatedOutputTokens: 240, upstreamDurationMs: 12000, apiKeyId: "fixture", apiKeyName: "Fixture key" },
                      { requestId: "request-2", timestamp: "2026-10-01T11:20:00Z", route: "/v1/responses", status: "error", statusCode: 429, errorKind: "rate_limit", errorMessage: "Synthetic failure", model: "second-model", provider: "other-provider", durationMs: 25, tokensPerSecond: null, generatedOutputTokens: null, upstreamDurationMs: null, ...(screen.name === "trace" ? { errorCode: "fixture_limit", streamTrace: { version: 1, stopReason: "error", finishReason: "failed", completedCount: 0, errorCount: 1, framesAfterCompleted: 0, doneSent: false, overflowed: 0, toolNames: ["fixture_tool"], upstreamEvents: { "response.error": 1 }, emittedEvents: { error: 1 }, itemTypes: { function_call: 1 }, entries: ["response.error", "error"] } } : {}) },
                    ];
                    if (path === "/api/request-logs") {
                      const filtered = records.filter((r) => (!url.searchParams.get("status") || r.status === url.searchParams.get("status")) && (!url.searchParams.get("provider") || r.provider.includes(url.searchParams.get("provider"))) && (!url.searchParams.get("model") || r.model.includes(url.searchParams.get("model"))) && (!url.searchParams.get("apiKeyId") || r.apiKeyId === url.searchParams.get("apiKeyId")));
                      return route.fulfill({ json: { requests: filtered.slice((page - 1) * size, page * size), pagination: { page, pageSize: size, totalItems: filtered.length, totalPages: Math.ceil(filtered.length / size) } } });
                    }
                    const hidden = JSON.parse(url.searchParams.get("hiddenProviders") || "[]");
                    const providers = records.length ? ["fixture-provider"] : [];
                    const observation = { medianTps: 20, sampleCount: 1, successfulAttemptCount: 1 };
                    const groups = providers.filter((id) => !hidden.includes(id)).map((provider) => ({ provider, model: "fixture-model", windows: { "1h": observation, "6h": observation, "24h": observation } }));
                    const bounds = { start: "2026-10-01T11:00:00Z", end: "2026-10-01T12:00:00Z" };
                    return route.fulfill({ json: { asOf: "2026-10-01T12:00:00Z", windows: { "1h": bounds, "6h": bounds, "24h": bounds }, availableProviders: providers, groups, pagination: { page, pageSize: size, totalItems: groups.length, totalPages: Math.ceil(groups.length / size) } } });
                  }
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
                  } else if (path === "/api/auth/password") {
                    profilePasswords += 1;
                    if (profilePasswords === 2) authenticated = false;
                    body = { success: true, sessionsInvalidated: profilePasswords === 2 };
                  } else if (profileExport) {
                    const password = request.headers()["x-op-password"];
                    profileExports.push(password);
                    if (password !== "fixture-current") { status = 401; body = { error: "Invalid password" }; }
                    else body = { metadata: { fixture: true } };
                  } else if (path === "/api/settings/database") {
                    body = { success: true };
                  } else if (path === "/api/auth/logout") {
                    authenticated = false;
                    body = { success: true };
                  } else if (path === "/api/shutdown") {
                    body = { success: true };
                  } else if (path === "/api/locale") {
                    headers["set-cookie"] = "locale=ru; Path=/; Secure; SameSite=Lax";
                    body = { success: true };
                  } else if (path === "/api/settings/proxy-test") {
                    body = { ok: true, status: "connected", elapsedMs: 12 };
                  } else if (path === "/api/settings") {
                    if (screen.section === "profile" && method === "PATCH") Object.assign(profileSettings, request.postDataJSON());
                    if (screen.section === "provider-codex-autoping" && method === "PATCH") currentCodexConfig = request.postDataJSON().codexAutoPing;
                    if (screen.section === "quota" && screen.name === "glm-ping" && method === "PATCH") {
                      if (writes.length === 3) return route.fulfill({ status: 503, json: { error: "Synthetic settings failure" } });
                      glmConfig = request.postDataJSON().glmAutoPing;
                    }
                    body = { requireLogin: true, requireApiKey: !screen.insecure, hasPassword: true, ...(screen.section === "translator" ? { enableTranslator: true } : {}), ...(screen.section === "profile" ? profileSettings : {}),
                      ...(isCodexDetail ? { codexAutoPing: currentCodexConfig, glmAutoPing: { enabled: false, connections: {} } } : {}),
                      ...(screen.section === "quota" && screen.name === "glm-ping" ? { codexAutoPing: { enabled: false, connections: {} }, glmAutoPing: glmConfig } : {}) };
                  } else if (path === "/api/build") {
                    body = { commit: ["provider-kilocode", "provider-compatible"].includes(screen.section) || isCodexDetail || isOpenRouter || isXaiDetail ? null : fixtureCommit };
                  } else if (path === "/api/catalog") {
                    body = screen.section === "provider-kilocode"
                      ? { providerIdToAlias: { kilocode: "kc" }, providerModels: [{ alias: "kc", models: [
                        { id: "fixture-standard", name: "Fixture Standard", kind: "llm", contextWindow: 128000 },
                        { id: "fixture-vision", name: "Fixture Vision", kind: "llm", contextWindow: 256000 },
                        { id: "fixture-disabled", name: "Fixture Disabled", kind: "llm" },
                      ] }] }
                      : isXaiDetail ? { providerIdToAlias: { xai: "xai" }, providerModels: [{ alias: "xai", models: ["empty", "unavailable", "no-connection"].includes(screen.name) ? [{ id: "fixture-image", name: "Fixture Image", kind: "image" }] : [{ id: "fixture-model", name: "Fixture Model", kind: "llm" }] }], ...(screen.name === "no-connection" ? {} : { xaiCatalog: screen.name === "stale" ? { status: "stale", warning: "Synthetic stale catalog warning" } : screen.name === "unavailable" ? { status: "unavailable", warning: "Synthetic unavailable catalog warning" } : { status: "ready" } }) }
                      : isOpenRouter ? { providerIdToAlias: { openrouter: "openrouter" }, providerModels: [{ alias: "openrouter", models: [{ id: "sample/catalog", name: "Catalog model", kind: "llm" }] }], xaiCatalog: { status: "ready" } }
                      : screen.section === "quota" && screen.name === "provider-windows" ? { providerIdToAlias: { antigravity: "ag" }, providerModels: [{ alias: "ag", models: [{ id: "gemini-3.7-flash-high" }, { id: "claude-sonnet-4-6" }] }] }
                      : isCodexDetail ? { providerIdToAlias: { codex: "cx" }, providerModels: [{ alias: "cx", models: [
                        { id: "fixture-model", name: "Fixture Model", kind: "llm" },
                      ] }], xaiCatalog: { status: "ready" } }
                      : { providerIdToAlias: {}, providerModels: [], xaiCatalog: { status: "ready" } };
                  } else if (path === "/api/providers") {
                    body = screen.section === "quota" ? { connections: quotaConnections } : isXaiDetail ? { connections: screen.name === "no-connection" ? [] : [{ id: "fixture-xai", provider: "xai", name: "Fixture xAI", authType: "apikey", testStatus: "active", isActive: true, priority: 1 }] } : screen.section === "provider-compatible" || isOpenRouter ? { connections: [] } : screen.section === "provider-kilocode" ? { connections: [
                      { id: "fixture-kilo-connection", provider: "kilocode", name: "Fixture connection", authType: "oauth", testStatus: "active", isActive: true, priority: 1 },
                    ] } : isCodexDetail ? { connections: [
                      { id: codexConnection, provider: "codex", name: "Fixture Codex", authType: "oauth", testStatus: "success", isActive: true, priority: 1 },
                    ] } : { connections: [
                      { id: "fixture-compatible-connection", provider: "openai-compatible-fixture", authType: "apikey", testStatus: "active", isActive: true },
                      { id: "fixture-codex-connection", provider: "codex", authType: "oauth", testStatus: "success", isActive: true },
                      { id: "fixture-openai-connection", provider: "openai", authType: "apikey", testStatus: "active", isActive: true },
                    ] };
                  } else if (path === "/api/provider-nodes") {
                    body = screen.section === "provider-compatible" ? { nodes: [{
                      id: compatibleId, type: "openai-compatible", name: "Fixture Compatible", prefix: compatiblePrefix,
                      apiType: "chat", baseUrl: compatibleBaseUrl, createdAt: "2026-10-01T12:00:00Z", updatedAt: "2026-10-01T12:00:00Z",
                    }] } : screen.section === "provider-kilocode" || isCodexDetail || isOpenRouter || isXaiDetail ? { nodes: [] } : { nodes: [
                      { id: "openai-compatible-fixture", type: "openai-compatible", name: "Fixture Connected", apiType: "chat" },
                      { id: "anthropic-compatible-fixture", type: "anthropic-compatible", name: "Fixture Disconnected" },
                    ] };
                  } else if (path === "/api/proxy-pools") {
                    body = { proxyPools: screen.section === "proxy-pools" ? proxyPools : [] };
                  } else if (path === "/api/oauth/codex/stop-proxy" || path === "/api/oauth/xai/stop-proxy") {
                    // Astro's closed OAuth modal calls this on mount. Fulfill in
                    // the browser: this GET is a real side effect if forwarded.
                    body = { success: true };
                  } else if (path === "/api/providers/kilo/free-models") {
                    body = { models: [{ id: "fixture-free", name: "Fixture Free" }] };
                  } else if (path === "/api/providers/suggested-models") {
                    body = { data: [
                      { id: "sample/catalog", name: "Catalog model", contextLength: 300000 },
                      { id: "sample/aliased", name: "Aliased model", contextLength: 280000 },
                      { id: "sample/model:free", name: "Suggested Model", contextLength: 262144 },
                      { id: "sample/second-free", name: "Second Model", contextLength: 200500 },
                    ] };
                  } else if (path === "/api/providers/filters") {
                    if (method === "PUT") {
                      kiloFreeOnly = request.postDataJSON().freeOnly;
                      return route.fulfill({ status: 204 });
                    }
                    body = screen.section === "provider-compatible" || isCodexDetail || isOpenRouter || isXaiDetail ? { filters: {} } : { filters: { kc: { freeOnly: kiloFreeOnly } } };
                  } else if (path === "/api/models/disabled") {
                    if (method === "POST") kiloDisabled = [...new Set([...kiloDisabled, ...request.postDataJSON().ids])].sort();
                    else if (method === "DELETE") kiloDisabled = kiloDisabled.filter((id) => id !== url.searchParams.get("id"));
                    body = method === "GET" ? { ids: url.searchParams.get("providerAlias") === "kc" ? kiloDisabled : [] } : { success: true };
                  } else if (path === "/api/models/custom") {
                    if (method === "POST") kiloCustom = [...kiloCustom, request.postDataJSON()];
                    if (isOpenRouter && method === "POST") routerCustom = [...routerCustom, request.postDataJSON()];
                    body = isOpenRouter ? (method === "GET" ? { models: routerCustom } : { success: true, added: true }) : screen.section === "provider-compatible" ? { models: [{ providerAlias: compatibleId, id: "fixture-model", type: "llm" }] }
                      : isXaiDetail && screen.name === "empty" ? { models: [{ providerAlias: "xai", id: "saved-custom", type: "llm" }] }
                      : isCodexDetail || isXaiDetail ? { models: [] }
                      : method === "GET" ? { models: kiloCustom } : { success: true, added: true };
                  } else if (path === "/api/models/alias") {
                    body = { aliases: isOpenRouter ? { fixtureAlias: "openrouter/sample/aliased" } : {} };
                  } else if (path === "/api/models/favorites") {
                    body = screen.section === "provider-compatible" || isCodexDetail || isOpenRouter || isXaiDetail ? { favorites: {} } : { favorites: { kc: ["fixture-standard"] } };
                  } else if (path === "/api/models") {
                    body = screen.section === "provider-compatible" || isCodexDetail || isOpenRouter || isXaiDetail ? { models: [] }
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
                  if (path === "/api/oauth/codex/stop-proxy") interceptedCodexStop += 1;
                });

                const path = screen.section === "login" ? "/login" : screen.section === "providers" ? "/dashboard/providers" : screen.section === "providers-new" ? "/dashboard/providers/new" : screen.section === "provider-kilocode" ? "/dashboard/providers/kilocode" : screen.section === "provider-compatible" ? compatiblePath : isCodexDetail ? codexPath : isOpenRouter ? "/dashboard/providers/openrouter" : isXaiDetail ? "/dashboard/providers/xai" : screen.section === "skills" ? "/dashboard/skills" : screen.section === "console-log" ? "/dashboard/console-log" : screen.section === "profile" ? "/dashboard/profile" : screen.section === "db-backups" ? "/dashboard/db-backups" : screen.section === "logs" ? "/dashboard/logs" : screen.section === "quota" ? "/dashboard/quota" : screen.section === "proxy-pools" ? "/dashboard/proxy-pools" : screen.section === "translator" ? "/dashboard/translator" : screen.section === "callback" ? "/callback" : "/dashboard/endpoint";
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
                    const component = screen.section === "login" ? "LoginPageClient." : screen.section === "providers" ? "ProvidersPageClient." : screen.section === "providers-new" ? "ProvidersNewPageClient." : ["provider-kilocode", "provider-compatible"].includes(screen.section) || isCodexDetail || isOpenRouter || isXaiDetail ? "ProviderDetailPageClient." : screen.section === "skills" ? "SkillsPageClient." : screen.section === "console-log" ? "ConsoleLogClient." : screen.section === "profile" ? "ProfilePageClient." : screen.section === "db-backups" ? "DbBackupsPageClient." : screen.section === "logs" ? "ApplicationLogsClient." : screen.section === "quota" ? "QuotaPageClient." : screen.section === "proxy-pools" ? "ProxyPoolsPageClient." : screen.section === "translator" ? "TranslatorPageClient." : "EndpointPageClient.";
                    assert(html.includes(`component-url="/_astro/${component}`), "record mode requires the Astro reference");
                    if (screen.section === "provider-compatible") assert(source.routes[sourceRoute], "dynamic Astro provider template hash must be pinned before recording");
                    if (isCodexDetail) assert(source.routes[sourceRoute], "Astro Codex route hash must be pinned before recording");
                  }
                  const hash = createHash("sha256").update(html).digest("hex");
                  assert(!source.routes[sourceRoute] || source.routes[sourceRoute] === hash, `Astro reference changed for ${sourceRoute}`);
                  if (!source.routes[sourceRoute]) {
                    source.routes[sourceRoute] = hash;
                    await writeFile(sourcePath, JSON.stringify(source, null, 2) + "\n");
                  }
                } else {
                  assert(source.routes[sourceRoute], `missing Astro reference identity for ${sourceRoute}`);
                  assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "compare mode requires the Dioxus candidate");
                }
                if (screen.section === "login") await page.getByRole("heading", { name: "OpenProxy" }).waitFor();
                else if (screen.section === "providers") await page.getByRole("heading", { name: "Custom Providers (OpenAI/Anthropic Compatible)" }).waitFor();
                else if (screen.section === "providers-new") await page.getByRole("heading", { name: "Add New Provider" }).waitFor();
                else if (screen.section === "provider-kilocode") await page.getByRole("checkbox", { name: "Select connection Fixture connection" }).waitFor();
                else if (screen.section === "translator") { await page.getByRole("heading", { name: "Translator Debug" }).waitFor(); if (mode === "record") await page.locator(".monaco-editor .view-lines").waitFor(); }
                else if (screen.section === "provider-compatible") await page.getByRole("heading", { name: "OpenAI Compatible Details", exact: true }).waitFor();
                else if (isCodexDetail) await page.getByRole("checkbox", { name: "Select connection Fixture Codex" }).waitFor();
                else if (isOpenRouter) await page.getByText("Suggested free models (≥200k context):", { exact: true }).waitFor();
                else if (isXaiDetail && ["no-connection", "connection-error"].includes(screen.name)) await page.getByRole("heading", { name: "Available Models", exact: true }).waitFor();
                else if (isXaiDetail) await page.getByRole("checkbox", { name: "Select connection Fixture xAI" }).waitFor();
                else if (screen.section === "skills") await page.getByRole("heading", { name: "OpenProxy (Entry)", exact: true }).waitFor();
                else if (screen.section === "console-log") await page.getByRole("button", { name: /Clear/ }).waitFor();
                else if (screen.section === "profile") await page.getByRole("heading", { name: "Security", exact: true }).waitFor();
                else if (screen.section === "db-backups") await page.getByRole("heading", { name: "Database Backups", exact: true }).waitFor();
                else if (screen.section === "logs") await page.getByRole("heading", { name: "Application Logs", exact: true }).waitFor();
                else if (screen.section === "quota") await page.getByRole("heading", { name: screen.name === "empty" ? "No Providers Connected" : "Provider Limits", exact: true }).waitFor();
                else if (screen.section === "proxy-pools") await page.getByText(screen.name === "empty" ? "No proxy pool entries yet" : "Fixture Proxy", { exact: true }).waitFor();
                else if (screen.section === "callback") await page.getByRole("heading", { name: screen.name === "error" ? "Authentication Failed" : screen.name === "no-auth-data" ? "No Auth Data Found" : "Authentication Complete", exact: true }).waitFor();
                else await page.getByText("Fixture key", { exact: true }).waitFor();
                const disableMotion = () => page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}" });
                const verifyDashboardFonts = async () => {
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
                };
                await disableMotion();
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
                  await verifyDashboardFonts();
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
                } else if (screen.section === "translator") {
                  if (translatorFixtures[screen.name]) {
                    const content = translatorFixtures[screen.name];
                    const detection = page.waitForResponse(response => response.url() === `${appUrl}/api/translator/translate` && response.request().method() === "POST");
                    if (mode === "record") await page.evaluate(value => window.monaco.editor.getEditors()[0].setValue(value), content);
                    else {
                      const editor = page.getByLabel("Translator editor");
                      await editor.fill(content);
                      await editor.evaluate(element => { element.setSelectionRange(0, 0); element.dispatchEvent(new Event("select", { bubbles: true })); element.blur(); });
                    }
                    await detection;
                  }
                  await page.waitForLoadState("networkidle");
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "proxy-pools") {
                  if (screen.name === "selected") {
                    await page.getByText("Select all", { exact: true }).locator("..").locator('input[type="checkbox"]').check();
                    await page.getByText("2 selected", { exact: true }).waitFor();
                  } else if (screen.name === "add") {
                    await page.getByRole("button", { name: /Add Proxy Pool/ }).click();
                    await page.getByRole("heading", { name: "Add Proxy Pool", exact: true }).waitFor();
                  }
                  await page.waitForLoadState("networkidle");
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "quota") {
                  if (screen.name === "reset-times") {
                    await page.getByText("Reset in 4h 30m", { exact: true }).waitFor();
                    assert.equal(await page.getByText("Reset in 4h 30m", { exact: true }).getAttribute("title"), "Today, 4:30 PM");
                    assert.equal(await page.getByText("Expires in 1d 3h 0m", { exact: true }).getAttribute("title"), "Tomorrow, 3:00 PM");
                    assert.equal(await page.getByText("Expired", { exact: true }).last().getAttribute("title"), "Sep 29, 9:00 AM");
                  }
                  if (screen.name === "provider-formats") {
                    await page.getByText("Monthly remaining", { exact: true }).waitFor();
                    await page.getByText("Unlimited", { exact: true }).waitFor();
                    await page.getByText("fixture@example.invalid", { exact: true }).waitFor();
                    await page.getByText("1,234.5 credit value", { exact: true }).waitFor();
                  }
                  if (screen.name === "deepseek-balance") {
                    const positive = page.getByRole("row").filter({ hasText: "Balance (USD)" });
                    const empty = page.getByRole("row").filter({ hasText: "Balance (CNY)" });
                    await positive.waitFor();
                    assert.equal(await positive.getByText("0 / 12.5", { exact: true }).count(), 1, "a positive DeepSeek balance must remain visible");
                    await positive.getByText("100%", { exact: true }).waitFor();
                    await empty.getByText("0 / ∞", { exact: true }).waitFor();
                    await empty.getByText("0%", { exact: true }).waitFor();
                    assert.equal(await positive.getByText("Unlimited", { exact: true }).count(), 0, "a positive DeepSeek balance is not unlimited");
                  }
                  if (screen.name === "kimi-coding") {
                    const weekly = page.getByRole("row").filter({ hasText: "Weekly" });
                    await weekly.getByText("40 / 100", { exact: true }).waitFor();
                    await weekly.getByText("60%", { exact: true }).waitFor();
                    verifyAfterCapture = async () => {
                      const assertIcon = async (icon) => {
                        await icon.waitFor();
                        assert.equal(await icon.getAttribute("src"), "/providers/kimi-coding.png");
                        assert(await icon.evaluate(async (image) => {
                          await image.decode().catch(() => {});
                          return image.complete && image.naturalWidth > 0;
                        }), "Kimi Coding icon must load");
                      };
                      await assertIcon(page.locator('img[alt="kimi-coding"]'));
                      await page.getByTitle("Filter quota providers").click();
                      const option = page.getByRole("button", { name: /kimi-coding/i });
                      await assertIcon(option.locator('img[alt="kimi-coding"]'));
                      await option.click();
                      await assertIcon(page.getByTitle("Filter quota providers").locator('img[alt="kimi-coding"]'));
                    };
                  }
                  if (screen.name === "provider-windows") {
                    await page.getByText("Claude Sonnet", { exact: true }).waitFor();
                    await page.getByText("35%", { exact: true }).waitFor();
                    await page.getByText("Observed from synthetic traffic", { exact: true }).waitFor();
                    await page.getByText("60%", { exact: true }).waitFor();
                    verifyAfterCapture = async () => {
                      const card = page.getByText("Fixture Claude", { exact: true }).locator("xpath=ancestor::div[contains(@class, 'rounded-mini-lg')][1]");
                      quotaClaudeUnauthorized = true;
                      await card.getByTitle("Refresh quota").click();
                      await card.getByText("Synthetic authorization error", { exact: true }).waitFor();
                      assert.equal(await card.getByText("error", { exact: true }).count(), 0, "an auth error is a message, not a fabricated quota row");
                      const antigravity = page.getByText("Fixture account", { exact: true }).locator("xpath=ancestor::div[contains(@class, 'rounded-mini-lg')][1]");
                      quotaAntigravityOrder = true;
                      await antigravity.getByTitle("Refresh quota").click();
                      await antigravity.getByText("Zulu", { exact: true }).waitFor();
                      assert.deepEqual(await antigravity.locator("tbody tr td:first-child").allTextContents(), ["Zulu", "Alpha"], "quota rows follow catalog model IDs, not display names or response order");
                    };
                  }
                  if (screen.name === "empty") await page.getByText("Connect to providers with OAuth to track your API quota limits and usage.", { exact: true }).waitFor();
                  else {
                    await page.getByText("Fixture account", { exact: true }).waitFor();
                    if (!["reset-times", "provider-formats", "provider-windows", "deepseek-balance", "kimi-coding"].includes(screen.name)) await page.getByText("Premium Requests", { exact: true }).waitFor();
                    if (!["provider-formats", "provider-windows", "deepseek-balance", "kimi-coding"].includes(screen.name)) await page.getByText("75%", { exact: true }).waitFor();
                    if (screen.name === "glm-ping") {
                      const ping = () => page.getByTitle("Toggle auto-ping warmup");
                      await ping().waitFor();
                      verifyAfterCapture = async () => {
                        await ping().click();
                        await page.getByRole("button", { name: "Disable auto-ping for Fixture account" }).waitFor();
                        await page.waitForFunction(() => !document.querySelector('[title="Toggle auto-ping warmup"]').disabled);
                        await ping().click();
                        await page.getByRole("button", { name: "Enable auto-ping for Fixture account" }).waitFor();
                        await page.waitForFunction(() => !document.querySelector('[title="Toggle auto-ping warmup"]').disabled);
                        await ping().click();
                        await page.waitForFunction(() => !document.querySelector('[title="Toggle auto-ping warmup"]').disabled);
                        assert.equal(await ping().getAttribute("aria-pressed"), "false", "failed settings save must roll back");
                        assert.deepEqual(glmConfig, { enabled: true, connections: { "other-glm": true, "fixture-glm": false } });
                      };
                    } else if (screen.name.startsWith("codex-")) {
                      const useCredit = () => page.getByRole("button", { name: "Use one Codex reset credit. 1 available." });
                      const expiry = () => page.getByRole("button", { name: "View Codex reset credit expiry" });
                      await useCredit().waitFor();
                      await expiry().waitFor();
                      if (screen.name === "codex-confirm") {
                        await useCredit().click();
                        await page.getByRole("heading", { name: "Reset Codex limit?" }).waitFor();
                      } else if (screen.name === "codex-expiry") {
                        await expiry().click();
                        await page.getByRole("heading", { name: "Codex Reset Credit Expiry" }).waitFor();
                        await page.getByText("2 reset credits", { exact: true }).waitFor();
                      } else verifyAfterCapture = async () => {
                        creditGetError = true;
                        await useCredit().click();
                        await creditLookupStarted;
                        await page.waitForFunction(() => document.querySelector('[title="Refresh quota"]').disabled);
                        assert(await expiry().isDisabled(), "other row actions are disabled during credit lookup");
                        releaseCreditLookup();
                        await page.getByText("Synthetic credit error", { exact: true }).waitFor();
                        assert.equal(writes.length, 0, "a failed lookup cannot consume a credit");
                        creditGetError = false;
                        await expiry().click();
                        await page.getByText("2 reset credits", { exact: true }).waitFor();
                        await page.getByRole("button", { name: "Close reset credit expiry modal" }).click();
                        await useCredit().click();
                        await page.getByRole("heading", { name: "Reset Codex limit?" }).waitFor();
                        await page.getByRole("button", { name: "Cancel", exact: true }).click();
                        assert.equal(writes.length, 0, "cancel must not consume a credit");
                        creditPostError = true;
                        await useCredit().click();
                        await page.getByRole("heading", { name: "Reset Codex limit?" }).waitFor();
                        await page.getByRole("button", { name: "Reset limit", exact: true }).click();
                        await page.getByText("Synthetic reset error", { exact: true }).first().waitFor();
                        assert.equal(writes.length, 1, "failed consumption is not retried automatically");
                        creditPostError = false;
                        await useCredit().click();
                        await page.getByRole("heading", { name: "Reset Codex limit?" }).waitFor();
                        await page.getByRole("button", { name: "Reset limit", exact: true }).click();
                        await codexResetStarted;
                        await page.waitForFunction(() => document.querySelector('[title="Refresh quota"]').disabled);
                        assert(await page.getByRole("button", { name: "Cancel", exact: true }).isDisabled(), "reset cannot be cancelled while the request is pending");
                        assert(await expiry().isDisabled(), "other row actions are disabled during reset");
                        releaseCodexReset();
                        await page.getByRole("button", { name: "No Codex reset credits available" }).waitFor();
                        assert(await page.getByRole("button", { name: "No Codex reset credits available" }).isDisabled());
                      };
                    } else if (screen.name === "ready") verifyAfterCapture = async () => {
                      const before = quotaReads;
                      await page.getByTitle("Refresh quota").click();
                      await page.waitForFunction(() => !document.querySelector('[title="Refresh quota"]').disabled);
                      assert(quotaReads > before, "manual refresh must read the quota again");
                      await page.getByTitle("Filter quota providers").click();
                      await page.getByRole("button", { name: /github/i }).click();
                      await page.getByText("Fixture account", { exact: true }).waitFor();
                      await page.getByTitle("Sort accounts by earliest quota reset time").click();
                      await page.getByTitle("Disable connection").getByRole("switch").click();
                      await page.getByTitle("Enable connection").getByRole("switch").waitFor();
                      await page.getByTitle("Enable connection").getByRole("switch").click();
                      await page.getByTitle("Disable connection").getByRole("switch").waitFor();
                      quotaStatus = 500;
                      await page.getByTitle("Refresh quota").click();
                      await page.getByText("HTTP 500: Synthetic temporary outage", { exact: true }).waitFor();
                      quotaStatus = 404;
                      await page.getByTitle("Refresh quota").click();
                      await page.getByText("Premium Requests", { exact: true }).waitFor();
                      await page.getByText("75%", { exact: true }).waitFor();
                    };
                  }
                  await page.waitForLoadState("networkidle");
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "logs") {
                  const stats = page.getByRole("region", { name: "Median TPS statistics" });
                  await stats.waitFor();
                  if (screen.name === "empty") {
                    await page.getByText("No application logs yet.", { exact: true }).waitFor();
                    await stats.getByText("No saved successful attempts in the last 24 hours for these filters.", { exact: true }).waitFor();
                  } else {
                    await page.getByRole("row").filter({ hasText: "fixture-model" }).last().waitFor();
                    await stats.getByText("20.0", { exact: true }).first().waitFor();
                    if (screen.name === "drawer") {
                      await page.getByRole("row").filter({ hasText: "fixture-model" }).last().click();
                      await page.getByRole("heading", { name: "Request details", exact: true }).waitFor();
                      await page.getByText("Observed throughput", { exact: true }).waitFor();
                    } else if (screen.name === "trace") {
                      await page.getByRole("row").filter({ hasText: "second-model" }).click();
                      await page.getByText("Synthetic failure", { exact: true }).waitFor();
                      await page.getByText("Stream trace", { exact: true }).click();
                      await page.getByText("fixture_tool", { exact: true }).scrollIntoViewIfNeeded();
                      await page.getByText("Upstream events", { exact: true }).waitFor();
                    }
                  }
                  await page.waitForLoadState("networkidle");
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                  if (screen.name === "ready") verifyAfterCapture = async () => {
                    await page.getByRole("checkbox", { name: "fixture-provider" }).uncheck();
                    await stats.getByText("All matching providers are hidden", { exact: true }).waitFor();
                    await page.getByRole("checkbox", { name: "Hide Median TPS" }).check();
                    assert.equal(await stats.count(), 0);
                    await page.reload({ waitUntil: "domcontentloaded" });
                    assert(await page.getByRole("checkbox", { name: "Hide Median TPS" }).isChecked());
                    await page.getByRole("checkbox", { name: "Hide Median TPS" }).uncheck();
                    await stats.getByText("All matching providers are hidden", { exact: true }).waitFor();
                    await page.getByRole("checkbox", { name: "fixture-provider" }).check();
                    await stats.getByText("20.0", { exact: true }).first().waitFor();
                    await page.getByLabel("Status (raw logs only)").selectOption("error");
                    await page.getByText("rate_limit", { exact: true }).waitFor();
                    assert.equal(await page.getByRole("row").filter({ hasText: "fixture-model" }).count(), 1, "status filters raw rows, not TPS statistics");
                  };
                } else if (isXaiDetail) {
                  const refresh = () => page.getByRole("button", { name: "Refresh models", exact: true });
                  const busy = () => page.getByRole("button", { name: "Refreshing models...", exact: true });
                  const warning = page.getByRole("alert").filter({ hasText: "xAI model refresh failed (503). Showing the last available catalog." });
                  await refresh().waitFor();
                  if (!["empty", "unavailable", "no-connection"].includes(screen.name)) await page.locator("code").filter({ hasText: "xai/fixture-model" }).waitFor();
                  assert.equal(await page.getByRole("link", { name: /Get API Key/ }).getAttribute("href"), "https://console.x.ai");
                  await page.getByRole("heading", { name: "Available Models", exact: true }).scrollIntoViewIfNeeded();
                  const diagnostic = {
                    stale: "Synthetic stale catalog warning",
                    unavailable: "Synthetic unavailable catalog warning",
                    "no-connection": "No active xAI connection. Add or enable a connection to discover models.",
                    "connection-error": "Failed to load connections: 503",
                  }[screen.name];
                  if (diagnostic) await page.getByRole("alert").filter({ hasText: diagnostic }).waitFor();
                  const empty = page.getByRole("status").filter({ hasText: "This xAI connection returned no chat/vision models. Saved custom models remain available." });
                  if (screen.name === "empty") {
                    await empty.waitFor();
                    await page.locator("code").filter({ hasText: "xai/saved-custom" }).waitFor();
                  } else if (screen.name === "unavailable") {
                    assert.equal(await empty.count(), 0, "unavailable discovery is not reported as an empty catalog");
                  }
                  if (["pending", "failure"].includes(screen.name)) {
                    await refresh().click();
                    if (screen.name === "pending") {
                      await busy().waitFor();
                      await xaiForceStarted;
                      assert(await busy().isDisabled());
                      await page.waitForFunction(() => document.querySelector('[role="status"]')?.textContent.includes("Loading xAI model catalog"));
                      verifyAfterCapture = async () => {
                        assert.equal(typeof releaseXaiForce, "function");
                        releaseXaiForce();
                        releaseXaiForce = null;
                        await warning.waitFor();
                        await refresh().waitFor();
                      };
                    } else {
                      await warning.waitFor();
                      await refresh().waitFor();
                    }
                  }
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "db-backups") {
                  await page.waitForLoadState("networkidle");
                  if (viewport.name === "desktop") await page.getByText(fixtureCommit.slice(0, 8), { exact: true }).first().waitFor();
                  if (screen.name === "empty") await page.getByText("No backups yet.", { exact: false }).waitFor();
                  else {
                    await page.getByText("fixture-1", { exact: true }).waitFor();
                    await page.getByText("2.0 KB", { exact: true }).waitFor();
                  }
                  const backupRow = page.getByRole("row").filter({ hasText: "fixture-1" });
                  const backupDialog = (name) => page.getByRole("heading", { name, exact: true }).locator("../../..");
                  if (screen.name === "restore") {
                    await backupRow.getByRole("button", { name: "Restore", exact: true }).click();
                    await page.getByRole("heading", { name: "Restore snapshot?", exact: true }).waitFor();
                  } else if (screen.name === "auth") {
                    await page.getByRole("button", { name: "Export db.json", exact: true }).click();
                    await page.getByRole("heading", { name: "Confirm Password", exact: true }).waitFor();
                  } else if (screen.name === "ready") {
                    verifyAfterCapture = async () => {
                      await page.getByRole("button", { name: "Create snapshot", exact: true }).click();
                      await page.getByText("Snapshot created: fixture-new", { exact: true }).waitFor();
                      await backupRow.getByRole("button", { name: "Restore", exact: true }).click();
                      let dialog = backupDialog("Restore snapshot?");
                      await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
                      assert.equal(writes.length, 1, "cancel must not restore");
                      await backupRow.getByRole("button", { name: "Restore", exact: true }).click();
                      await dialog.getByRole("button", { name: "Restore", exact: true }).click();
                      await page.getByText("Restored fixture-1 — 2 providers, 1 API keys.", { exact: true }).waitFor();
                      await backupRow.getByRole("button", { name: "Delete", exact: true }).click();
                      dialog = backupDialog("Delete backup?");
                      await dialog.getByRole("button", { name: "Delete", exact: true }).click();
                      await page.getByText("Backup deleted.", { exact: true }).waitFor();
                      await page.getByRole("button", { name: "Prune", exact: true }).click();
                      dialog = backupDialog("Prune backups?");
                      await dialog.getByRole("button", { name: "Prune", exact: true }).click();
                      await page.getByText("Cleanup ran (deleted 0, kept 1).", { exact: true }).waitFor();
                      const exportButton = page.getByRole("button", { name: "Export db.json", exact: true });
                      await exportButton.click();
                      await page.getByPlaceholder("Current password", { exact: true }).fill("fixture-wrong");
                      await backupDialog("Confirm Password").getByRole("button", { name: "Confirm", exact: true }).click();
                      await page.getByText("Export failed: Invalid password", { exact: true }).waitFor();
                      await exportButton.click();
                      await page.getByPlaceholder("Current password", { exact: true }).fill("fixture-current");
                      const downloading = page.waitForEvent("download");
                      await backupDialog("Confirm Password").getByRole("button", { name: "Confirm", exact: true }).click();
                      const download = await downloading;
                      assert.equal(download.suggestedFilename(), "fixture-export.json");
                      const chunks = [];
                      for await (const chunk of await download.createReadStream()) chunks.push(chunk);
                      assert.deepEqual(JSON.parse(Buffer.concat(chunks).toString()), { fixture: true });
                      assert.deepEqual(backupExports, ["fixture-wrong", "fixture-current"]);
                      const input = page.locator('input[type="file"]');
                      const file = { name: "fixture.json", mimeType: "application/json", buffer: Buffer.from('{"fixture":true}') };
                      await input.setInputFiles(file);
                      dialog = backupDialog("Import database?");
                      await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
                      assert.equal(writes.length, 4, "cancel must not import");
                      await input.setInputFiles(file);
                      await dialog.getByRole("button", { name: "Import", exact: true }).click();
                      await page.getByPlaceholder("Current password", { exact: true }).fill("fixture-current");
                      await backupDialog("Confirm Password").getByRole("button", { name: "Confirm", exact: true }).click();
                      await page.getByText("Imported fixture.json — 2 providers, 1 API keys.", { exact: true }).waitFor();
                    };
                  }
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "profile") {
                  await page.waitForLoadState("networkidle");
                  if (viewport.name === "desktop") await page.getByText(fixtureCommit.slice(0, 8), { exact: true }).first().waitFor();
                  const heading = { security: "Security", appearance: "Appearance", language: "Language", network: "Network", observability: "Observability", database: "Database", "database-auth": "Database" }[screen.name];
                  const card = heading ? page.getByRole("heading", { name: heading, exact: true }).locator(screen.name === "appearance" ? "../../../.." : "../..") : null;
                  if (screen.name === "appearance") {
                    verifyAfterCapture = async () => {
                      await card.getByRole("button").filter({ hasText: "dark_mode" }).click();
                      await page.waitForFunction(() => document.documentElement.classList.contains("dark") && JSON.parse(localStorage.getItem("theme")).state.theme === "dark");
                      await card.getByRole("button").filter({ hasText: "contrast" }).click();
                      await page.waitForFunction(() => JSON.parse(localStorage.getItem("theme")).state.theme === "system");
                    };
                  } else if (screen.name === "language") {
                    verifyAfterCapture = async () => {
                      await card.getByRole("button", { name: /Display language/ }).click();
                      await page.getByText("Select Language", { exact: true }).waitFor();
                      await page.getByTitle("Русский").click();
                      await page.getByRole("button").filter({ hasText: "🇷🇺" }).waitFor();
                      assert.match(await page.evaluate(() => document.cookie), /locale=ru/);
                    };
                  } else if (screen.name === "network") {
                    const toggle = card.getByRole("switch");
                    await page.waitForFunction(() => {
                      const heading = [...document.querySelectorAll("h3")].find((h) => h.textContent === "Network");
                      return heading?.parentElement?.parentElement?.querySelector('[role="switch"]:not(:disabled)');
                    });
                    verifyAfterCapture = async () => {
                      await toggle.click();
                      await card.getByPlaceholder("http://127.0.0.1:7897").fill("http://127.0.0.1:7897");
                      await card.getByPlaceholder("localhost,127.0.0.1").fill("localhost");
                      await card.getByRole("button", { name: "Test proxy URL" }).click();
                      await card.getByText("Proxy test OK (connected) in 12ms", { exact: true }).waitFor();
                      await card.getByRole("button", { name: "Apply" }).click();
                      await card.getByText("Proxy settings applied", { exact: true }).waitFor();
                      await toggle.click();
                      await card.getByPlaceholder("http://127.0.0.1:7897").waitFor({ state: "detached" });
                    };
                  } else if (screen.name === "observability") {
                    const toggle = card.getByRole("switch");
                    await page.waitForFunction(() => {
                      const heading = [...document.querySelectorAll("h3")].find((h) => h.textContent === "Observability");
                      return heading?.parentElement?.parentElement?.querySelector('[role="switch"]:not(:disabled)');
                    });
                    verifyAfterCapture = async () => {
                      await toggle.click();
                      await page.waitForFunction(() => document.querySelector('[role="switch"][aria-label="Enable Request Logging"]')?.getAttribute("aria-checked") === "true" || [...document.querySelectorAll("h3")].find((h) => h.textContent === "Observability")?.parentElement?.parentElement?.querySelector('[role="switch"]')?.getAttribute("aria-checked") === "true");
                      await toggle.click();
                      await page.waitForFunction(() => [...document.querySelectorAll("h3")].find((h) => h.textContent === "Observability")?.parentElement?.parentElement?.querySelector('[role="switch"]')?.getAttribute("aria-checked") === "false");
                    };
                  } else if (screen.name === "database" || screen.name === "database-auth") {
                    await page.waitForFunction(() => {
                      const heading = [...document.querySelectorAll("h3")].find((h) => h.textContent === "Security");
                      const button = heading?.parentElement?.parentElement?.querySelector('[role="switch"]');
                      return button && !button.disabled && button.getAttribute("aria-checked") === "true";
                    });
                    const backup = card.getByRole("button", { name: /Download Backup/ });
                    const openAuth = async () => {
                      await backup.click();
                      await page.getByRole("heading", { name: "Confirm Password", exact: true }).waitFor();
                      assert(await page.getByRole("button", { name: "Confirm", exact: true }).isDisabled());
                    };
                    if (screen.name === "database-auth") {
                      await openAuth();
                      await page.getByText("Enter your current password to export the database.", { exact: true }).waitFor();
                      verifyAfterCapture = async () => { await page.getByRole("button", { name: "Cancel", exact: true }).click(); assert.deepEqual(profileExports, []); };
                    } else {
                      verifyAfterCapture = async () => {
                        await openAuth();
                        await page.getByPlaceholder("Current password", { exact: true }).fill("fixture-wrong");
                        await page.getByRole("button", { name: "Confirm", exact: true }).click();
                        await card.getByText("Invalid password", { exact: true }).waitFor();
                        await openAuth();
                        await page.getByPlaceholder("Current password", { exact: true }).fill("fixture-current");
                        const downloading = page.waitForEvent("download");
                        await page.getByRole("button", { name: "Confirm", exact: true }).click();
                        const download = await downloading;
                        assert.match(download.suggestedFilename(), /^openproxy-backup-.*\.json$/);
                        const chunks = [];
                        for await (const chunk of await download.createReadStream()) chunks.push(chunk);
                        assert.deepEqual(JSON.parse(Buffer.concat(chunks).toString()), { metadata: { fixture: true } });
                        await card.getByText("Database backup downloaded", { exact: true }).waitFor();
                        assert.deepEqual(profileExports, ["fixture-wrong", "fixture-current"]);
                        const file = { name: "fixture.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify({ metadata: { fixture: true } })) };
                        await card.locator('input[type="file"]').setInputFiles(file);
                        await page.getByRole("heading", { name: "Confirm Password", exact: true }).waitFor();
                        assert.equal(writes.length, 0, "import must wait for password confirmation");
                        await page.getByRole("button", { name: "Cancel", exact: true }).click();
                        await card.locator('input[type="file"]').setInputFiles(file);
                        await page.getByPlaceholder("Current password", { exact: true }).fill("fixture-current");
                        await page.getByRole("button", { name: "Confirm", exact: true }).click();
                        await card.getByText("Database imported from fixture.json", { exact: true }).waitFor();
                        await page.locator("main").getByRole("button", { name: /Shutdown/ }).click();
                        await page.getByRole("heading", { name: "Shutdown Server", exact: true }).waitFor();
                        await page.getByRole("button", { name: "Cancel", exact: true }).click();
                        assert.equal(writes.length, 1, "cancel must not shut down the server");
                        await page.locator("main").getByRole("button", { name: /Shutdown/ }).click();
                        await page.getByRole("button", { name: "Shutdown", exact: true }).click();
                        await page.getByRole("heading", { name: "Shutdown Server", exact: true }).waitFor({ state: "detached" });
                        await page.getByRole("button", { name: /Logout/ }).click();
                        await page.waitForURL(`${appUrl}/login`);
                      };
                    }
                  } else if (screen.name === "bottom") {
                    await page.locator("main .overflow-y-auto").evaluate((element) => { element.scrollTop = element.scrollHeight; });
                  } else if (screen.name === "security") {
                  const toggle = card.getByRole("switch");
                  await page.waitForFunction(() => {
                    const heading = [...document.querySelectorAll("h3")].find((h) => h.textContent === "Security");
                    const button = heading?.parentElement?.parentElement?.querySelector('[role="switch"]');
                    return button && !button.disabled && button.getAttribute("aria-checked") === "true";
                  });
                  await card.getByPlaceholder("Enter current password").waitFor();
                  verifyAfterCapture = async () => {
                    const fill = async (confirmation) => {
                      await card.getByPlaceholder("Enter current password").fill("fixture-current");
                      await card.getByPlaceholder("Enter new password").fill("fixture-new-pass");
                      await card.getByPlaceholder("Confirm password").fill(confirmation);
                    };
                    await fill("fixture-mismatch");
                    await card.getByRole("button", { name: "Update Password" }).click();
                    await card.getByText("Passwords do not match", { exact: true }).waitFor();
                    assert.equal(writes.length, 0, "mismatched passwords are never sent");
                    await fill("fixture-new-pass");
                    await card.getByRole("button", { name: "Update Password" }).click();
                    await card.getByText("Password updated successfully.", { exact: true }).waitFor();
                    assert.equal(await card.getByPlaceholder("Enter current password").inputValue(), "");
                    await toggle.click();
                    await card.getByPlaceholder("Enter current password").waitFor({ state: "detached" });
                    await toggle.click();
                    await card.getByPlaceholder("Enter current password").waitFor();
                    await fill("fixture-new-pass");
                    await card.getByRole("button", { name: "Update Password" }).click();
                    await page.waitForURL(`${appUrl}/login`);
                  };
                  }
                } else if (screen.section === "console-log") {
                  const output = page.locator("#console-log-output, div.bg-black.rounded-b-lg");
                  await output.waitFor();
                  if (screen.name === "empty") {
                    await output.getByText("No console logs yet.", { exact: true }).waitFor();
                  } else {
                    await output.getByText("[fixture] [ERROR] latest line", { exact: true }).waitFor();
                    assert.equal(await output.locator("div.space-y-0\\.5 > div").count(), 200, "the stream retains only the latest 200 lines");
                    assert.equal(await output.getByText("[fixture] [INFO] line 1", { exact: true }).count(), 0);
                    assert.equal(await output.getByText("[fixture] [INFO] line 2", { exact: true }).count(), 1);
                    assert.equal(await output.getByText("[fixture] [ERROR] latest line", { exact: true }).getAttribute("class"), "text-red-400");
                    await page.waitForFunction(() => {
                      const output = document.querySelector("div.bg-black.rounded-b-lg");
                      return output && output.scrollTop + output.clientHeight >= output.scrollHeight;
                    });
                    verifyAfterCapture = async () => {
                      const response = page.waitForResponse((r) => r.request().method() === "DELETE" && r.url() === `${appUrl}/api/translator/console-logs`);
                      await page.getByRole("button", { name: /Clear/ }).click();
                      assert((await response).ok());
                      assert.equal(await output.getByText("[fixture] [ERROR] latest line", { exact: true }).count(), 1, "the DELETE alone does not clear the view");
                      await page.evaluate(() => window.__consoleSource.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ type: "clear" }) })));
                      await output.getByText("No console logs yet.", { exact: true }).waitFor();
                    };
                  }
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                } else if (screen.section === "skills") {
                  const rawUrl = "https://raw.githubusercontent.com/0FL01/openproxy/refs/heads/main/.agents/skills/openproxy/SKILL.md";
                  const copy = page.getByTitle(rawUrl);
                  await copy.waitFor();
                  assert.equal(await page.getByText(`Read this skill and use it: ${rawUrl}`, { exact: true }).count(), 1);
                  assert.equal(await page.getByRole("link", { name: new RegExp(rawUrl) }).getAttribute("href"), "https://github.com/0FL01/openproxy/blob/main/.agents/skills/openproxy/SKILL.md");
                  assert.equal(await page.getByRole("link", { name: /View on GitHub/ }).getAttribute("href"), "https://github.com/0FL01/openproxy/tree/main/.agents/skills");
                  const checkCopy = async () => {
                    await copy.click();
                    await copy.filter({ hasText: "Copied!" }).waitFor();
                    assert.equal(await page.evaluate(() => window.__fixtureCopied), rawUrl);
                  };
                  await page.waitForLoadState("networkidle");
                  if (screen.action === "copied") await checkCopy();
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                  verifyAfterCapture = async () => {
                    if (screen.action !== "copied") await checkCopy();
                    await copy.filter({ hasText: "Copy link" }).waitFor();
                  };
                } else if (isOpenRouter) {
                  const section = page.getByText("Suggested free models (≥200k context):", { exact: true }).locator("..");
                  const suggestion = section.getByTitle("Suggested Model · 262k ctx");
                  await suggestion.waitFor();
                  assert.equal(await section.getByRole("button").count(), 2, "catalog and aliased suggestions are excluded");
                  assert.equal((await suggestion.textContent()).trim(), "addmodel:free");
                  assert.equal((await section.getByTitle("Second Model · 201k ctx").textContent()).trim(), "addsecond-free");
                  await page.waitForLoadState("networkidle");
                  await suggestion.scrollIntoViewIfNeeded();
                  await page.mouse.move(1, 1);
                  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
                  verifyAfterCapture = async () => {
                    const response = page.waitForResponse((r) => r.request().method() === "POST" && r.url() === `${appUrl}/api/models/custom`);
                    await suggestion.click();
                    assert((await response).ok());
                    await page.waitForLoadState("networkidle");
                    assert.deepEqual(routerCustom, [{ providerAlias: "openrouter", id: "sample/model:free", type: "llm" }]);
                    assert((await page.reload({ waitUntil: "domcontentloaded" }))?.ok());
                    await page.waitForLoadState("networkidle");
                    await page.locator("code").filter({ hasText: "openrouter/sample/model:free" }).waitFor();
                    await page.getByTitle("Suggested Model · 262k ctx").waitFor();
                  };
                } else if (isCodexDetail) {
                  const tooltip = "Auto-starts the next available Codex quota window after reset with a tiny gpt-6-luna request. Consumes a small amount of quota.";
                  const waitState = async (on) => {
                    await page.locator("main .overflow-y-auto").getByRole("heading", { name: "OpenAI Codex", exact: true }).waitFor();
                    await page.getByText("1 connection", { exact: true }).waitFor();
                    await page.getByRole("checkbox", { name: "Select connection Fixture Codex" }).waitFor();
                    const button = page.getByRole("button", { name: `${on ? "Disable" : "Enable"} auto-ping for Fixture Codex`, exact: true });
                    await button.waitFor();
                    assert.equal(await button.getAttribute("aria-pressed"), String(on), "per-connection auto-ping state");
                    assert(await button.isEnabled(), "auto-ping control must be ready, not saving");
                    assert.equal(await page.getByText(tooltip, { exact: true }).count(), 1, "Codex warmup explanation must be present");
                    await button.scrollIntoViewIfNeeded();
                    assert(await button.evaluate((element) => {
                      const rect = element.getBoundingClientRect();
                      const parent = element.closest("main .overflow-y-auto")?.getBoundingClientRect();
                      return parent && rect.left >= 0 && rect.right <= innerWidth && rect.top >= parent.top && rect.bottom <= parent.bottom && rect.bottom <= innerHeight;
                    }), "auto-ping button must be visible in the screenshot");
                    return button;
                  };
                  const waitPatch = () => page.waitForResponse((response) => {
                    const url = new URL(response.url());
                    return response.request().method() === "PATCH" && url.origin === appUrl && `${url.pathname}${url.search}` === "/api/settings";
                  });
                  const reload = async () => {
                    await page.waitForLoadState("networkidle");
                    assert((await page.reload({ waitUntil: "domcontentloaded" }))?.ok());
                    await page.waitForLoadState("networkidle");
                    await disableMotion();
                    await verifyDashboardFonts();
                  };
                  let button = await waitState(false);
                  assert.deepEqual(currentCodexConfig, codexConfig(false, screen.name === "off"));
                  if (screen.name === "on") {
                    const response = waitPatch();
                    await button.click();
                    assert((await response).ok());
                    await page.waitForLoadState("networkidle");
                    assert.deepEqual(currentCodexConfig, codexConfig(true));
                    await reload();
                    button = await waitState(true);
                    verifyAfterCapture = async () => {
                      const response = waitPatch();
                      await button.click();
                      assert((await response).ok());
                      await page.waitForLoadState("networkidle");
                      assert.deepEqual(currentCodexConfig, codexConfig(false));
                      await reload();
                      await waitState(false);
                    };
                  }
                  await page.waitForLoadState("networkidle");
                  const icon = page.locator("main .overflow-y-auto").getByRole("img", { name: "OpenAI Codex", exact: true });
                  await icon.waitFor();
                  assert(await icon.evaluate((element) => element.complete && element.naturalWidth > 0), "Codex icon must load");
                  if (viewport.name === "desktop") await page.getByLabel("Build commit unavailable").first().waitFor();
                  if (screen.section === "provider-codex-bulk-add") {
                    await page.getByRole("button", { name: /Bulk Add/ }).click();
                    await page.getByRole("heading", { name: "Bulk Add Codex Accounts", exact: true }).waitFor();
                    await page.getByText("Paste a Codex auth.json object or an array of accounts. Terminal prompts after the JSON are ignored.", { exact: true }).waitFor();
                    const input = page.locator("textarea");
                    assert.equal(await input.count(), 1, "exactly one bulk input must be present");
                    assert(await input.isVisible());
                    assert.equal(await input.inputValue(), "", "no credentials or JSON are entered");
                    assert.match(await input.getAttribute("placeholder"), /"auth_mode": "chatgpt"/, "expected the built-in example placeholder");
                    assert(await page.getByRole("button", { name: "Import All", exact: true }).isDisabled());
                    assert(await page.getByRole("button", { name: "Close", exact: true }).filter({ hasText: /^Close$/ }).isEnabled());
                    await page.waitForLoadState("networkidle");
                  }
                  if (isManualOAuth) {
                    await page.getByRole("button", { name: /^add Add$/ }).click();
                    const heading = page.getByRole("heading", { name: "Connect OpenAI Codex", exact: true });
                    await heading.waitFor();
                    const modal = heading.locator("../../..");
                    await modal.getByText("Waiting for popup authorization…", { exact: true }).waitFor();
                    await modal.getByText("Step 1: Open this URL in your browser", { exact: true }).waitFor();
                    await modal.getByText("Step 2: Paste the callback URL here", { exact: true }).waitFor();
                    const inputs = modal.locator("input");
                    assert.equal(await inputs.count(), 2);
                    assert.equal(await inputs.nth(0).inputValue(), "https://fixture.invalid/authorize?client=synthetic");
                    assert.equal(await inputs.nth(0).evaluate((input) => input.readOnly), true);
                    assert.equal(await inputs.nth(1).getAttribute("placeholder"), `${appUrl}/callback?code=...`);
                    assert.equal(await inputs.nth(1).inputValue(), "");
                    assert(await modal.getByRole("button", { name: "Connect", exact: true }).isDisabled());
                    assert(await modal.getByRole("button", { name: "Cancel", exact: true }).isEnabled());
                    assert.equal(await modal.getByRole("button", { name: "Open Authorization", exact: true }).count(), 0, "the synthetic popup remains open");
                    assert.equal(await page.evaluate(() => window.__fixtureOpened[0]), mode === "record" ? "https://fixture.invalid/authorize?client=synthetic" : "about:blank");
                    if (mode === "compare") {
                      assert.equal(await page.evaluate(() => window.__fixturePopup.location.href), "https://fixture.invalid/authorize?client=synthetic");
                      assert.equal(await page.evaluate(() => window.__fixturePopup.opener), null);
                    }
                    await page.waitForLoadState("networkidle");
                  }
                  await page.mouse.move(1, 1);
                  await page.waitForFunction(() => Array.from(document.images).every((image) => image.complete));
                  await page.evaluate(async () => {
                    await Promise.all(Array.from(document.images, (image) => image.decode().catch(() => {})));
                    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
                  });
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
                  if (screen.action && screen.action !== "device-oauth" && !isKiloOAuthError) await scrollModels();
                  if (screen.action === "device-oauth") {
                    await page.getByRole("button", { name: /OAuth/ }).click();
                    await page.getByRole("heading", { name: "Connect Kilo Code" }).waitFor();
                    await page.getByText("Visit the login URL below and authorize:", { exact: true }).waitFor();
                    await page.getByText("ABCD-EFGH", { exact: true }).waitFor();
                    verifyAfterCapture = async () => {
                      const modal = page.getByRole("heading", { name: "Connect Kilo Code" }).locator("../../..");
                      await modal.getByRole("button", { name: /Open/ }).click();
                      assert.equal((await page.evaluate(() => window.__fixtureOpened))[0], "https://fixture.example.invalid/login?user_code=ABCD-EFGH");
                    };
                  } else if (isKiloOAuthError) {
                    await page.getByRole("button", { name: /OAuth/ }).click();
                    const heading = page.getByRole("heading", { name: "Connect Kilo Code", exact: true });
                    await heading.waitFor();
                    const modal = heading.locator("../../..");
                    await modal.getByRole("heading", { name: "Connection Failed", exact: true }).waitFor();
                    await modal.getByText("Synthetic device failure", { exact: true }).waitFor();
                    if (mode === "compare") {
                      await modal.getByRole("alert").getByText("Synthetic device failure", { exact: true }).waitFor();
                      assert.equal(await modal.getByText("error", { exact: true }).getAttribute("aria-hidden"), "true");
                    }
                    assert.equal(deviceAuthAttempts, 1);
                    assert.equal(await modal.getByRole("button", { name: "Try Again", exact: true }).count(), 1);
                    assert.equal(await modal.getByRole("button", { name: "Cancel", exact: true }).count(), 1);
                    assert.equal(await page.evaluate(() => window.__fixtureOpened), undefined, "failed authorization cannot open a popup");
                    verifyAfterCapture = async () => {
                      await modal.getByRole("button", { name: "Try Again", exact: true }).press("Enter");
                      await modal.getByText("Synthetic retry failure", { exact: true }).waitFor();
                      assert.equal(deviceAuthAttempts, 2, "retry sends exactly one new device-code request");
                      await modal.getByRole("button", { name: "Cancel", exact: true }).press("Enter");
                      await heading.waitFor({ state: "detached" });
                      assert.equal(deviceAuthAttempts, 2, "cancel cannot restart authorization");
                      assert.equal(await page.evaluate(() => window.__fixtureOpened), undefined);
                    };
                  } else if (screen.action === "disable-model") {
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

                // The Profile route is incremental; these are card comparisons,
                // not a claim of full-page Profile parity.
                const image = screen.section === "profile" && !["database-auth", "ready", "bottom"].includes(screen.name)
                  ? await page.getByRole("heading", { name: { security: "Security", appearance: "Appearance", language: "Language", network: "Network", observability: "Observability", database: "Database" }[screen.name], exact: true }).locator(screen.name === "appearance" ? "../../../.." : "../..").screenshot({ animations: "disabled" })
                  : await page.screenshot({ fullPage: true, animations: "disabled" });
                if (verifyAfterCapture) await verifyAfterCapture();
                scenarioReady = true;
                return image;
              } finally {
                if (releaseXaiForce) releaseXaiForce();
                await context.close();
                assert.deepEqual(unexpected, [], "unexpected network requests were blocked");
                if (scenarioReady) assert.deepEqual(writes, expectedWrites, "unexpected API mutation");
                if (scenarioReady && mode === "record" && screen.section === "provider-codex-bulk-add") assert(interceptedCodexStop > 0, "Astro's side-effecting stop-proxy GET must be intercepted");
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
