import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";

// Run only with a fresh .ui-browser Compose project and its empty tmpfs backend.
// The backend prints its startup-seeded key to stderr: redirect its stdout and
// stderr to /dev/null and disable container logging when starting the fixture;
// set DIOXUS_BACKEND_OUTPUT_SUPPRESSED=1 only after both are in place.
test("Dioxus custom model visibility persists across a page reload with an isolated backend", {
  skip: process.env.RUN_DIOXUS_BACKEND_TEST !== "1",
  timeout: 90_000,
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
    const modelId = `fixture-ui-model-${randomUUID()}`;
    const fullModel = `modal/${modelId}`;
    const fixturePassword = "synthetic-browser-only";
    let discoveryKey;
    let phase = "initial";
    let customCreates = 0;
    let disables = 0;
    let restores = 0;
    const blocked = { external: 0, unexpected: 0 };
    try {
      await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== "https://app:4623") {
          blocked.external++;
          return route.abort("blockedbyclient");
        }
        let body;
        try { body = request.postDataJSON(); } catch { body = null; }
        const method = request.method();
        const path = url.pathname;

        if (method === "POST" && path === "/api/auth/login" && !url.search &&
            body?.password === fixturePassword && Object.keys(body).length === 1) {
          return route.continue();
        }
        if (method === "POST" && path === "/api/models/custom" && !url.search && phase === "add" &&
            body?.providerAlias === "modal" && body?.id === modelId && body?.type === "llm" &&
            Object.keys(body).length === 3 && customCreates === 0) {
          customCreates++;
          return route.continue();
        }
        if (method === "POST" && path === "/api/models/disabled" && !url.search && phase === "disable" &&
            body?.providerAlias === "modal" && Array.isArray(body?.ids) && body.ids.length === 1 &&
            body.ids[0] === modelId && Object.keys(body).length === 2 && disables === 0) {
          disables++;
          return route.continue();
        }
        if (method === "DELETE" && path === "/api/models/disabled" && phase === "restore" &&
            url.search === `?providerAlias=modal&id=${encodeURIComponent(modelId)}` && restores === 0) {
          restores++;
          return route.continue();
        }
        if (method === "GET" && path === "/v1/models" && !url.search && discoveryKey &&
            request.headers().authorization === `Bearer ${discoveryKey}`) {
          return route.continue();
        }
        const allowedGet = method === "GET" && (
          (!url.search && ["/login", "/dashboard", "/dashboard/providers/modal", "/favicon.svg", "/providers/modal.png"].includes(path)) ||
          (!url.search && ["/api/auth/status", "/api/build", "/api/settings", "/api/keys", "/api/providers",
            "/api/catalog", "/api/models", "/api/models/custom", "/api/models/alias", "/api/models/favorites",
            "/api/providers/filters"].includes(path)) ||
          (path === "/api/models/disabled" && url.search === "?providerAlias=modal") ||
          (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
          (!url.search && (path.startsWith("/assets/") || path.startsWith("/_astro/")))
        );
        if (allowedGet) return route.continue();
        // This includes all model tests, provider validation/discovery, and batch APIs.
        blocked.unexpected++;
        return route.abort("blockedbyclient");
      });

      const page = await context.newPage();
      await page.goto("https://app:4623/login");
      assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "test must use the Dioxus build");
      await page.getByPlaceholder("Enter your password").evaluate((input, value) => {
        input.value = value;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }, fixturePassword);
      await page.getByRole("button", { name: "Sign In" }).click();
      await page.waitForURL("https://app:4623/dashboard");
      await page.getByRole("heading", { name: "API Endpoint" }).waitFor();

      // The fresh backend seeds this key. Retain it only in memory; never print
      // or assert against the key or whole key response.
      discoveryKey = await page.evaluate(async () => {
        const response = await fetch("/api/keys", { cache: "no-store" });
        if (response.status !== 200) throw new Error("Could not read the isolated discovery key");
        const keys = (await response.json()).keys;
        if (!Array.isArray(keys) || keys.length !== 1 || keys[0].name !== "default" ||
            keys[0].isActive === false || typeof keys[0].key !== "string" || !keys[0].key) {
          throw new Error("Expected one generated isolated discovery key");
        }
        return keys[0].key;
      });
      const initial = await page.evaluate(async () => {
        const [providers, custom, disabled, catalog] = await Promise.all([
          fetch("/api/providers"), fetch("/api/models/custom"),
          fetch("/api/models/disabled?providerAlias=modal"), fetch("/api/catalog"),
        ]);
        const p = await providers.json();
        const c = await custom.json();
        const d = await disabled.json();
        const catalogData = await catalog.json();
        const alias = catalogData.providerIdToAlias?.modal || "modal";
        const models = catalogData.providerModels?.find((row) => row.alias === alias)?.models || [];
        return {
          statuses: [providers.status, custom.status, disabled.status, catalog.status],
          connections: Array.isArray(p.connections) ? p.connections.length : -1,
          custom: Array.isArray(c.models) ? c.models.length : -1,
          ids: d.ids,
          coreModels: models.filter((row) => !row.kind || row.kind === "llm").length,
        };
      });
      assert.deepEqual(initial.statuses, [200, 200, 200, 200]);
      assert.equal(initial.connections, 0, "the disposable backend must have no provider connections");
      assert.equal(initial.custom, 0);
      assert.deepEqual(initial.ids, []);
      assert.equal(initial.coreModels, 0, "Disable All must affect only the fixture custom model");

      const readCustom = () => page.evaluate(async (id) => {
        const response = await fetch("/api/models/custom", { cache: "no-store" });
        const data = await response.json();
        const rows = Array.isArray(data.models) ? data.models.filter((row) => row.id === id) : [];
        return { status: response.status, rows: rows.map(({ providerAlias, id, type, name }) => ({ providerAlias, id, type, name })) };
      }, modelId);
      const readDisabled = () => page.evaluate(async () => {
        const response = await fetch("/api/models/disabled?providerAlias=modal", { cache: "no-store" });
        return { status: response.status, ids: (await response.json()).ids };
      });
      const readDiscovery = () => page.evaluate(async ({ key, id }) => {
        const response = await fetch("/v1/models", { cache: "no-store", headers: { Authorization: `Bearer ${key}` } });
        const data = await response.json();
        const rows = Array.isArray(data.data) ? data.data.filter((row) => row.id === id) : null;
        return { status: response.status, object: data.object, rows: rows?.map((row) => ({
          id: row.id, object: row.object, owned_by: row.owned_by, source: row.opencode?.source,
        })) ?? null };
      }, { key: discoveryKey, id: fullModel });
      const expectedModel = { id: fullModel, object: "model", owned_by: "modal", source: "modal" };
      const expectedCustom = { providerAlias: "modal", id: modelId, type: "llm", name: null };
      assert.deepEqual(await readDiscovery(), { status: 200, object: "list", rows: [] });

      await page.goto("https://app:4623/dashboard/providers/modal");
      await page.getByRole("heading", { name: "Available Models" }).waitFor();
      await page.getByRole("button", { name: /Add Model/ }).click();
      const addDialog = page.getByRole("dialog", { name: "Add Custom Model" });
      await addDialog.getByPlaceholder("e.g. claude-opus-4-5").fill(modelId);
      phase = "add";
      const [created] = await Promise.all([
        page.waitForResponse((response) => response.url() === "https://app:4623/api/models/custom" && response.request().method() === "POST"),
        addDialog.getByRole("button", { name: "Add Model", exact: true }).click(),
      ]);
      assert.equal(created.status(), 200);
      assert.deepEqual(await created.json(), { success: true, added: true });
      await addDialog.waitFor({ state: "detached" });
      await page.getByText(fullModel, { exact: true }).waitFor();
      assert.deepEqual(await readCustom(), { status: 200, rows: [expectedCustom] });
      assert.deepEqual(await readDiscovery(), { status: 200, object: "list", rows: [expectedModel] });

      await page.getByRole("button", { name: /Disable All/ }).click();
      const disableDialog = page.getByRole("dialog", { name: "Disable all models" });
      await disableDialog.getByText("Disable all 1 model(s) for this provider?", { exact: true }).waitFor();
      phase = "disable";
      const [disabled] = await Promise.all([
        page.waitForResponse((response) => response.url() === "https://app:4623/api/models/disabled" && response.request().method() === "POST"),
        disableDialog.getByRole("button", { name: "Disable all", exact: true }).click(),
      ]);
      assert.equal(disabled.status(), 200);
      assert.deepEqual(await disabled.json(), { success: true });
      const restore = page.getByTitle("Restore model").filter({ hasText: modelId });
      await restore.waitFor();
      await page.getByText(fullModel, { exact: true }).waitFor({ state: "detached" });
      assert.deepEqual(await readDisabled(), { status: 200, ids: [modelId] });
      assert.deepEqual(await readCustom(), { status: 200, rows: [expectedCustom] });
      assert.deepEqual(await readDiscovery(), { status: 200, object: "list", rows: [] });

      const reloadedCustom = page.waitForResponse((response) => response.url() === "https://app:4623/api/models/custom" && response.request().method() === "GET");
      const reloadedDisabled = page.waitForResponse((response) => response.url() === "https://app:4623/api/models/disabled?providerAlias=modal" && response.request().method() === "GET");
      await page.reload();
      assert.equal((await reloadedCustom).status(), 200);
      assert.equal((await reloadedDisabled).status(), 200);
      await restore.waitFor();
      assert.equal(await page.getByText(fullModel, { exact: true }).count(), 0);
      assert.deepEqual(await readCustom(), { status: 200, rows: [expectedCustom] });
      assert.deepEqual(await readDisabled(), { status: 200, ids: [modelId] });
      assert.deepEqual(await readDiscovery(), { status: 200, object: "list", rows: [] });

      phase = "restore";
      const [restored] = await Promise.all([
        page.waitForResponse((response) => response.url() === `https://app:4623/api/models/disabled?providerAlias=modal&id=${encodeURIComponent(modelId)}` && response.request().method() === "DELETE"),
        restore.click(),
      ]);
      assert.equal(restored.status(), 200);
      assert.deepEqual(await restored.json(), { success: true });
      await page.getByText(fullModel, { exact: true }).waitFor();
      assert.deepEqual(await readDisabled(), { status: 200, ids: [] });
      assert.deepEqual(await readCustom(), { status: 200, rows: [expectedCustom] });
      assert.deepEqual(await readDiscovery(), { status: 200, object: "list", rows: [expectedModel] });
      assert.equal(customCreates, 1);
      assert.equal(disables, 1);
      assert.equal(restores, 1);
      assert.deepEqual(blocked, { external: 0, unexpected: 0 }, "unexpected network requests were blocked");
    } catch (error) {
      if (blocked.external || blocked.unexpected) {
        console.error(`Blocked unexpected requests: external=${blocked.external}, other=${blocked.unexpected}`);
      }
      throw error;
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
});
