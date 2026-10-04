import assert from "node:assert/strict";
import { test } from "node:test";

// DASHBOARD_TEST_URL=http://127.0.0.1:14624 node --test tests/provider_connections.browser.test.mjs
test("provider connections survive delayed and invalid responses", {
  skip: !process.env.DASHBOARD_TEST_URL,
}, async () => {
  const { chromium } = await import("playwright");
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    const account = {
      id: "codex-test", provider: "codex", name: "Regression account",
      authType: "oauth", isActive: true, testStatus: "active", priority: 1,
    };
    let providersRequests = 0;
    let delayedProviders;
    let delayedNodes;
    const json = (route, body) => route.fulfill({
      contentType: "application/json", body: JSON.stringify(body),
    });
    await page.route("**/api/**", async route => {
      const path = new URL(route.request().url()).pathname;
      if (path === "/api/auth/status") {
        return json(route, { requireLogin: false, authenticated: true });
      }
      if (path === "/api/providers") {
        providersRequests += 1;
        if (providersRequests === 2) {
          delayedProviders = route;
          return;
        }
        if (providersRequests === 4) return json(route, {});
        return json(route, { connections: providersRequests === 5 ? [] : [account] });
      }
      if (path === "/api/provider-nodes" && !delayedNodes) {
        delayedNodes = route;
        return;
      }
      if (path.endsWith("/test")) return json(route, { valid: true });
      return json(route, {});
    });
    await page.goto(`${process.env.DASHBOARD_TEST_URL}/dashboard/providers/codex`);
    const row = page.getByRole("checkbox", { name: "Select connection Regression account" });
    await row.waitFor();
    // Connections render without waiting for the ancillary nodes request.
    assert.equal(providersRequests, 1, "must not request connections for an empty provider ID");
    assert.ok(delayedNodes);
    await json(delayedNodes, { nodes: [] });

    const runTest = page.getByRole("button", { name: /Test Connection One-by-One$/ });
    const firstRefresh = page.waitForRequest(request => new URL(request.url()).pathname === "/api/providers");
    await runTest.click();
    await firstRefresh;
    await runTest.waitFor();
    // A second refresh finishes before the first one.
    const secondRefresh = page.waitForResponse(response => response.url().endsWith("/api/providers"));
    await runTest.click();
    await secondRefresh;
    assert.ok(delayedProviders);
    const staleResponse = page.waitForResponse(response => response.url().endsWith("/api/providers"));
    await json(delayedProviders, { connections: [] });
    await staleResponse;
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await row.count(), 1, "stale empty response must not remove the account");

    const invalidResponse = page.waitForResponse(response => response.url().endsWith("/api/providers"));
    await runTest.click();
    await invalidResponse;
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await row.count(), 1, "malformed response must preserve the account");

    await runTest.click();
    await row.waitFor({ state: "detached" });
    assert.equal(providersRequests, 5, "a current empty list must still remove the account");
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});
