import assert from "node:assert/strict";
import { test } from "node:test";

// Requires Playwright + Chromium and an isolated server serving the current web/dist:
// pnpm --dir web run build
// pnpm --dir web exec astro preview --host 127.0.0.1 --port 14624
// DASHBOARD_TEST_URL=http://127.0.0.1:14624 node --test tests/xai_models.browser.test.mjs
// Every API call is intercepted; these tests never use real credentials or generation.
const options = { skip: !process.env.DASHBOARD_TEST_URL };
const json = (route, body, status = 200) => route.fulfill({
  status, contentType: "application/json", body: JSON.stringify(body),
});

test("xAI catalog badges/context/efforts and manual refresh preserve user overlays without imports", options, async () => {
  const { chromium } = await import("playwright");
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    const connections = [
      { id: "inactive", provider: "xai", name: "Inactive", authType: "apikey", isActive: false, priority: -100 },
      { id: "canonical-first", provider: "xai", name: "Canonical first", authType: "apikey", priority: 90 },
      { id: "priority-first", provider: "xai", name: "Priority first", authType: "apikey", isActive: true, priority: 0 },
    ];
    const customModels = [
      { providerAlias: "xai", id: "grok-vision", name: "My Vision" },
      { providerAlias: "xai", id: "grok-disabled", name: "My disabled model" },
      { providerAlias: "xai", id: "grok-custom-reasoning", name: "Unknown custom capabilities" },
    ];
    let disabled = ["grok-disabled"];
    const writes = [];
    const forceSources = [];
    let refreshes = 0;
    let catalogRequests = 0;
    const catalog = () => ({
      providerIdToAlias: { xai: "xai" },
      providerModels: [{ alias: "xai", models: [
        { id: "grok-vision", name: "Official Vision", kind: "llm", contextWindow: 256000, capabilities: ["vision"], reasoningEfforts: null },
        { id: "grok-reasoning", kind: "llm", contextWindow: null, capabilities: refreshes >= 2 ? [] : ["reasoning"], reasoningEfforts: refreshes >= 2 ? null : ["low", "high"] },
        { id: "grok-none", kind: "llm", contextWindow: null, capabilities: [], reasoningEfforts: ["none"] },
        ...(refreshes >= 2 ? [{ id: "grok-disabled", kind: "llm", capabilities: [], reasoningEfforts: null }] : []),
      ] }],
      xaiCatalog: refreshes === 1 ? { status: "stale", warning: "xAI upstream temporarily unavailable" } : { status: "ready" },
    });
    await page.route("**/api/**", async route => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.method() !== "GET") writes.push({ path: url.pathname, method: request.method() });
      if (url.pathname === "/api/auth/status") return json(route, { requireLogin: false, authenticated: true });
      if (url.pathname === "/api/providers") return json(route, { connections });
      if (url.pathname === "/api/catalog") {
        catalogRequests += 1;
        return json(route, catalog());
      }
      if (/^\/api\/providers\/[^/]+\/models$/.test(url.pathname)) {
        forceSources.push(url.pathname);
        refreshes += 1;
        return refreshes === 1
          ? json(route, { error: "upstream failure" }, 503)
          : json(route, { models: [{ id: "must-not-auto-import" }] });
      }
      if (url.pathname === "/api/models/custom") return json(route, { models: customModels });
      if (url.pathname === "/api/models/alias") return json(route, { aliases: { personal: "xai/grok-vision" } });
      if (url.pathname === "/api/models/disabled") {
        if (request.method() === "DELETE") disabled = disabled.filter(id => id !== url.searchParams.get("id"));
        return json(route, { ids: disabled });
      }
      if (url.pathname === "/api/models") return json(route, { models: [
        { provider: "openrouter", model: "grok-vision", fullModel: "openrouter/grok-vision", caps: { vision: true, reasoning: true } },
        { provider: "xai", model: "grok-vision", fullModel: "xai/grok-vision", caps: { reasoning: true } },
      ] });
      return json(route, {});
    });
    await page.goto(`${process.env.DASHBOARD_TEST_URL}/dashboard/providers/xai`);
    const refresh = page.getByRole("button", { name: "Refresh models", exact: true });
    await refresh.waitFor();
    const row = id => page.locator("div.group").filter({ has: page.locator("code").filter({ hasText: new RegExp(`^xai/${id}(?:\\(|$)`) }) });
    const vision = row("grok-vision");
    await vision.getByText("My Vision", { exact: true }).waitFor();
    assert.equal(await vision.getByText("visibility", { exact: true }).count(), 1);
    assert.equal(await vision.getByText("neurology", { exact: true }).count(), 0, "exact catalog caps beat API collisions and Grok guesses");
    assert.equal(await vision.getByText("256K ctx", { exact: true }).count(), 1);
    assert.equal(await vision.locator('[title="Context window: 256,000 tokens"]').count(), 1);
    assert.equal(await row("grok-reasoning").getByText(/ctx$/).count(), 0, "unknown context has no fabricated default");
    assert.equal(await row("grok-custom-reasoning").getByText("neurology", { exact: true }).count(), 0);
    assert.equal(await row("grok-disabled").count(), 0, "disabled custom is not active");
    assert.equal(await page.getByRole("button", { name: /grok-disabled/ }).count(), 1, "disabled custom is restorable");
    const picker = page.locator('select[title="Appends (level) suffix to copied model names"]').last();
    assert.deepEqual(await picker.locator("option").evaluateAll(options => options.map(option => option.value)), ["auto", "none", "low", "high"]);
    await picker.selectOption("high");
    assert.equal(await row("grok-reasoning").locator("code").textContent(), "xai/grok-reasoning(high)");
    assert.equal(await vision.locator("code").textContent(), "xai/grok-vision", "suffix only applies to a matching model");
    assert.deepEqual(forceSources, [], "page entry never force-fetches a connection");
    assert.deepEqual(writes, [], "catalog discovery does not import or generate");

    const beforeFailure = catalogRequests;
    await refresh.click();
    await page.getByRole("alert").filter({ hasText: /model refresh failed/ }).waitFor();
    await refresh.waitFor();
    assert.ok(catalogRequests > beforeFailure, "even a failed force must reload the backend catalog");
    assert.deepEqual(forceSources, ["/api/providers/canonical-first/models"], "source uses first active canonical vector, not priority; missing active defaults true");
    assert.equal(await vision.count(), 1, "last-good rows survive failure");

    await refresh.click();
    await page.waitForFunction(() => {
      const picker = [...document.querySelectorAll('select[title="Appends (level) suffix to copied model names"]')].pop();
      return picker?.value === "auto" && ![...picker.options].some(option => option.value === "high");
    });
    await refresh.waitFor();
    assert.deepEqual(await picker.locator("option").evaluateAll(options => options.map(option => option.value)), ["auto", "none"]);
    assert.equal(await row("grok-reasoning").getByText("neurology", { exact: true }).count(), 0, "caps react to catalog changes");
    assert.equal(await row("grok-reasoning").locator("code").textContent(), "xai/grok-reasoning");
    assert.equal(await page.getByRole("alert").count(), 0, "successful refresh clears page-local warning");
    assert.equal(await page.getByText(/must-not-auto-import/).count(), 0, "force result is not a second inventory");
    assert.equal(await row("grok-disabled").count(), 0);
    assert.equal(await page.getByRole("button", { name: /grok-disabled/ }).count(), 1, "catalog overlap stays disabled and deduplicated");
    assert.deepEqual(writes, []);

    await picker.selectOption("none");
    assert.equal(await row("grok-none").locator("code").textContent(), "xai/grok-none(none)", "declared none remains selectable");
    await page.getByRole("button", { name: /grok-disabled/ }).click();
    await row("grok-disabled").getByText("My disabled model", { exact: true }).waitFor();
    assert.deepEqual(writes, [{ path: "/api/models/disabled", method: "DELETE" }], "only explicit restore mutates saved models");
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});

