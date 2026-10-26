// Optional browser smoke against synthetic responses, never a real OpenProxy.
// Requires Playwright and a sandbox-capable Chromium (see README).
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const publicDir = path.resolve(__dirname, "../target/dx/openproxy-dashboard/release/web/public");
const mime = { ".html": "text/html", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml" };

const server = http.createServer(async (req, res) => {
  const pathname = new URL(req.url, "http://localhost").pathname;
  if (pathname === "/dashboard") {
    res.writeHead(200, { "content-type": "text/html" });
    res.end("<!doctype html><title>synthetic dashboard</title><p>synthetic dashboard</p>");
    return;
  }
  const relative = pathname === "/login" || pathname === "/" ? "index.html" : pathname.slice(1);
  const file = path.resolve(publicDir, relative);
  if (!file.startsWith(publicDir + path.sep)) {
    res.writeHead(403).end();
    return;
  }
  try {
    const body = await fs.readFile(file);
    res.writeHead(200, { "content-type": mime[path.extname(file)] || "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});

async function main() {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({
    executablePath: process.env.CHROME_BIN || undefined,
    chromiumSandbox: true,
    headless: true,
  });
  try {
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.addInitScript(() => localStorage.setItem("theme", JSON.stringify({ state: { theme: "dark" }, version: 0 })));
    const pageErrors = [];
    page.on("pageerror", (err) => pageErrors.push(String(err)));
    await page.route("**/*", (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());

    let state;
    const reset = () => {
      state = {
        status: { requireLogin: true, hasPassword: true, authenticated: false },
        login: { success: true },
        passwordCode: 200,
        password: {},
        settings: {},
        requests: [],
      };
    };
    reset();
    await page.route("**/api/**", async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      const body = request.postDataJSON();
      const headers = await request.allHeaders();
      state.requests.push({ path: url.pathname, method: request.method(), body, cookie: headers.cookie });
      if (url.pathname === "/api/auth/status") {
        if (state.holdStatus) {
          await new Promise((resolve) => {
            state.releaseStatus = resolve;
            state.statusEntered();
          });
        }
        await route.fulfill({ status: state.statusCode || 200, json: state.status, headers: { "set-cookie": "synthetic=1; Path=/; HttpOnly" } }).catch(() => {});
      } else if (url.pathname === "/api/auth/login") {
        if (state.abortLogin) return route.abort("failed");
        if (state.holdLogin) {
          await new Promise((resolve) => {
            state.releaseLogin = resolve;
            state.loginEntered();
          });
        }
        await route.fulfill({ status: state.loginCode || 200, json: state.login, headers: state.loginHeaders || {} });
      } else if (url.pathname === "/api/auth/password") {
        await route.fulfill({ status: state.passwordCode, json: state.password });
      } else if (url.pathname === "/api/settings") {
        await route.fulfill({ status: 200, json: state.settings });
      } else {
        throw new Error(`Unexpected API call ${url.pathname}`);
      }
    });
    const gotoLogin = () => page.goto(`${origin}/login`);
    const password = () => page.getByLabel(/^Password\s*\*$/);

    // The visible asterisk does not prevent an empty password reaching the API.
    state.loginCode = 401;
    state.login = { error: "Empty password rejected by server." };
    await gotoLogin();
    await password().waitFor();
    assert.equal(await password().inputValue(), "");
    await page.getByRole("button", { name: "Sign In" }).click();
    await page.getByText("Empty password rejected by server.").waitFor({ timeout: 5000 });
    const emptyLoginRequest = state.requests.find((r) => r.path === "/api/auth/login");
    assert.equal(emptyLoginRequest?.method, "POST");
    assert.deepEqual(emptyLoginRequest?.body, { password: "" });

    // Status loading and the no-password warning, with no live backend.
    reset();
    state.holdStatus = true;
    state.status.hasPassword = false;
    const statusEntered = new Promise((resolve) => { state.statusEntered = resolve; });
    await gotoLogin();
    await page.getByText("Loading…").waitFor();
    await statusEntered;
    assert.equal(typeof state.releaseStatus, "function");
    state.releaseStatus();
    await page.getByText("Security risk: no password set.", { exact: false }).waitFor();
    assert.equal(await page.title(), "OpenProxy - Login");
    assert.equal(await page.locator("html").evaluate((element) => element.classList.contains("dark")), true);
    if (await password().count() !== 1) {
      throw new Error(`Password label association missing: ${await page.locator("form").innerHTML()}`);
    }
    await password().fill("synthetic-wrong");
    state.loginCode = 429;
    state.login = { error: "Locked", resetHint: "available", retryAfter: 1.2 };
    state.loginHeaders = { "retry-after": "10" };
    await page.getByRole("button", { name: "Sign In" }).click();
    await page.getByText("Locked. Retry in", { exact: false }).waitFor();
    await page.getByText("openproxy auth reset-password").waitFor();
    assert.equal(await password().inputValue(), "");
    assert.equal(await password().isDisabled(), true);
    const loginRequest = state.requests.find((r) => r.path === "/api/auth/login");
    assert.deepEqual(loginRequest.body, { password: "synthetic-wrong" });
    assert.match(loginRequest.cookie, /synthetic=1/);
    await page.getByRole("button", { name: "Sign In" }).waitFor({ timeout: 5000 });
    assert.equal(await password().isEnabled(), true);

    // Forced rotation: validate, fallback on 405, and reload login when sessions expire.
    reset();
    state.login = { success: true, mustChangePassword: true };
    state.passwordCode = 405;
    state.settings = { sessionsInvalidated: true };
    await gotoLogin();
    await password().fill("synthetic-old");
    await page.getByRole("button", { name: "Sign In" }).click();
    const newPassword = page.getByLabel(/^New password\s*\*$/);
    const confirmPassword = page.getByLabel(/^Confirm password\s*\*$/);
    await newPassword.waitFor();
    assert.equal(await newPassword.evaluate((input) => document.activeElement === input), true);
    await newPassword.fill("short");
    await confirmPassword.fill("short");
    await page.getByRole("button", { name: "Set password" }).click();
    await page.getByText("New password must be at least 8 characters long").waitFor();
    await newPassword.fill("synthetic-new");
    await confirmPassword.fill("different");
    await page.getByRole("button", { name: "Set password" }).click();
    await page.getByText("Passwords do not match").waitFor();
    await confirmPassword.fill("synthetic-new");
    await page.getByRole("button", { name: "Set password" }).click();
    await password().waitFor();
    const changes = state.requests.filter((r) => ["/api/auth/password", "/api/settings"].includes(r.path));
    assert.deepEqual(changes.map((r) => [r.path, r.method, r.body]), [
      ["/api/auth/password", "POST", { currentPassword: "synthetic-old", newPassword: "synthetic-new" }],
      ["/api/settings", "PATCH", { currentPassword: "synthetic-old", newPassword: "synthetic-new" }],
    ]);

    // A successful dedicated password update preserves the session and navigates.
    reset();
    state.login = { authenticated: true, mustChangePassword: true };
    await gotoLogin();
    await password().fill("synthetic-old");
    await page.getByRole("button", { name: "Sign In" }).click();
    await newPassword.fill("synthetic-new");
    await confirmPassword.fill("synthetic-new");
    await page.getByRole("button", { name: "Set password" }).click();
    await page.waitForURL(`${origin}/dashboard`);
    assert.equal(state.requests.some((r) => r.path === "/api/settings"), false);

    // Submission displays a disabled loading button while the request is pending.
    reset();
    state.holdLogin = true;
    const loginEntered = new Promise((resolve) => { state.loginEntered = resolve; });
    await gotoLogin();
    await password().fill("synthetic-pass");
    await page.getByRole("button", { name: "Sign In" }).click();
    await loginEntered;
    const submit = page.locator("button[type=submit]");
    assert.equal(await submit.isDisabled(), true);
    assert.equal(await submit.locator("span").textContent(), "progress_activity");
    state.releaseLogin();
    await page.waitForURL(`${origin}/dashboard`);

    // A network failure retains the input; a successful login fully navigates.
    reset();
    state.abortLogin = true;
    await gotoLogin();
    await password().fill("synthetic-pass");
    await page.getByRole("button", { name: "Sign In" }).click();
    await page.getByText("Connection failed. Is the server running?").waitFor();
    assert.equal(await password().inputValue(), "synthetic-pass");
    state.abortLogin = false;
    await page.getByRole("button", { name: "Sign In" }).click();
    await page.waitForURL(`${origin}/dashboard`);

    // Existing sessions bypass the form, and a failed status still shows it.
    reset();
    state.status.authenticated = true;
    await gotoLogin();
    await page.waitForURL(`${origin}/dashboard`);
    reset();
    state.status.requireLogin = false;
    await gotoLogin();
    await page.waitForURL(`${origin}/dashboard`);
    reset();
    state.statusCode = 500;
    await gotoLogin();
    await password().waitFor();
    assert.equal(await page.getByText("Security risk: no password set.", { exact: false }).count(), 0);

    // A status response that never arrives is aborted after five seconds.
    reset();
    state.holdStatus = true;
    const timeoutEntered = new Promise((resolve) => { state.statusEntered = resolve; });
    await gotoLogin();
    await timeoutEntered;
    await password().waitFor({ timeout: 7000 });
    state.releaseStatus();

    // The reference normalizes the legacy zh cookie to zh-CN and translates
    // literal text nodes, including later login and password-change errors.
    reset();
    state.status.hasPassword = false;
    state.loginCode = 401;
    state.login = { error: "Invalid password" };
    await context.addCookies([{ name: "locale", value: "zh", url: origin }]);
    const catalogRequest = page.waitForResponse((response) => response.url() === `${origin}/i18n/literals/zh-CN.json`, { timeout: 5000 });
    await gotoLogin();
    const catalogResponse = await catalogRequest;
    assert.equal(catalogResponse.status(), 200);
    const catalog = await catalogResponse.json();
    assert.equal(catalog.Password, "密码");
    assert.equal(catalog["Invalid password"], "密码错误");
    assert.equal(catalog["Passwords do not match"], "密码不匹配");
    assert.equal(catalog["Sign In"], undefined);
    const chinesePassword = page.getByLabel(/^密码\s*\*$/);
    await chinesePassword.waitFor();
    await page.getByText("输入密码以访问仪表盘", { exact: true }).waitFor();
    await page.getByText("安全风险：未设置密码。远程登录时将要求您设置一个密码。", { exact: true }).waitFor();
    // Neither untranslated literals nor input placeholder attributes are rewritten.
    assert.equal(await chinesePassword.getAttribute("placeholder"), "Enter your password");
    await chinesePassword.fill("synthetic-wrong");
    await page.getByRole("button", { name: "Sign In" }).click();
    await page.getByText("密码错误", { exact: true }).waitFor();

    state.loginCode = 200;
    state.login = { success: true, mustChangePassword: true };
    await chinesePassword.fill("synthetic-old");
    await page.getByRole("button", { name: "Sign In" }).click();
    const chineseNewPassword = page.getByLabel(/^新密码\s*\*$/);
    await chineseNewPassword.waitFor();
    await page.getByText("Choose a new password before continuing", { exact: true }).waitFor();
    await chineseNewPassword.fill("synthetic-new");
    await page.getByLabel(/^Confirm password\s*\*$/).fill("different");
    await page.getByRole("button", { name: "设置密码" }).click();
    await page.getByText("密码不匹配", { exact: true }).waitFor();

    assert.deepEqual(pageErrors, []);
    await context.close();
    console.log("Synthetic login browser smoke passed");
  } finally {
    await browser.close();
    server.close();
  }
}

main().catch((error) => { console.error(error); server.close(); process.exitCode = 1; });
