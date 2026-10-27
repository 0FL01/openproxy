import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";

// Requires a fresh .ui-browser project started with silent-backend.compose.yaml.
test("Dioxus edits and deletes a real compatible node without deleting its connection", {
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
    const password = "synthetic-browser-only";
    const fixtureKey = `fixture-${randomUUID()}`; // Memory and backend tmpfs only.
    const nodeSeed = {
      type: "openai-compatible", name: "Fixture Compatible Node", prefix: "fixture-node",
      baseUrl: "https://initial.invalid/v1", apiType: "chat",
    };
    const nodeEdit = {
      name: "Fixture Compatible Edited", prefix: "fixture-edited",
      baseUrl: "https://edited.invalid/v1", apiType: "responses",
    };
    const connectionName = "Fixture Compatible Connection";
    let nodeId;
    let phase = "initial";
    const writes = { nodes: 0, connections: 0, edits: 0, deletes: 0 };
    const blocked = { external: 0, unexpected: 0 };
    const matches = (actual, expected) => actual && typeof actual === "object" &&
      Object.keys(actual).length === Object.keys(expected).length &&
      Object.entries(expected).every(([key, value]) => actual[key] === value);
    try {
      await context.route("**/*", async (route) => {
        const request = route.request();
        const url = new URL(request.url());
        if (url.origin !== "https://app:4623") {
          blocked.external++;
          return route.abort("blockedbyclient");
        }
        const method = request.method();
        const path = url.pathname;
        let body;
        try { body = request.postDataJSON(); } catch { body = null; }
        if (method === "POST" && path === "/api/auth/login" && !url.search && matches(body, { password })) {
          return route.continue();
        }
        if (method === "POST" && path === "/api/provider-nodes" && !url.search && phase === "seed-node" &&
            writes.nodes === 0 && matches(body, nodeSeed)) {
          writes.nodes++;
          return route.continue();
        }
        if (method === "POST" && path === "/api/providers" && !url.search && phase === "seed-connection" &&
            writes.connections === 0 && matches(body, { provider: nodeId, name: connectionName, apiKey: fixtureKey })) {
          writes.connections++;
          return route.continue();
        }
        if (method === "PUT" && nodeId && path === `/api/provider-nodes/${encodeURIComponent(nodeId)}` &&
            !url.search && phase === "edit" && writes.edits === 0 && matches(body, nodeEdit)) {
          writes.edits++;
          return route.continue();
        }
        if (method === "DELETE" && nodeId && path === `/api/provider-nodes/${encodeURIComponent(nodeId)}` &&
            !url.search && phase === "delete" && writes.deletes === 0 && !request.postData()) {
          writes.deletes++;
          return route.continue();
        }
        const allowedGet = method === "GET" && (
          (!url.search && ["/login", "/dashboard", "/dashboard/providers", "/favicon.svg"].includes(path)) ||
          (!url.search && nodeId && path === `/dashboard/providers/${encodeURIComponent(nodeId)}`) ||
          (!url.search && ["/api/auth/status", "/api/build", "/api/settings", "/api/keys", "/api/providers",
            "/api/provider-nodes", "/api/catalog", "/api/models", "/api/models/custom", "/api/models/alias",
            "/api/models/favorites", "/api/providers/filters"].includes(path)) ||
          (nodeId && path === "/api/models/disabled" && url.search === `?providerAlias=${encodeURIComponent(nodeId)}`) ||
          (path === "/api/proxy-pools" && url.search === "?isActive=true") ||
          (!url.search && (/^\/providers\/[a-z0-9_-]+\.png$/.test(path) || path.startsWith("/assets/") || path.startsWith("/_astro/")))
        );
        if (allowedGet) return route.continue();
        // Validation, provider discovery, connection tests, and model tests all fail closed.
        blocked.unexpected++;
        return route.abort("blockedbyclient");
      });

      const page = await context.newPage();
      page.setDefaultTimeout(5_000);
      page.setDefaultNavigationTimeout(7_000);
      await page.goto("https://app:4623/login");
      assert.equal(await page.locator('script[src*="/assets/openproxy-dashboard-"]').count(), 1, "test must use the Dioxus build");
      await page.getByPlaceholder("Enter your password").evaluate((input, value) => {
        input.value = value;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }, password);
      await page.getByRole("button", { name: "Sign In" }).click();
      await page.waitForURL("https://app:4623/dashboard");
      await page.getByRole("heading", { name: "API Endpoint" }).waitFor();

      const initial = await page.evaluate(async () => {
        const [nodeResponse, connectionResponse] = await Promise.all([
          fetch("/api/provider-nodes", { cache: "no-store" }), fetch("/api/providers", { cache: "no-store" }),
        ]);
        const nodes = (await nodeResponse.json()).nodes;
        const connections = (await connectionResponse.json()).connections;
        return {
          nodeStatus: nodeResponse.status, connectionStatus: connectionResponse.status,
          nodes: Array.isArray(nodes) ? nodes.length : -1,
          connections: Array.isArray(connections) ? connections.length : -1,
        };
      });
      assert.deepEqual(initial, { nodeStatus: 200, connectionStatus: 200, nodes: 0, connections: 0 }, "backend must be fresh");

      phase = "seed-node";
      const createdNode = await page.evaluate(async (body) => {
        const response = await fetch("/api/provider-nodes", {
          method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
        });
        const node = (await response.json()).node;
        return { status: response.status, node: node && {
          id: node.id, type: node.type, name: node.name, prefix: node.prefix, baseUrl: node.baseUrl, apiType: node.apiType,
        } };
      }, nodeSeed);
      assert.equal(createdNode.status, 201);
      nodeId = createdNode.node?.id;
      assert.match(nodeId || "", /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i, "backend must assign a UUID");
      assert.deepEqual(createdNode.node, { id: nodeId, ...nodeSeed });

      phase = "seed-connection";
      const createdConnection = await page.evaluate(async (body) => {
        const response = await fetch("/api/providers", {
          method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
        });
        const connection = (await response.json()).connection;
        return { status: response.status, id: connection?.id, provider: connection?.provider, name: connection?.name, authType: connection?.authType };
      }, { provider: nodeId, name: connectionName, apiKey: fixtureKey });
      assert.equal(createdConnection.status, 201);
      assert.equal(typeof createdConnection.id, "string");
      assert.ok(createdConnection.id.length > 0);
      assert.equal(createdConnection.provider, nodeId);
      assert.equal(createdConnection.name, connectionName);
      assert.equal(createdConnection.authType, "apikey");
      const connectionId = createdConnection.id;

      const readState = () => page.evaluate(async ({ nodeId, connectionId }) => {
        const [nodeResponse, connectionResponse] = await Promise.all([
          fetch("/api/provider-nodes", { cache: "no-store" }), fetch("/api/providers", { cache: "no-store" }),
        ]);
        const nodes = (await nodeResponse.json()).nodes;
        const connections = (await connectionResponse.json()).connections;
        const node = Array.isArray(nodes) ? nodes.find((item) => item.id === nodeId) : null;
        const connection = Array.isArray(connections) ? connections.find((item) => item.id === connectionId) : null;
        return {
          nodeStatus: nodeResponse.status, connectionStatus: connectionResponse.status,
          nodeCount: Array.isArray(nodes) ? nodes.length : -1,
          connectionCount: Array.isArray(connections) ? connections.length : -1,
          node: node ? { id: node.id, type: node.type, name: node.name, prefix: node.prefix, baseUrl: node.baseUrl, apiType: node.apiType } : null,
          connection: connection ? { id: connection.id, provider: connection.provider, name: connection.name,
            authType: connection.authType, isActive: connection.isActive, hasApiKey: connection.hasApiKey } : null,
        };
      }, { nodeId, connectionId });
      const expectedConnection = { id: connectionId, provider: nodeId, name: connectionName, authType: "apikey", isActive: true, hasApiKey: true };
      const expectedSeed = { nodeStatus: 200, connectionStatus: 200, nodeCount: 1, connectionCount: 1,
        node: { id: nodeId, ...nodeSeed }, connection: expectedConnection };
      const expectedEdit = { ...expectedSeed, node: { id: nodeId, type: nodeSeed.type, ...nodeEdit } };
      assert.deepEqual(await readState(), expectedSeed);

      await page.goto("https://app:4623/dashboard/providers");
      const card = page.locator(`a[href="/dashboard/providers/${nodeId}"]`);
      await card.getByText(nodeSeed.name, { exact: true }).waitFor();
      await card.click();
      await page.waitForURL(`https://app:4623/dashboard/providers/${nodeId}`);
      // Make an ID-contract failure explicit rather than disguising it as a timeout.
      await page.waitForFunction(() => document.body.innerText.includes("OpenAI Compatible Details") || document.body.innerText.includes("Provider not found"));
      assert.equal(await page.getByText("Provider not found", { exact: true }).count(), 0,
        "a real backend-created compatible UUID must open its detail page");
      const detailsHeading = page.getByRole("heading", { name: "OpenAI Compatible Details", exact: true });
      await detailsHeading.waitFor();
      await page.getByText(connectionName, { exact: true }).waitFor();
      const details = detailsHeading.locator("../..");
      await details.getByRole("button", { name: /Edit/ }).click();
      const editDialog = page.getByRole("dialog", { name: "Edit OpenAI Compatible" });
      await editDialog.getByLabel("Name", { exact: true }).fill(nodeEdit.name);
      await editDialog.getByLabel("Prefix", { exact: true }).fill(nodeEdit.prefix);
      await editDialog.getByLabel("API Type", { exact: true }).selectOption(nodeEdit.apiType);
      await editDialog.getByLabel("Base URL", { exact: true }).fill(nodeEdit.baseUrl);
      assert.ok((await editDialog.getByLabel("API Key (for Check)").inputValue()).length === 0, "check credential field must be empty");
      assert.equal(await editDialog.getByRole("button", { name: "Check", exact: true }).isDisabled(), true);
      phase = "edit";
      const [edited] = await Promise.all([
        page.waitForResponse((response) => response.url() === `https://app:4623/api/provider-nodes/${nodeId}` && response.request().method() === "PUT"),
        editDialog.getByRole("button", { name: "Save", exact: true }).click(),
      ]);
      assert.equal(edited.status(), 200);
      const saved = (await edited.json()).node;
      assert.deepEqual({ id: saved.id, type: saved.type, name: saved.name, prefix: saved.prefix, baseUrl: saved.baseUrl, apiType: saved.apiType }, expectedEdit.node);
      await editDialog.waitFor({ state: "detached" });
      await page.getByRole("heading", { name: nodeEdit.name, exact: true }).waitFor();
      await page.getByText(`Responses API · ${nodeEdit.baseUrl}/responses`, { exact: true }).waitFor();
      assert.deepEqual(await readState(), expectedEdit);

      const nodesReloaded = page.waitForResponse((response) => response.url() === "https://app:4623/api/provider-nodes" && response.request().method() === "GET");
      await page.reload();
      assert.equal((await nodesReloaded).status(), 200);
      await page.getByRole("heading", { name: nodeEdit.name, exact: true }).waitFor();
      await page.getByText(connectionName, { exact: true }).waitFor();
      await page.getByText(`Responses API · ${nodeEdit.baseUrl}/responses`, { exact: true }).waitFor();
      assert.deepEqual(await readState(), expectedEdit);
      await details.getByRole("button", { name: /Edit/ }).click();
      assert.equal(await editDialog.getByLabel("Name", { exact: true }).inputValue(), nodeEdit.name);
      assert.equal(await editDialog.getByLabel("Prefix", { exact: true }).inputValue(), nodeEdit.prefix);
      assert.equal(await editDialog.getByLabel("API Type", { exact: true }).inputValue(), nodeEdit.apiType);
      assert.equal(await editDialog.getByLabel("Base URL", { exact: true }).inputValue(), nodeEdit.baseUrl);
      await editDialog.getByRole("button", { name: "Cancel", exact: true }).click();
      await editDialog.waitFor({ state: "detached" });

      await details.getByRole("button", { name: /Delete/ }).click();
      const deleteDialog = page.getByRole("dialog", { name: "Delete compatible node" });
      await deleteDialog.getByText(nodeEdit.name, { exact: true }).waitFor();
      phase = "delete";
      const [deleted] = await Promise.all([
        page.waitForResponse((response) => response.url() === `https://app:4623/api/provider-nodes/${nodeId}` && response.request().method() === "DELETE"),
        deleteDialog.getByRole("button", { name: "Delete", exact: true }).click(),
      ]);
      assert.equal(deleted.status(), 200);
      // The UI immediately navigates after the response headers. Reading the
      // interrupted response body can hang; the independent read-back below
      // proves the node was deleted and its connection retained.
      await page.waitForURL("https://app:4623/dashboard/providers");
      await page.getByText("No custom providers — use buttons above to add OpenAI/Anthropic compatible endpoints", { exact: true }).waitFor();
      assert.deepEqual(await readState(), { ...expectedEdit, nodeCount: 0, node: null });
      assert.equal(await page.locator(`a[href="/dashboard/providers/${nodeId}"]`).count(), 0);
      assert.deepEqual(writes, { nodes: 1, connections: 1, edits: 1, deletes: 1 });
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