test("failed xAI connections requests are not presented as no active source", options, async () => {
  const { chromium } = await import("playwright");
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    let providersRequests = 0;
    let catalogRequests = 0;
    let forceRequests = 0;
    await page.route("**/api/**", route => {
      const path = new URL(route.request().url()).pathname;
      if (path === "/api/auth/status") return json(route, { requireLogin: false, authenticated: true });
      if (path === "/api/providers") return ++providersRequests === 1 ? json(route, {}) : json(route, {}, 503);
      if (path === "/api/catalog") {
        catalogRequests += 1;
        return json(route, { providerModels: [{ alias: "xai", models: [] }] });
      }
      if (/\/providers\/[^/]+\/models$/.test(path)) forceRequests += 1;
      return json(route, {});
    });
    await page.goto(`${process.env.DASHBOARD_TEST_URL}/dashboard/providers/xai`);
    await page.getByText("Connections could not be loaded.", { exact: true }).waitFor();
    assert.equal(await page.getByText("No connections yet", { exact: true }).count(), 0);
    assert.equal(await page.getByRole("alert").filter({ hasText: /No active/ }).count(), 0);
    const refresh = page.getByRole("button", { name: "Refresh models", exact: true });
    await refresh.waitFor();
    const before = catalogRequests;
    await refresh.click();
    await page.getByRole("alert").filter({ hasText: /Could not load xAI connections/ }).waitFor();
    assert.ok(catalogRequests > before);
    assert.equal(forceRequests, 0);
    assert.equal(await page.getByRole("alert").filter({ hasText: /No active/ }).count(), 0);
  } finally {
    await browser.close();
  }
});

