import assert from "node:assert/strict";
import http from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";

// Optional built-dashboard gate; Playwright is a local test tool, not a dependency.
// RUN_TPS_BROWSER_TESTS=1 node --test tests/request_tps_stats.browser.test.mjs
// Optional: DASHBOARD_PLAYWRIGHT_MODULE, DASHBOARD_CHROMIUM_EXECUTABLE.
test("TPS visibility, provider preferences and scoped polling survive reload/errors", {
  skip: process.env.RUN_TPS_BROWSER_TESTS !== "1",
  timeout: 120_000,
}, async () => {
  const { chromium } = await import(process.env.DASHBOARD_PLAYWRIGHT_MODULE || "playwright");
  const candidate = process.env.TPS_DASHBOARD_DIST;
  const dist = candidate ? resolve(candidate) : fileURLToPath(new URL("../web/dist/", import.meta.url));
  const server = http.createServer(async (req, res) => {
    try {
      const path = new URL(req.url, "http://localhost").pathname;
      const target = resolve(dist, path === "/dashboard/logs" ? (candidate ? "index.html" : "dashboard/logs.html") : `.${path}`);
      assert(target.startsWith(`${dist.replace(/\/$/, "")}${sep}`));
      const body = await readFile(target);
      res.writeHead(200, { "Content-Type": ({ ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".wasm": "application/wasm" })[extname(target)] || "application/octet-stream" });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  let browser;
  try {
    browser = await chromium.launch({ headless: true, chromiumSandbox: true, ...(process.env.DASHBOARD_CHROMIUM_EXECUTABLE ? { executablePath: process.env.DASHBOARD_CHROMIUM_EXECUTABLE } : {}) });
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    const key = "openproxy.logs.tps-preferences.v1";
    const calls = [];
    let mode = "ok", responseDelay = 0, groupCount = 25;
    const asOf = "2026-10-12T20:00:00.000Z";
    const windows = Object.fromEntries([1, 6, 24].map((h) => [`${h}h`, { start: new Date(Date.parse(asOf) - h * 3_600_000).toISOString(), end: asOf }]));
    const identity = (index) => [null, "", "null", " p "][index] ?? (index === 0 ? null : `p${index}`);
    const group = (index, provider = identity(index)) => {
      const measured = index !== 1 && index !== 24;
      const priority = index === 22 ? 1 : index === 23 ? 2 : 0;
      return { provider, model: index === 0 ? null : index === 1 ? "" : `m${index}`, windows: Object.fromEntries(["1h", "6h", "24h"].map((w, window) => {
        const hasSamples = measured && window >= priority;
        return [w, {
          medianTps: hasSamples ? (index === 0 ? 0 : index === 22 ? 1000 : index === 23 ? 2000 : index + 0.05) : null,
          sampleCount: hasSamples ? 18 : 0, successfulAttemptCount: 20,
        }];
      })) };
    };
    const compareId = (a, b) => a === b ? 0 : a === null ? -1 : b === null ? 1 : a < b ? -1 : 1;
    const rank = (group) => {
      const priority = ["1h", "6h", "24h"].findIndex((window) => group.windows[window].sampleCount > 0);
      return priority === -1 ? [3, 0] : [priority, group.windows[["1h", "6h", "24h"][priority]].medianTps];
    };
    const stats = (params) => {
      const pageNumber = Number(params.get("page")), pageSize = Number(params.get("pageSize"));
      const provider = params.get("provider");
      let all = provider ? [group(2, provider)] : Array.from({ length: groupCount }, (_, i) => group(i));
      all.push({ ...group(25, "search-only"), model: "standalone-web-search" });
      all = all.filter((g) => g.model !== "standalone-web-search");
      const model = params.get("model");
      if (model) all = all.filter((g) => g.model?.includes(model));
      const availableProviders = [...new Set(all.map((g) => g.provider))].sort(compareId);
      const hidden = JSON.parse(params.get("hiddenProviders") || "[]");
      all = all.filter((g) => !hidden.includes(g.provider)).sort((a, b) => {
        const left = rank(a), right = rank(b);
        return left[0] - right[0] || right[1] - left[1] || compareId(a.provider, b.provider) || compareId(a.model, b.model);
      });
      return { asOf, windows, availableProviders, groups: all.slice((pageNumber - 1) * pageSize, pageNumber * pageSize), pagination: { page: pageNumber, pageSize, totalItems: all.length, totalPages: Math.ceil(all.length / pageSize) } };
    };
    const raw = { requestId: "raw-fixture", timestamp: asOf, route: "fixture", provider: "raw-provider", model: "raw-model", status: "success", durationMs: 500, inputTokens: 32, outputTokens: 9, cachedTokens: 6, reasoningTokens: 94, totalTokens: 135, tokensPerSecond: 18, generatedOutputTokens: 9, upstreamDurationMs: 500, apiKeyId: "consumer", apiKeyName: "Fixture" };
    const legacy = { requestId: "legacy-fixture", timestamp: asOf, route: "fixture", provider: "raw-provider", model: "legacy-model", status: "success", durationMs: 500, tokensPerSecond: null, generatedOutputTokens: null, upstreamDurationMs: null };
    const zero = { ...raw, requestId: "zero-fixture", model: "zero-model", inputTokens: 0, outputTokens: 0, cachedTokens: 0, reasoningTokens: 0, totalTokens: 0, tokensPerSecond: 0, generatedOutputTokens: 0 };
    const origin = `http://127.0.0.1:${server.address().port}`;
    await page.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    await page.route("**/api/**", async (route) => {
      const url = new URL(route.request().url());
      if (url.origin !== origin) return route.abort();
      let status = 200, body = {};
      if (url.pathname === "/api/auth/status") body = { requireLogin: false, authenticated: true };
      else if (url.pathname === "/api/keys") body = { keys: [{ id: "consumer", name: "Fixture" }] };
      else if (url.pathname === "/api/request-logs") body = { requests: [raw, legacy, zero], pagination: { page: Number(url.searchParams.get("page")), pageSize: 20, totalItems: 50, totalPages: 3 } };
      else if (url.pathname === "/api/request-logs/tps-stats") {
        calls.push(Object.fromEntries(url.searchParams));
        const capturedMode = mode, capturedDelay = responseDelay;
        body = stats(url.searchParams);
        if (capturedMode === "malformed") body.availableProviders = ["duplicate", "duplicate"];
        if (capturedMode === "fail") { status = 503; body = { error: { code: "tps_stats_unavailable" } }; }
        if (capturedDelay) await delay(capturedDelay);
      }
      try { await route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) }); } catch { /* cancelled obsolete request */ }
    });
    await page.clock.install({ time: new Date(asOf) });
    const url = `${origin}/dashboard/logs`;
    const panel = page.getByRole("region", { name: "Median TPS statistics" });
    const table = panel.getByRole("table");
    const choices = panel.getByRole("group", { name: "Providers (TPS only)" });
    const hide = page.getByRole("checkbox", { name: "Hide Median TPS", exact: true });
    const pagination = page.getByRole("navigation", { name: "TPS statistics pagination" });
    const refresh = page.getByRole("button", { name: "Refresh", exact: false });
    const tick = async (ms = 1) => { await page.clock.runFor(ms); await delay(40); };
    const waitCalls = async (count) => {
      for (let i = 0; i < 100 && calls.length < count; i++) { await tick(); }
      assert(calls.length >= count, `expected ${count} calls, got ${calls.length}`);
    };
    const prefs = () => page.evaluate((key) => JSON.parse(localStorage.getItem(key)), key);
    const reload = async () => { await page.reload(); await hide.waitFor(); await tick(301); };
    await page.goto(url);
    await panel.locator("time").waitFor();
    await page.clock.pauseAt(new Date(Date.parse(asOf) + 5_000));
    assert.equal(await table.locator("tbody tr").count(), 20);
    assert.match(await table.locator("tbody tr").first().innerText(), /p21.*m21.*21\.1.*18\/20/s);
    assert.deepEqual(await table.locator("tbody tr td:first-of-type").allTextContents(), Array.from({ length: 20 }, (_, index) => `m${21 - index}`));
    assert.equal(await table.getByText("standalone-web-search", { exact: true }).count(), 0);
    assert.equal(await choices.getByRole("checkbox", { name: "search-only", exact: true }).count(), 0);
    await panel.getByText("Sorted by highest-priority available window:", { exact: false }).waitFor();
    assert.equal(await choices.getByRole("checkbox").count(), 25);
    assert.equal(await hide.isChecked(), false);
    assert.equal(await panel.locator("time").getAttribute("datetime"), asOf);
    console.log("TPS browser: default controls, zero/coverage/asOf and complete provider choices passed");

    const rawTable = page.getByRole("table").nth(1);
    const rawRow = (model) => rawTable.getByRole("row").filter({ has: page.getByText(model, { exact: true }) });
    const tokens = (model) => rawRow(model).locator("td").nth(5);
    assert.equal(await tokens("raw-model").innerText(), "32 / 9\nTotal 135");
    assert.equal(await tokens("legacy-model").innerText(), "— / —");
    assert.equal(await tokens("zero-model").innerText(), "0 / 0\nTotal 0");
    const tokenCard = page.getByRole("heading", { name: "Tokens", exact: true }).locator("..");
    const fieldValue = (label) => tokenCard.getByText(label, { exact: true }).locator("..").locator("div").nth(1).innerText();
    for (const [model, expected] of [
      ["raw-model", ["32", "9", "6", "94", "135"]],
      ["legacy-model", ["—", "—", "—", "—", "—"]],
      ["zero-model", ["0", "0", "0", "0", "0"]],
    ]) {
      await rawRow(model).click(); await tick(301);
      await page.getByRole("heading", { name: "Request details", exact: true }).waitFor();
      assert.deepEqual(await Promise.all(["Input", "Output", "Cache Read", "Reasoning", "Total"].map(fieldValue)), expected);
      await tokenCard.getByText("Cache read and reasoning may overlap input/output; do not sum these fields to calculate Total.", { exact: false }).waitFor();
      await page.getByText("Reasoning is included only when the original provider completion counter includes it.", { exact: false }).waitFor();
      if (model === "raw-model") await page.getByText("9 ÷ (500 / 1000) = 18.0 tokens/s", { exact: true }).waitFor();
      if (model === "zero-model") await page.getByText("0 ÷ (500 / 1000) = 0.0 tokens/s", { exact: true }).waitFor();
      if (model === "legacy-model") await page.getByText("— · Final original usage and completion timing are unavailable.", { exact: true }).waitFor();
      await page.keyboard.press("Escape"); await tick(301);
      assert.equal(await page.getByRole("heading", { name: "Request details", exact: true }).count(), 0);
    }
    console.log("TPS browser: selected-attempt token breakdown, known total, legacy dashes, true zero and unchanged TPS formula passed");

    await pagination.getByRole("button", { name: "Next page" }).click();
    await tick();
    await table.getByText("m24", { exact: true }).waitFor();
    assert.equal(await table.locator("tbody tr").count(), 5);
    assert.deepEqual(await table.locator("tbody tr td:first-of-type").allTextContents(), ["Unknown", "m22", "m23", "Empty", "m24"]);
    assert.match(await table.locator("tbody tr").first().innerText(), /Unknown.*Unknown.*0\.0.*18\/20/s);
    await choices.getByRole("checkbox", { name: "p4", exact: true }).uncheck();
    await tick();
    await panel.locator("time").waitFor();
    assert.equal(calls.at(-1).page, "1");
    assert.deepEqual(JSON.parse(calls.at(-1).hiddenProviders), ["p4"]);
    assert.equal(await table.getByText("m4", { exact: true }).count(), 0);
    assert.equal(await choices.getByRole("checkbox").count(), 25);
    assert.deepEqual(await prefs(), { hidden: false, hiddenProviders: ["p4"] });
    await reload();
    await panel.locator("time").waitFor();
    assert.equal(await choices.getByRole("checkbox", { name: "p4", exact: true }).isChecked(), false);
    assert.deepEqual(JSON.parse(calls.at(-1).hiddenProviders), ["p4"]);
    console.log("TPS browser: exclusion resets paging, remains re-enableable and survives reload passed");

    await hide.check();
    assert.equal(await panel.count(), 0);
    let before = calls.length;
    await tick(120_001); await refresh.click();
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await tick();
    assert.equal(calls.length, before);
    await page.getByText("raw-model", { exact: true }).waitFor();
    await reload();
    assert.equal(await hide.isChecked(), true);
    assert.equal(await panel.count(), 0);
    await tick(60_001);
    assert.equal(calls.length, before);
    await hide.uncheck(); await tick(301);
    await panel.locator("time").waitFor();
    assert.equal(await choices.getByRole("checkbox", { name: "p4", exact: true }).isChecked(), false);
    console.log("TPS browser: persisted whole-panel hide sends no requests, raw logs remain usable passed");

    for (const checkbox of await choices.getByRole("checkbox").all()) await checkbox.uncheck();
    await tick();
    await panel.getByText("All matching providers are hidden", { exact: true }).waitFor();
    assert.equal(await choices.getByRole("checkbox").count(), 25);
    await reload();
    await panel.getByText("All matching providers are hidden", { exact: true }).waitFor();
    await choices.getByRole("checkbox", { name: "Unknown", exact: true }).check(); await tick();
    await panel.locator("time").waitFor();
    let saved = await prefs();
    assert(!saved.hiddenProviders.includes(null));
    assert(saved.hiddenProviders.includes("null"));
    assert(saved.hiddenProviders.includes(""));
    await choices.getByRole("checkbox", { name: "Empty", exact: true }).check(); await tick();
    await choices.getByRole("checkbox", { name: "null", exact: true }).check(); await tick();
    await choices.getByRole("checkbox", { name: "p", exact: true }).check(); await tick();
    await panel.locator("time").waitFor();
    saved = await prefs();
    for (const id of [null, "", "null", " p "]) assert(!saved.hiddenProviders.includes(id));
    console.log("TPS browser: all-hidden reload/recovery and exact nullable/empty/literal/space IDs passed");

    mode = "fail";
    await choices.getByRole("checkbox", { name: "null", exact: true }).uncheck(); await tick();
    await panel.getByText("Unavailable", { exact: true }).waitFor();
    assert.equal(await choices.getByRole("checkbox").count(), 25);
    await choices.getByRole("checkbox", { name: "p4", exact: true }).check(); await tick();
    await panel.getByText("Unavailable", { exact: true }).waitFor();
    assert.equal(await choices.getByRole("checkbox", { name: "p4", exact: true }).isChecked(), true);
    before = calls.length; await tick(60_001); await waitCalls(before + 1);
    mode = "malformed"; await refresh.click();
    await panel.getByText("Invalid TPS statistics response", { exact: false }).waitFor();
    assert.equal(await choices.getByRole("checkbox").count(), 25);
    mode = "ok"; await refresh.click();
    await panel.locator("time").waitFor();
    for (const checkbox of await choices.getByRole("checkbox").all()) await checkbox.check();
    await tick(); await panel.locator("time").waitFor();
    assert.deepEqual((await prefs()).hiddenProviders, []);
    console.log("TPS browser: choices usable during 503/malformed responses, minute retry and recovery passed");

    responseDelay = 500; before = calls.length;
    await choices.getByRole("checkbox", { name: "p10", exact: true }).uncheck();
    await waitCalls(before + 1);
    responseDelay = 0;
    await choices.getByRole("checkbox", { name: "p10", exact: true }).check(); await tick();
    await table.getByText("m10", { exact: true }).waitFor();
    await delay(550);
    assert.equal(await table.getByText("m10", { exact: true }).count(), 1);
    assert.deepEqual((await prefs()).hiddenProviders, []);
    before = calls.length;
    await page.getByLabel("Status (raw logs only)").selectOption("error");
    const rawCard = page.getByRole("table").nth(1).locator("..").locator("..");
    await rawCard.getByRole("button", { name: "Next page", exact: true }).click(); await tick();
    assert.equal(calls.length, before);
    await pagination.getByRole("button", { name: "Next page" }).click(); await tick();
    await table.getByText("m24", { exact: true }).waitFor();
    before = calls.length; await tick(60_001); await waitCalls(before + 1);
    assert.equal(calls.at(-1).page, "2");
    assert(!("status" in calls.at(-1)));
    await page.evaluate(() => { window.__visible = false; Object.defineProperty(document, "visibilityState", { configurable: true, get: () => window.__visible ? "visible" : "hidden" }); document.dispatchEvent(new Event("visibilitychange")); });
    before = calls.length; await tick(120_001); assert.equal(calls.length, before);
    await page.evaluate(() => { window.__visible = true; document.dispatchEvent(new Event("visibilitychange")); });
    await waitCalls(before + 1); await tick();
    mode = "fail"; await refresh.click();
    await panel.getByText("Stale", { exact: true }).waitFor();
    assert.equal(await table.locator("tbody tr").count(), 5);
    assert.equal(await panel.locator("time").getAttribute("datetime"), asOf);
    mode = "ok"; responseDelay = 300; before = calls.length;
    await refresh.click(); await waitCalls(before + 1);
    await refresh.click(); await refresh.click();
    await page.evaluate(() => window.dispatchEvent(new Event("focus"))); await tick();
    assert.equal(calls.length, before + 1);
    await delay(350); responseDelay = 0;
    console.log("TPS browser: obsolete exclusions, raw-log independence, later-page/visibility polling and coalescing passed");

    groupCount = 1; before = calls.length;
    await refresh.click(); await waitCalls(before + 2);
    await table.getByText("Unknown", { exact: true }).first().waitFor();
    assert.equal(calls.at(-1).page, "1");
    groupCount = 0; await refresh.click();
    await panel.getByText("No saved successful attempts", { exact: false }).waitFor();
    groupCount = 26; await refresh.click();
    await choices.getByRole("checkbox", { name: "p25", exact: true }).waitFor();
    assert.equal(await choices.getByRole("checkbox", { name: "p25", exact: true }).isChecked(), true);
    await pagination.getByRole("combobox", { name: "Rows per page" }).selectOption("10"); await tick();
    await panel.locator("time").waitFor();
    assert.equal(calls.at(-1).page, "1"); assert.equal(calls.at(-1).pageSize, "10");
    const provider = page.getByPlaceholder("Provider", { exact: true });
    before = calls.length;
    await provider.fill("A"); await provider.fill("AB"); await provider.fill("  NEW  ");
    assert.equal(await panel.locator("time").count(), 0);
    assert.equal(calls.length, before); await tick(301); await waitCalls(before + 1);
    await table.getByText("NEW", { exact: true }).waitFor();
    assert.equal(calls.at(-1).provider, "NEW");
    assert.equal(await choices.getByRole("checkbox").count(), 1);
    await provider.fill(""); await tick(301); await panel.locator("time").waitFor();
    await page.setViewportSize({ width: 375, height: 812 });
    const scroll = await table.evaluate((node) => ({ width: node.parentElement.clientWidth, scroll: node.parentElement.scrollWidth }));
    assert(scroll.scroll > scroll.width);
    assert.equal(await pagination.getByRole("combobox", { name: "Rows per page" }).count(), 1);
    console.log("TPS browser: successful page correction, new-provider defaults, filter scope/debounce and mobile controls passed");

    await page.evaluate((key) => localStorage.setItem(key, "{broken"), key); await reload();
    await panel.locator("time").waitFor();
    assert.deepEqual(await prefs(), { hidden: false, hiddenProviders: [] });
    await page.addInitScript((key) => {
      const read = Storage.prototype.getItem, write = Storage.prototype.setItem;
      Storage.prototype.getItem = function (name) { if (name === key) throw new DOMException("blocked", "SecurityError"); return read.call(this, name); };
      Storage.prototype.setItem = function (name, value) { if (name === key) throw new DOMException("blocked", "SecurityError"); return write.call(this, name, value); };
    }, key);
    await reload(); await panel.locator("time").waitFor();
    await hide.check(); assert.equal(await panel.count(), 0);
    await hide.uncheck(); await tick(301); await panel.locator("time").waitFor();
    await choices.getByRole("checkbox", { name: "p4", exact: true }).uncheck(); await tick();
    assert.equal(await choices.getByRole("checkbox", { name: "p4", exact: true }).isChecked(), false);
    assert.deepEqual(errors, []);
    console.log("TPS browser: malformed/blocked preference storage keeps controls functional, zero page exceptions passed");
  } finally {
    await browser?.close();
    await new Promise((done) => server.close(done));
  }
});
