import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";

// Run only in the isolated .ui-browser network with the Dioxus build mounted.
test("Dioxus login, key creation, provider list, and new-provider save work with an isolated backend", {
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
      // Generated in memory, sent only to the disposable backend's tmpfs.
      const newProviderKey = `fixture-${randomUUID()}`;
      const fixturePassword = "synthetic-browser-only";
      let createdFixtureKey = false;
      let fixtureProviderId;
      let validations = 0;
      let providerCreates = 0;
      let providerUpdates = 0;
      let newProviderCreates = 0;
      let detailNavigations = 0;
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
          (!url.search && ["/login", "/dashboard", "/dashboard/providers", "/dashboard/providers/new", "/favicon.svg"].includes(url.pathname)) ||
          (url.pathname === "/dashboard/providers" && url.search === "?add=openai") ||
          (!url.search && ["/api/auth/status", "/api/build", "/api/settings", "/api/keys", "/api/providers", "/api/provider-nodes"].includes(url.pathname)) ||
          (url.pathname === "/api/proxy-pools" && url.search === "?isActive=true") ||
          (!url.search && (/^\/providers\/[a-z0-9_-]+\.png$/.test(url.pathname) ||
            url.pathname.startsWith("/assets/") || url.pathname.startsWith("/_astro/")))
        )) || (request.method() === "POST" && url.pathname === "/api/auth/login" && !url.search && body?.password === fixturePassword && Object.keys(body).length === 1);
        if (!allowed && url.pathname.startsWith("/api/")) blockedApis++;
        return allowed ? route.continue() : route.abort("blockedbyclient");
      });
      const page = await context.newPage();
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
      assert.equal(blockedApis, 0, "unexpected API requests must be blocked");
      assert.equal(blockedExternal, 0, "external requests must be blocked");
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
});
