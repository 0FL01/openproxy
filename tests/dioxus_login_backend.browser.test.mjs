import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";
import { isDeepStrictEqual } from "node:util";

test("Dioxus Profile database backup requires password reauthentication on an isolated backend", {
  skip: process.env.RUN_DIOXUS_BACKEND_TEST !== "1",
  timeout: 60_000,
}, async () => {
  assert.equal(process.env.DIOXUS_BACKEND_OUTPUT_SUPPRESSED, "1");
  const { chromium } = await import(process.env.DASHBOARD_PLAYWRIGHT_MODULE || "playwright");
  const browser = await chromium.launch({
    headless: true,
    chromiumSandbox: true,
    ...(process.env.DASHBOARD_CHROMIUM_EXECUTABLE ? { executablePath: process.env.DASHBOARD_CHROMIUM_EXECUTABLE } : {}),
  });
  try {
    const context = await browser.newContext({ serviceWorkers: "block", acceptDownloads: true });
    try {
      const password = "synthetic-browser-only";
      const wrongPassword = "synthetic-wrong-password";
      let backup;
      let exports = 0;
      let imports = 0;
      const blocked = [];
      await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== "https://app:4623") {
          blocked.push("external request");
          return route.abort("blockedbyclient");
        }
        const method = request.method();
        const path = url.pathname;
        if (method === "POST" && path === "/api/auth/login" && !url.search) {
          const body = request.postDataJSON();
          if (body?.password === password && Object.keys(body).length === 1) return route.continue();
        }
        if (method === "POST" && path === "/api/auth/password" && !url.search) {
          const body = request.postDataJSON();
          if (body?.newPassword === password && Object.keys(body).length === 1) return route.continue();
        }
        if (method === "PATCH" && path === "/api/settings" && !url.search) {
          const body = request.postDataJSON();
          if (body?.requireLogin === true && Object.keys(body).length === 1) return route.continue();
        }
        if (path === "/api/settings/database" && !url.search) {
          if (method === "GET" && [wrongPassword, password][exports] === request.headers()["x-op-password"]) {
            exports++;
            return route.continue();
          }
          if (method === "POST" && backup && imports < 2) {
            const body = request.postDataJSON();
            const sentPassword = body.password;
            delete body.password;
            if (sentPassword === [wrongPassword, password][imports] && isDeepStrictEqual(body, backup)) {
              imports++;
              return route.continue();
            }
          }
        }
        if (method === "GET" && !url.search && (
          ["/login", "/dashboard/profile", "/favicon.svg", "/api/auth/status", "/api/build", "/api/settings", "/api/providers"].includes(path) ||
          path.startsWith("/assets/") || path.startsWith("/_astro/") || path.startsWith("/i18n/")
        )) return route.continue();
        blocked.push(`${method} ${path}`);
        return route.abort("blockedbyclient");
      });
      const page = await context.newPage();
      page.setDefaultTimeout(5_000);
      await page.goto("https://app:4623/login");
      assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1);
      const login = await page.evaluate(async (password) => (await fetch("/api/auth/login", {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ password }),
      })).status, password);
      assert.equal(login, 200);
      const connections = await page.evaluate(async () => (await (await fetch("/api/providers")).json()).connections.length);
      assert.equal(connections, 0, "the fresh disposable backend has no provider connections");
      // INITIAL_PASSWORD permits the first login but is not the stored bcrypt
      // hash required by database reauthentication. Set it only in this tmpfs.
      const setup = await page.evaluate(async (password) => {
        const headers = { "Content-Type": "application/json" };
        const saved = await fetch("/api/auth/password", { method: "POST", headers, body: JSON.stringify({ newPassword: password }) });
        const enabled = await fetch("/api/settings", { method: "PATCH", headers, body: JSON.stringify({ requireLogin: true }) });
        const settings = await (await fetch("/api/settings")).json();
        return { saved: saved.status, enabled: enabled.status, hasPassword: settings.hasPassword, requireLogin: settings.requireLogin };
      }, password);
      assert.deepEqual(setup, { saved: 200, enabled: 200, hasPassword: true, requireLogin: true });
      await page.goto("https://app:4623/dashboard/profile");
      await page.getByRole("heading", { name: "Database", exact: true }).waitFor();
      const confirm = async (value) => {
        const dialog = page.getByRole("dialog", { name: "Confirm Password" });
        await dialog.getByPlaceholder("Current password", { exact: true }).fill(value);
        await dialog.getByRole("button", { name: "Confirm", exact: true }).click();
      };

      await page.getByRole("button", { name: /Download Backup/ }).click();
      await confirm(wrongPassword);
      await page.getByText("Invalid password", { exact: true }).waitFor();
      await page.getByRole("button", { name: /Download Backup/ }).click();
      const downloadPromise = page.waitForEvent("download");
      await confirm(password);
      const download = await downloadPromise;
      assert.match(download.suggestedFilename(), /^openproxy-backup-.*\.json$/);
      const chunks = [];
      for await (const chunk of await download.createReadStream()) chunks.push(chunk);
      backup = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      assert(backup && typeof backup === "object" && !Array.isArray(backup));
      const file = { name: "synthetic-backup.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(backup)) };

      await page.locator('input[type="file"]').setInputFiles(file);
      await confirm(wrongPassword);
      await page.getByText("Invalid password", { exact: true }).waitFor();
      await page.locator('input[type="file"]').setInputFiles(file);
      await confirm(password);
      await page.getByText("Database imported from synthetic-backup.json", { exact: true }).waitFor();
      assert.equal(exports, 2);
      assert.equal(imports, 2);
      assert.deepEqual(blocked, []);
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
});

