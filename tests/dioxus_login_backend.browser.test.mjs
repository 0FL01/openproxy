import assert from "node:assert/strict";
import { test } from "node:test";

// Run only in the isolated .ui-browser network with the Dioxus build mounted.
test("Dioxus login and key creation work with an isolated backend", {
  skip: process.env.RUN_DIOXUS_BACKEND_TEST !== "1",
  timeout: 60_000,
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
      let createdFixtureKey = false;
      await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== "https://app:4623") return route.abort("blockedbyclient");
        if (request.method() === "POST" && url.pathname === "/api/keys") {
          const name = request.postDataJSON()?.name;
          if (name !== "Default Key" && name !== fixtureKeyName) return route.abort("blockedbyclient");
          if (name === fixtureKeyName) createdFixtureKey = true;
          return route.continue();
        }
        const allowed = (request.method() === "GET" && (
          ["/login", "/dashboard", "/favicon.svg", "/api/auth/status", "/api/build", "/api/settings", "/api/keys"].includes(url.pathname) ||
          url.pathname.startsWith("/assets/") ||
          url.pathname.startsWith("/_astro/")
        )) || (request.method() === "POST" && url.pathname === "/api/auth/login");
        return allowed ? route.continue() : route.abort("blockedbyclient");
      });
      const page = await context.newPage();
      await page.goto("https://app:4623/login");
      assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "test must run against the Dioxus build");
      const before = await page.evaluate(async () => (await fetch("/api/auth/status")).json());
      assert.equal(before.requireLogin, true);
      assert.equal(before.authenticated, false);
      await page.getByPlaceholder("Enter your password").fill("synthetic-browser-only");
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
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
});