test("queued initial xAI catalog reload wins and empty inventory differs from a removed source", options, async () => {
  const { chromium } = await import("playwright");
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    let active = true;
    let empty = false;
    let firstCatalog;
    let catalogRequests = 0;
    let forceRequests = 0;
    const catalog = id => ({
      providerIdToAlias: { xai: "xai" },
      providerModels: [{ alias: "xai", models: id ? [{ id, kind: "llm", capabilities: [] }] : [] }],
      ...(active ? { xaiCatalog: { status: "ready" } } : {}),
    });
    await page.route("**/api/**", route => {
      const path = new URL(route.request().url()).pathname;
      assert.equal(route.request().method(), "GET", "discovery must not mutate user models or generate");
      if (path === "/api/auth/status") return json(route, { requireLogin: false, authenticated: true });
      if (path === "/api/providers") return json(route, { connections: active ? [{ id: "only-source", provider: "xai", name: "Only source", authType: "apikey" }] : [] });
      if (path === "/api/catalog") {
        catalogRequests += 1;
        if (catalogRequests === 1) { firstCatalog = route; return; }
        return json(route, catalog(empty ? null : "grok-current"));
      }
      if (path === "/api/models/custom") return json(route, { models: [{ providerAlias: "xai", id: "saved-custom", name: "Saved custom" }] });
      if (path === "/api/providers/only-source/models") {
        forceRequests += 1;
        empty = true;
        return json(route, { models: [] });
      }
      return json(route, {});
    });
    await page.goto(`${process.env.DASHBOARD_TEST_URL}/dashboard/providers/xai`);
    await page.getByRole("status").filter({ hasText: "Loading xAI model catalog" }).waitFor();
    await page.waitForFunction(() => document.querySelector('button[aria-label="Refreshing models..."]')?.disabled);
    assert.ok(firstCatalog);
    await json(firstCatalog, catalog("grok-obsolete"));
    await page.locator("code").filter({ hasText: /^xai\/grok-current$/ }).waitFor();
    assert.equal(await page.locator("code").filter({ hasText: /^xai\/grok-obsolete$/ }).count(), 0);
    assert.equal(catalogRequests, 2, "page entry queues exactly one follow-up behind the captured initial response");
    const refresh = page.getByRole("button", { name: "Refresh models", exact: true });
    await refresh.waitFor();
    await refresh.click();
    await page.getByRole("status").filter({ hasText: "returned no chat/vision models" }).waitFor();
    await page.getByText("Saved custom", { exact: true }).waitFor();
    assert.equal(await page.getByRole("alert").count(), 0, "valid empty is not a discovery failure");
    active = false;
    await refresh.click();
    await page.getByRole("alert").filter({ hasText: /No active xAI connection/ }).waitFor();
    await page.getByText("Saved custom", { exact: true }).waitFor();
    assert.equal(await page.getByRole("status").filter({ hasText: "returned no chat/vision models" }).count(), 0);
    assert.equal(forceRequests, 1, "no removed account or fallback account is force-fetched");
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});
