import assert from "node:assert/strict";
import { test } from "node:test";

// Optional live-image gate: npm install --no-save --package-lock=false playwright
// DASHBOARD_TEST_URL=http://127.0.0.1:14625 DASHBOARD_TEST_COMMIT=<full SHA> node --test tests/dashboard_build_commit.browser.test.mjs
test("live dashboard displays backend commit and handles missing metadata", {
  skip: !process.env.DASHBOARD_TEST_URL,
}, async () => {
  const { chromium } = await import("playwright");
  const expected = process.env.DASHBOARD_TEST_COMMIT;
  assert.match(expected, /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/);
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    if (process.env.DASHBOARD_TEST_PASSWORD) {
      const response = await page.request.post(`${process.env.DASHBOARD_TEST_URL}/api/auth/login`, {
        data: { password: process.env.DASHBOARD_TEST_PASSWORD },
      });
      assert.equal(response.status(), 200);
    }
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`${process.env.DASHBOARD_TEST_URL}/dashboard/endpoint`, { waitUntil: "networkidle" });
    const commits = page.locator('aside a[href="/dashboard"] span[title]');
    const commit = commits.first();
    await commit.waitFor();
    await page.waitForFunction(expected => {
      const node = document.querySelector('aside a[href="/dashboard"] span[title]');
      return node?.textContent === expected.slice(0, 8);
    }, expected);
    assert.equal(await commit.textContent(), expected.slice(0, 8));
    assert.equal(await commit.getAttribute("title"), `Commit: ${expected}`);
    for (const text of await commits.allTextContents()) assert.equal(text, expected.slice(0, 8));
    for (const text of await page.locator('aside a[href="/dashboard"] h1').allTextContents()) assert.equal(text, "OpenProxy");
    for (const text of await page.locator("aside").allTextContents()) assert.ok(!text.includes("v0.3.0"));
    assert.ok(await page.getByRole("button", { name: /Shutdown/ }).count());
    assert.deepEqual(errors, []);

    for (const fixture of [{ commit: null }, { commit: "0.3.0" }, "unauthorized"]) {
      await page.route("**/api/build", route => route.fulfill({
        status: fixture === "unauthorized" ? 401 : 200,
        contentType: "application/json",
        body: JSON.stringify(fixture),
      }));
      await page.reload({ waitUntil: "networkidle" });
      assert.equal(await commit.textContent(), "—");
      assert.equal(await commit.getAttribute("title"), "Build commit unavailable");
      await page.unroute("**/api/build");
    }
  } finally {
    await browser.close();
  }
});