// Run first against a fresh, output-suppressed .ui-browser backend. No Codex
// connection is needed to edit its advertised context setting.
test("Dioxus provider advertised context persists across a page reload with an isolated backend", {
  skip: process.env.RUN_DIOXUS_BACKEND_TEST !== "1",
  timeout: 60_000,
}, async () => {
  assert.equal(process.env.DIOXUS_BACKEND_OUTPUT_SUPPRESSED, "1", "fixture backend output must be suppressed before startup");
  const { chromium } = await import(process.env.DASHBOARD_PLAYWRIGHT_MODULE || "playwright");
  const browser = await chromium.launch({
    headless: true,
    chromiumSandbox: true,
    ...(process.env.DASHBOARD_CHROMIUM_EXECUTABLE ? { executablePath: process.env.DASHBOARD_CHROMIUM_EXECUTABLE } : {}),
  });
  try {
    const context = await browser.newContext({ serviceWorkers: "block" });
    try {
      const fixturePassword = "synthetic-browser-only";
      const newLimit = 375_000;
      let expectedLimits;
      let saving = false;
      let patches = 0;
      const blocked = { external: 0, unexpected: 0 };
      await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== "https://app:4623") {
          blocked.external++;
          return route.abort("blockedbyclient");
        }
        const method = request.method();
        const path = url.pathname;
        if (method === "POST" && path === "/api/auth/login" && !url.search) {
          let body;
          try { body = request.postDataJSON(); } catch { body = null; }
          if (body?.password === fixturePassword && Object.keys(body).length === 1) return route.continue();
        }
        if (method === "PATCH" && path === "/api/settings" && !url.search && saving && patches < 2) {
          let body;
          try { body = request.postDataJSON(); } catch { body = null; }
          const limits = body?.providerContextLimits;
          if (body && Object.keys(body).length === 1 && limits && typeof limits === "object" &&
              !Array.isArray(limits) && Object.keys(limits).length === Object.keys(expectedLimits).length &&
              Object.entries(expectedLimits).every(([provider, value]) => limits[provider] === value)) {
            patches++;
            return route.continue();
          }
        }
        const allowedGet = method === "GET" && (
          (!url.search && ["/login", "/dashboard/providers/codex", "/dashboard/providers/glm", "/favicon.svg", "/providers/codex.png", "/providers/glm.png"].includes(path)) ||
          (!url.search && ["/api/auth/status", "/api/build", "/api/settings", "/api/providers", "/api/provider-nodes",
            "/api/catalog", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites", "/api/providers/filters"].includes(path)) ||
          (path === "/api/models/disabled" && ["?providerAlias=cx", "?providerAlias=glm"].includes(url.search)) ||
          (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
          (!url.search && (path.startsWith("/assets/") || path.startsWith("/_astro/")))
        );
        if (allowedGet) return route.continue();
        // Never allow provider validation, tests, model discovery, or other writes.
        blocked.unexpected++;
        return route.abort("blockedbyclient");
      });

      const page = await context.newPage();
      page.setDefaultTimeout(5_000);
      await page.goto("https://app:4623/login");
      assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "test must use the Dioxus build");
      const loginStatus = await page.evaluate(async (password) => (await fetch("/api/auth/login", {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ password }),
      })).status, fixturePassword);
      assert.equal(loginStatus, 200);

      // Project only non-secret fields; the backend must have no connections.
      const connectionCount = await page.evaluate(async () => {
        const response = await fetch("/api/providers", { cache: "no-store" });
        const data = await response.json();
        return response.ok && Array.isArray(data.connections) ? data.connections.length : -1;
      });
      assert.equal(connectionCount, 0, "the disposable backend must start without provider connections");
      const readSettings = () => page.evaluate(async () => {
        const response = await fetch("/api/settings", { cache: "no-store" });
        const settings = await response.json();
        return { status: response.status, limits: settings.providerContextLimits,
          codexAutoPing: settings.codexAutoPing ?? null, glmAutoPing: settings.glmAutoPing ?? null };
      });
      const initial = await readSettings();
      assert.equal(initial.status, 200);
      assert.equal(initial.limits?.codex, 500_000);
      assert.deepEqual([initial.codexAutoPing, initial.glmAutoPing], [null, null], "auto-ping must be off");
      expectedLimits = { ...initial.limits, codex: newLimit };

      await page.goto("https://app:4623/dashboard/providers/codex");
      await page.getByRole("heading", { name: "Available Models" }).waitFor();
      assert.deepEqual(blocked, { external: 0, unexpected: 0 }, "unexpected requests must be blocked");
      const advertisedContext = page.getByRole("spinbutton", { name: "Advertised Context" });
      await advertisedContext.waitFor();
      assert.equal(await advertisedContext.inputValue(), String(initial.limits.codex));
      await advertisedContext.fill(String(newLimit));
      saving = true;
      const [saved] = await Promise.all([
        page.waitForResponse((response) => response.url() === "https://app:4623/api/settings" && response.request().method() === "PATCH"),
        advertisedContext.press("Enter"),
      ]);
      saving = false;
      assert.equal(saved.status(), 200, "the actual backend must save the setting");
      assert.deepEqual((await saved.json()).providerContextLimits, expectedLimits);
      assert.equal(patches, 1);
      assert.deepEqual(await readSettings(), { ...initial, limits: expectedLimits });

      await page.reload();
      await advertisedContext.waitFor();
      await page.waitForFunction((value) => Array.from(document.querySelectorAll('input[type="number"]')).some((input) => input.value === value), String(newLimit));
      assert.equal(await advertisedContext.inputValue(), String(newLimit));
      assert.deepEqual(await readSettings(), { ...initial, limits: expectedLimits });
      assert.equal(patches, 1, "reload must not write settings again");

      await page.goto("https://app:4623/dashboard/providers/glm");
      await advertisedContext.waitFor();
      await page.waitForFunction((value) => Array.from(document.querySelectorAll('input[type="number"]')).some((input) => input.value === value && !input.disabled), String(initial.limits.glm));
      expectedLimits = { ...expectedLimits, glm: newLimit - 25_000 };
      await advertisedContext.fill(String(expectedLimits.glm));
      saving = true;
      const [glmSaved] = await Promise.all([
        page.waitForResponse((response) => response.url() === "https://app:4623/api/settings" && response.request().method() === "PATCH"),
        advertisedContext.press("Enter"),
      ]);
      saving = false;
      assert.equal(glmSaved.status(), 200);
      assert.deepEqual((await glmSaved.json()).providerContextLimits, expectedLimits);
      await page.reload();
      await advertisedContext.waitFor();
      await page.waitForFunction((value) => Array.from(document.querySelectorAll('input[type="number"]')).some((input) => input.value === value), String(expectedLimits.glm));
      assert.deepEqual(await readSettings(), { ...initial, limits: expectedLimits });
      assert.equal(patches, 2, "reload must not write settings again");
      assert.deepEqual(blocked, { external: 0, unexpected: 0 }, "unexpected requests must be blocked");
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
});

