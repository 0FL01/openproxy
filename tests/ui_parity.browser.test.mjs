import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

// Run inside the isolated .ui-browser Compose network, never against production.
// UI_PARITY_MODE=record captures synthetic reference images; compare is the default.
const enabled = process.env.RUN_UI_PARITY_TESTS === "1";
const mode = process.env.UI_PARITY_MODE || "compare";
const selectedScreen = process.env.UI_PARITY_SCREEN;
const referenceDir = resolve(process.env.UI_REFERENCE_DIR || "tests/ui/reference");
const artifactDir = resolve(process.env.UI_ARTIFACT_DIR || ".ui-browser/artifacts");
const appUrl = "https://app:4623";
const fixtureCommit = "0123456789abcdef0123456789abcdef01234567";

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
];
const viewports = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
];

test("dashboard states match the synthetic visual reference", { skip: !enabled, timeout: 480_000 }, async (t) => {
  assert(["record", "compare"].includes(mode), "UI_PARITY_MODE must be record or compare");
  assert(!selectedScreen || screens.some((screen) => `${screen.section}-${screen.name}` === selectedScreen), "unknown UI_PARITY_SCREEN");
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
    for (const viewport of viewports) {
      for (const theme of ["light", "dark"]) {
        for (const screen of screens) {
          if (selectedScreen && `${screen.section}-${screen.name}` !== selectedScreen) continue;
          if (screen.mobileOnly && viewport.name !== "mobile") continue;
          if (screen.colorScheme && screen.colorScheme !== theme) continue;
          const name = `${screen.section}-${screen.name}-${viewport.name}-${theme}`;
          await t.test(name, async () => {
            assert(mode !== "record" || process.env.UI_PARITY_REPLACE === "1" || !existsSync(join(referenceDir, `${name}.png`)), "reference already exists; set UI_PARITY_REPLACE=1 deliberately to replace it");
            const capture = async () => {
              const context = await browser.newContext({
                viewport: { width: viewport.width, height: viewport.height },
                deviceScaleFactor: 1,
                colorScheme: theme,
                locale: "en-US",
                timezoneId: "UTC",
                serviceWorkers: "block",
              });
              const errors = [];
              const writes = [];
              const unexpected = [];
              let scenarioReady = false;
              const expectedWrites = screen.login
                ? [{ method: "POST", path: "/api/auth/login", body: { password: "fixture-password" } }]
                : screen.action === "created"
                  ? [{ method: "POST", path: "/api/keys", body: { name: "Fixture new" } }]
                  : [];
              try {
                const page = await context.newPage();
                if (screen.storedTheme) {
                  await page.addInitScript((theme) => localStorage.setItem("theme", JSON.stringify({ state: { theme }, version: 0 })), screen.storedTheme);
                }
                let created = false;
                let authenticated = screen.section === "endpoint";
                page.on("pageerror", (error) => errors.push(error.message));
                await page.clock.setFixedTime(new Date("2026-10-01T12:00:00Z"));
                await context.route("**/*", async (route) => {
                  const request = route.request();
                  const url = new URL(request.url());
                  const path = url.pathname;
                  const method = request.method();
                  const assets = path === "/login" || path === "/dashboard/endpoint" || path === "/favicon.svg" || path.startsWith("/_astro/") || path.startsWith("/assets/") || path.startsWith("/i18n/");
                  if (url.origin === appUrl && method === "GET" && assets) return route.continue();
                  const valid = url.origin === appUrl && (
                    (method === "GET" && ["/api/auth/status", "/api/settings", "/api/keys", "/api/build", "/api/catalog"].includes(path)) ||
                    (method === "POST" && path === "/api/auth/login" && !!screen.login) ||
                    (method === "POST" && path === "/api/keys" && screen.action === "created")
                  );
                  if (!valid) {
                    unexpected.push(`${method} ${url.origin}${path}`);
                    return route.abort("blockedbyclient");
                  }
                  if (method !== "GET") writes.push({ method, path, body: request.postDataJSON() });
                  let status = 200;
                  let body = {};
                  const headers = {};
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
                  } else if (path === "/api/settings") {
                    body = { requireLogin: true, requireApiKey: !screen.insecure, hasPassword: true };
                  } else if (path === "/api/build") {
                    body = { commit: fixtureCommit };
                  } else if (path === "/api/catalog") {
                    body = { providerIdToAlias: {}, providerModels: [], xaiCatalog: { status: "ready" } };
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
                });

                const path = screen.section === "login" ? "/login" : "/dashboard/endpoint";
                const response = await page.goto(`${appUrl}${path}`, { waitUntil: "domcontentloaded" });
                assert(response?.ok(), `failed to load ${path}`);
                if (mode === "record") {
                  const component = screen.section === "login" ? "LoginPageClient." : "EndpointPageClient.";
                  const html = await response.body();
                  assert(html.includes(`component-url="/_astro/${component}`), "record mode requires the Astro reference");
                  const hash = createHash("sha256").update(html).digest("hex");
                  assert(!source.routes[path] || source.routes[path] === hash, `Astro reference changed for ${path}`);
                  source.routes[path] = hash;
                  await writeFile(sourcePath, JSON.stringify(source, null, 2) + "\n");
                } else {
                  assert(source.routes[path], `missing Astro reference identity for ${path}`);
                  assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "compare mode requires the Dioxus candidate");
                }
                if (screen.section === "login") await page.getByRole("heading", { name: "OpenProxy" }).waitFor();
                else await page.getByText("Fixture key", { exact: true }).waitFor();
                await page.addStyleTag({ content: "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}" });
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

                scenarioReady = true;
                return await page.screenshot({ fullPage: true, animations: "disabled" });
              } finally {
                await context.close();
                assert.deepEqual(unexpected, [], "unexpected network requests were blocked");
                if (scenarioReady) assert.deepEqual(writes, expectedWrites, "unexpected API mutation");
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