// Run only in the isolated .ui-browser network with the Dioxus build mounted.
test("Dioxus login, provider creation, and auto-ping persistence work with an isolated backend", {
  skip: process.env.RUN_DIOXUS_BACKEND_TEST !== "1",
  timeout: 90_000,
}, async () => {
  const { chromium } = await import(process.env.DASHBOARD_PLAYWRIGHT_MODULE || "playwright");
  const browser = await chromium.launch({
    headless: true,
    chromiumSandbox: true,
    ...(process.env.DASHBOARD_CHROMIUM_EXECUTABLE ? { executablePath: process.env.DASHBOARD_CHROMIUM_EXECUTABLE } : {}),
  });
  try {
    const context = await browser.newContext({ serviceWorkers: "block" });
    try {
      const fixtureKeyName = `Fixture Integration Key ${Date.now()}`;
      const fixtureProviderName = "Fixture Browser Connection";
      const fixtureProviderKey = "synthetic-provider-fixture-only";
      const newProviderName = "Fixture New Page Connection";
      const glmProviderName = "Fixture Inactive GLM";
      // Generated in memory, sent only to the disposable backend's tmpfs.
      const newProviderKey = `fixture-${randomUUID()}`;
      const glmProviderKey = `fixture-${randomUUID()}`;
      const fixturePassword = "synthetic-browser-only";
      let createdFixtureKey = false;
      let fixtureProviderId;
      let validations = 0;
      let providerCreates = 0;
      let providerUpdates = 0;
      let newProviderCreates = 0;
      let detailNavigations = 0;
      let glmProviderId;
      let glmPatches = 0;
      let blockedApis = 0;
      let blockedExternal = 0;
      await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== "https://app:4623") {
          blockedExternal++;
          return route.abort("blockedbyclient");
        }
        let body;
        try { body = request.postDataJSON(); } catch { body = null; }
        // Provider validation must never reach even the isolated backend: it
        // could otherwise contact a real upstream. Only this fixture is mocked.
        if (request.method() === "POST" && url.pathname === "/api/providers/validate" && !url.search &&
            body?.provider === "openai" && body?.apiKey === fixtureProviderKey && Object.keys(body).length === 2) {
          validations++;
          return route.fulfill({ status: 200, json: { valid: true } });
        }
        if (request.method() === "POST" && url.pathname === "/api/providers" && !url.search &&
            body?.provider === "openai" && body?.name === fixtureProviderName && body?.apiKey === fixtureProviderKey &&
            body?.priority === 1 && body?.proxyPoolId === null && body?.testStatus === "active" && Object.keys(body).length === 6) {
          providerCreates++;
          return route.continue();
        }
        if (request.method() === "POST" && url.pathname === "/api/providers" && !url.search &&
            body?.provider === "deepseek" && body?.name === newProviderName && body?.apiKey === newProviderKey &&
            body?.priority === 1 && body?.testStatus === "unknown" && Object.keys(body).length === 5) {
          newProviderCreates++;
          return route.continue();
        }
        // An inactive synthetic connection exercises the real settings API
        // without starting auto-ping or contacting any provider.
        if (request.method() === "POST" && url.pathname === "/api/providers" && !url.search &&
            body?.provider === "glm" && body?.name === glmProviderName && body?.apiKey === glmProviderKey &&
            Object.keys(body).length === 3) {
          return route.continue();
        }
        if (request.method() === "PUT" && glmProviderId && url.pathname === `/api/providers/${encodeURIComponent(glmProviderId)}` &&
            !url.search && body?.isActive === false && Object.keys(body).length === 1) {
          return route.continue();
        }
        if (request.method() === "PATCH" && url.pathname === "/api/settings" && !url.search && glmProviderId &&
            Object.keys(body ?? {}).length === 1 && body.glmAutoPing?.enabled === true &&
            Object.keys(body.glmAutoPing.connections ?? {}).length === 1 && body.glmAutoPing.connections[glmProviderId] === true) {
          glmPatches++;
          return route.continue();
        }
        if (request.method() === "GET" && url.pathname === "/dashboard/providers/deepseek" && !url.search &&
            request.isNavigationRequest() && newProviderCreates === 1) {
          detailNavigations++;
          return route.fulfill({ status: 200, contentType: "text/html", body: "<!doctype html><h1>Fixture detail destination</h1>" });
        }
        if (request.method() === "PUT" && fixtureProviderId && url.pathname === `/api/providers/${encodeURIComponent(fixtureProviderId)}` &&
            !url.search && body?.isActive === false && Object.keys(body).length === 1) {
          providerUpdates++;
          return route.continue();
        }
        if (request.method() === "POST" && url.pathname === "/api/keys") {
          const name = body?.name;
          if (url.search || !body || Object.keys(body).length !== 1 || (name !== "Default Key" && name !== fixtureKeyName)) {
            blockedApis++;
            return route.abort("blockedbyclient");
          }
          if (name === fixtureKeyName) createdFixtureKey = true;
          return route.continue();
        }
        const allowed = (request.method() === "GET" && (
          (!url.search && ["/login", "/dashboard", "/dashboard/providers", "/dashboard/providers/new", "/dashboard/providers/glm", "/favicon.svg"].includes(url.pathname)) ||
          (url.pathname === "/dashboard/providers" && url.search === "?add=openai") ||
          (!url.search && ["/api/auth/status", "/api/build", "/api/settings", "/api/keys", "/api/providers", "/api/provider-nodes",
            "/api/catalog", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites", "/api/providers/filters"].includes(url.pathname)) ||
          (url.pathname === "/api/models/disabled" && url.search === "?providerAlias=glm") ||
          (url.pathname === "/api/proxy-pools" && url.search === "?isActive=true") ||
          (!url.search && (/^\/providers\/[a-z0-9_-]+\.png$/.test(url.pathname) ||
            url.pathname.startsWith("/assets/") || url.pathname.startsWith("/_astro/")))
        )) || (request.method() === "POST" && url.pathname === "/api/auth/login" && !url.search && body?.password === fixturePassword && Object.keys(body).length === 1);
        if (!allowed && url.pathname.startsWith("/api/")) blockedApis++;
        return allowed ? route.continue() : route.abort("blockedbyclient");
      });
      const page = await context.newPage();
      page.setDefaultTimeout(5_000);
      await page.goto("https://app:4623/login");
      assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "test must run against the Dioxus build");
      const before = await page.evaluate(async () => (await fetch("/api/auth/status")).json());
      assert.equal(before.requireLogin, true);
      assert.equal(before.authenticated, false);
      await page.getByPlaceholder("Enter your password").evaluate((input, value) => {
        input.value = value;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }, fixturePassword);
      await page.getByRole("button", { name: "Sign In" }).click();
      await page.waitForURL("https://app:4623/dashboard");
      const after = await page.evaluate(async () => (await fetch("/api/auth/status")).json());
      assert.equal(after.authenticated, true);
      const cookie = (await context.cookies()).find((item) => item.name === "auth_token");
      assert.equal(cookie?.httpOnly, true);
      assert.equal(cookie?.secure, true);
      assert.equal(cookie?.sameSite, "Lax");
      assert.equal(await page.evaluate(() => document.cookie.includes("auth_token=")), false);
      await page.getByRole("heading", { name: "API Endpoint" }).waitFor();
      await page.getByRole("button", { name: /Create Key/ }).first().click();
      await page.getByLabel("Key Name").fill(fixtureKeyName);
      await page.getByRole("button", { name: "Create", exact: true }).click();
      await page.getByRole("dialog", { name: "API Key Created" }).waitFor();
      await page.getByRole("button", { name: "Done", exact: true }).click();
      await page.getByText(fixtureKeyName, { exact: true }).waitFor();
      assert.equal(createdFixtureKey, true);
      await page.reload();
      await page.getByText(fixtureKeyName, { exact: true }).waitFor();

      // A fresh disposable backend starts without connections. Return only
      // non-secret fields from the authenticated list response.
      const initialProviders = await page.evaluate(async () => {
        const response = await fetch("/api/providers", { cache: "no-store" });
        const data = await response.json();
        return { status: response.status, count: Array.isArray(data.connections) ? data.connections.length : -1 };
      });
      assert.equal(initialProviders.status, 200);
      assert.equal(initialProviders.count, 0, "the disposable backend must start without provider connections");

      await page.goto("https://app:4623/dashboard/providers?add=openai");
      const dialog = page.getByRole("dialog", { name: "Add OpenAI API Key" });
      await dialog.waitFor();
      await dialog.getByLabel("Name", { exact: true }).fill(fixtureProviderName);
      // Avoid putting even fixture credentials in Playwright's action log.
      await dialog.getByLabel("API Key", { exact: true }).evaluate((input, value) => {
        input.value = value;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }, fixtureProviderKey);
      const createResponse = page.waitForResponse((response) =>
        response.url() === "https://app:4623/api/providers" && response.request().method() === "POST");
      await dialog.getByRole("button", { name: "Save", exact: true }).click();
      const createdResponse = await createResponse;
      assert.equal(createdResponse.status(), 201, "provider creation must be handled by the backend");
      const created = (await createdResponse.json()).connection;
      assert.equal(created?.provider, "openai");
      assert.equal(created?.name, fixtureProviderName);
      assert.equal(created?.authType, "apikey");
      assert.equal(created?.isActive, true);
      assert.equal(created?.testStatus, "active");
      assert.equal(typeof created?.id, "string");
      assert.ok(created.id.length > 0);
      fixtureProviderId = created.id;
      await dialog.waitFor({ state: "detached" });
      const openai = page.locator('a[href="/dashboard/providers/openai"]');
      await openai.getByText("1 Connected", { exact: true }).waitFor();
      assert.equal(await openai.getByRole("switch", { name: "Disable provider" }).getAttribute("aria-checked"), "true");

      const updateResponse = page.waitForResponse((response) =>
        response.url() === `https://app:4623/api/providers/${encodeURIComponent(fixtureProviderId)}` && response.request().method() === "PUT");
      await openai.getByRole("switch", { name: "Disable provider" }).click();
      const updatedResponse = await updateResponse;
      assert.equal(updatedResponse.status(), 200, "provider update must be handled by the backend");
      const updated = (await updatedResponse.json()).connection;
      assert.equal(updated?.id, fixtureProviderId);
      assert.equal(updated?.isActive, false);
      await openai.getByText("Disabled").waitFor();
      assert.equal(await openai.getByRole("switch", { name: "Enable provider" }).getAttribute("aria-checked"), "false");
      assert.equal(new URL(page.url()).pathname, "/dashboard/providers", "toggling must not navigate away");

      const reloadList = page.waitForResponse((response) =>
        response.url() === "https://app:4623/api/providers" && response.request().method() === "GET");
      await page.reload();
      const listResponse = await reloadList;
      assert.equal(listResponse.status(), 200, "reload must read providers from the backend");
      const connections = (await listResponse.json()).connections;
      assert.equal(Array.isArray(connections), true);
      assert.equal(connections.length, 1);
      const saved = connections.find((connection) => connection.id === fixtureProviderId);
      assert.equal(saved?.provider, "openai");
      assert.equal(saved?.name, fixtureProviderName);
      assert.equal(saved?.authType, "apikey");
      assert.equal(saved?.hasApiKey, true);
      assert.equal(saved?.isActive, false);
      assert.equal(saved?.testStatus, "active");
      await openai.getByText("Disabled").waitFor();
      assert.equal(await openai.getByRole("switch", { name: "Enable provider" }).getAttribute("aria-checked"), "false");
      assert.equal(validations, 1, "provider validation must be intercepted");
      assert.equal(providerCreates, 1);
      assert.equal(providerUpdates, 1);
      assert.equal(blockedApis, 0, "unexpected API requests must be blocked");
      assert.equal(blockedExternal, 0, "external requests must be blocked");

      // The new page saves directly, without validation or a provider request.
      await page.goto("https://app:4623/dashboard/providers/new");
      await page.getByRole("heading", { name: "Add New Provider" }).waitFor();
      await page.getByRole("combobox").selectOption("apikey:deepseek");
      await page.getByLabel("Display Name").fill(newProviderName);
      await page.getByLabel("API Key", { exact: true }).evaluate((input, value) => {
        input.value = value;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }, newProviderKey);
      const [newCreatedResponse] = await Promise.all([
        page.waitForResponse((response) =>
          response.url() === "https://app:4623/api/providers" && response.request().method() === "POST"),
        page.getByRole("button", { name: "Save API Key" }).click(),
      ]);
      assert.equal(newCreatedResponse.status(), 201, "new-page creation must be handled by the backend");
      await page.waitForURL("https://app:4623/dashboard/providers/deepseek");
      await page.getByRole("heading", { name: "Fixture detail destination" }).waitFor();
      assert.equal(detailNavigations, 1, "save must navigate to the selected provider detail");

      await page.goto("https://app:4623/dashboard/providers/new");
      await page.getByRole("heading", { name: "Add New Provider" }).waitFor();
      // Read only non-secret fields, both before and after reloading the page.
      const readNewProvider = () => page.evaluate(async (name) => {
        const response = await fetch("/api/providers", { cache: "no-store" });
        const data = await response.json();
        const items = Array.isArray(data.connections) ? data.connections : [];
        const connection = items.find((item) => item.provider === "deepseek" && item.name === name);
        return {
          status: response.status, count: items.length, id: connection?.id, provider: connection?.provider,
          name: connection?.name, authType: connection?.authType, isActive: connection?.isActive,
          testStatus: connection?.testStatus, hasApiKey: connection?.hasApiKey,
        };
      }, newProviderName);
      const savedNew = await readNewProvider();
      assert.equal(savedNew.status, 200);
      assert.equal(typeof savedNew.id, "string");
      assert.ok(savedNew.id.length > 0 && savedNew.id !== fixtureProviderId);
      await page.reload();
      await page.getByRole("heading", { name: "Add New Provider" }).waitFor();
      const persistedNew = await readNewProvider();
      assert.equal(persistedNew.status, 200);
      assert.equal(persistedNew.count, 2);
      assert.equal(persistedNew.id, savedNew.id);
      assert.equal(persistedNew.provider, "deepseek");
      assert.equal(persistedNew.name, newProviderName);
      assert.equal(persistedNew.authType, "apikey");
      assert.equal(persistedNew.isActive, true);
      assert.equal(persistedNew.testStatus, "unknown");
      assert.equal(persistedNew.hasApiKey, true);
      assert.equal(newProviderCreates, 1);
      assert.equal(validations, 1, "the new-page save must not validate with a provider");

      glmProviderId = await page.evaluate(async ({ name, key }) => {
        const created = await fetch("/api/providers", {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ provider: "glm", name, apiKey: key }),
        });
        if (created.status !== 201) throw new Error("Could not create synthetic GLM connection");
        return (await created.json()).connection.id;
      }, { name: glmProviderName, key: glmProviderKey });
      assert.ok(typeof glmProviderId === "string" && glmProviderId.length > 0);
      await page.evaluate(async (id) => {
        const response = await fetch(`/api/providers/${encodeURIComponent(id)}`, {
          method: "PUT", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ isActive: false }),
        });
        if (!response.ok || (await response.json()).connection?.isActive !== false) {
          throw new Error("Could not disable synthetic GLM connection");
        }
      }, glmProviderId);

      await page.goto("https://app:4623/dashboard/providers/glm");
      const autoPing = page.getByRole("button", { name: `Enable auto-ping for ${glmProviderName}` });
      await autoPing.waitFor();
      assert.equal(await autoPing.getAttribute("aria-pressed"), "false");
      const [savedSettings] = await Promise.all([
        page.waitForResponse((response) => response.url() === "https://app:4623/api/settings" && response.request().method() === "PATCH"),
        autoPing.click(),
      ]);
      assert.equal(savedSettings.status(), 200);
      assert.equal(glmPatches, 1);
      await page.reload();
      const enabledAutoPing = page.getByRole("button", { name: `Disable auto-ping for ${glmProviderName}` });
      await enabledAutoPing.waitFor();
      assert.equal(await enabledAutoPing.getAttribute("aria-pressed"), "true");
      const persistedAutoPing = await page.evaluate(async () => (await (await fetch("/api/settings", { cache: "no-store" })).json()).glmAutoPing);
      assert.deepEqual(persistedAutoPing, { enabled: true, connections: { [glmProviderId]: true } });
      assert.equal(blockedApis, 0, "unexpected API requests must be blocked");
      assert.equal(blockedExternal, 0, "external requests must be blocked");
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
});
