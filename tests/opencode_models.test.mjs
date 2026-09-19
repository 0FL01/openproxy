import assert from "node:assert/strict"
import { createServer } from "node:http"
import { once } from "node:events"
import { test } from "node:test"
import OpenProxyModels from "../plugins/openproxy-models.js"

test("discovery authenticates, refreshes inventory, preserves options and validates metadata atomically", async (t) => {
  let status = 200
  let body = {
    object: "list",
    data: [{
      id: "cx/new/model",
      opencode: {
        name: "New Model",
        source: "codex",
        limit: { context: 628000, input: 450000, output: 128000 },
        modalities: { input: ["text", "image"], output: ["text"] },
        attachment: true,
        reasoning: true,
        variants: { high: { reasoningEffort: "high" } },
        provider: { api: "https://example.invalid" }, // Must never reach the SDK.
      },
    }],
  }
  let requests = 0
  const server = createServer((req, res) => {
    requests++
    assert.equal(req.url, "/v1/models")
    assert.equal(req.headers.authorization, "Bearer fixture-key")
    assert.equal(req.headers["x-openproxy-fixture"], "keep")
    res.writeHead(status, { "Content-Type": "application/json" })
    res.end(JSON.stringify(body))
  }).listen(0, "127.0.0.1")
  await once(server, "listening")
  t.after(() => { server.closeAllConnections(); server.close() })
  const warnings = []
  t.mock.method(console, "warn", (message) => warnings.push(message))
  const options = {
    baseURL: `http://127.0.0.1:${server.address().port}/v1/`,
    apiKey: "fixture-key",
    timeout: false,
    chunkTimeout: 6000000,
    setCacheKey: true,
    headers: { "X-OpenProxy-Fixture": "keep" },
  }
  const config = { provider: {
    ludka2: { npm: "@ai-sdk/openai", options, models: {
      "cx/new/model": { name: "Local name", limit: { context: 900000 } },
      "cx/disabled": { name: "Disabled upstream" },
    } },
    untouched: { models: { "keep-me": {} } },
  } }
  const plugin = await OpenProxyModels()
  await plugin.config(config)
  const models = config.provider.ludka2.models
  assert.deepEqual(Object.keys(models), ["cx/new/model"])
  assert.deepEqual(models["cx/new/model"], {
    name: "Local name · codex",
    limit: { context: 500000, input: 450000, output: 128000 },
    modalities: { input: ["text", "image"], output: ["text"] },
    attachment: true,
    reasoning: true,
    variants: {
      high: { reasoningEffort: "high" },
      none: { disabled: true },
      minimal: { disabled: true },
      low: { disabled: true },
      medium: { disabled: true },
      xhigh: { disabled: true },
      max: { disabled: true },
    },
  })
  assert.strictEqual(config.provider.ludka2.options, options)
  assert.equal(config.provider.ludka2.npm, "@ai-sdk/openai")
  assert.deepEqual(config.provider.untouched.models, { "keep-me": {} })

  status = 401
  body = { error: "fixture-key should not be logged" }
  await plugin.config(config)
  assert.strictEqual(config.provider.ludka2.models, models)
  assert.match(warnings.at(-1), /HTTP 401/)
  assert.ok(warnings.every((message) => !message.includes("fixture-key")))

  status = 200
  body = { object: "list", data: [{ id: "valid" }, { id: "invalid", opencode: { limit: { context: -1 } } }] }
  await plugin.config(config)
  assert.strictEqual(config.provider.ludka2.models, models)
  assert.match(warnings.at(-1), /invalid models response/)

  body = { object: "list", data: [{ id: "opencode-go/added", context_length: 272000, max_completion_tokens: 128000 }] }
  await plugin.config(config)
  assert.deepEqual(config.provider.ludka2.models, {
    "opencode-go/added": { name: "Added", limit: { context: 272000, output: 128000 } },
  })
  config.disabled_providers = ["ludka2"]
  await plugin.config(config)
  assert.equal(requests, 4)
})

test("authoritative empty effort list disables SDK reasoning variants", async (t) => {
  t.mock.method(console, "warn", () => {})
  t.mock.method(globalThis, "fetch", async () => new Response(JSON.stringify({
    object: "list",
    data: [
      { id: "known-none", opencode: { variants: {} } },
      { id: "unknown" },
    ],
  }), { headers: { "Content-Type": "application/json" } }))
  const config = { provider: { ludka2: {
    options: { baseURL: "https://example.invalid/v1", apiKey: "fixture-key" },
  } } }

  await (await OpenProxyModels()).config(config)

  assert.deepEqual(config.provider.ludka2.models["known-none"].variants, Object.fromEntries(
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"].map((name) => [name, { disabled: true }]),
  ))
  assert.equal(Object.hasOwn(config.provider.ludka2.models.unknown, "variants"), false)
})

test("discovery has its own timeout and preserves local models on network failure", async (t) => {
  t.mock.method(console, "warn", () => {})
  t.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.ok(options.signal instanceof AbortSignal)
    assert.equal(options.redirect, "error")
    throw new DOMException("fixture-key", "TimeoutError")
  })
  const models = { existing: { name: "Existing" } }
  const config = { provider: { ludka2: { options: { baseURL: "https://example.invalid/v1", apiKey: "fixture-key", timeout: false }, models } } }
  await (await OpenProxyModels()).config(config)
  assert.strictEqual(config.provider.ludka2.models, models)
})

test("discovery generates readable names without changing model IDs", async (t) => {
  t.mock.method(console, "warn", () => {})
  t.mock.method(globalThis, "fetch", async () => new Response(JSON.stringify({
    object: "list",
    data: [
      { id: "custom-cx/gpt-5.6-luna", opencode: { source: "codex" } },
      { id: "custom-ocg/gpt-5.6-luna", opencode: { source: "opencode-go" } },
      { id: "cx/gpt-5.6-sol-fast", opencode: { source: "codex" } },
      { id: "cx/glm-5.2", opencode: { source: "codex" } },
      { id: "cx/glm-4.6v", opencode: { name: "Glm 4.6v", source: "codex" } },
      { id: "cx/explicit", opencode: { name: "Public Luna", source: "codex" } },
      { id: "cx/same", opencode: { name: "cx/same", source: "codex" } },
      { id: "cx/local", opencode: { source: "codex" } },
    ],
  }), { headers: { "Content-Type": "application/json" } }))
  const config = { provider: { ludka2: {
    options: { baseURL: "https://example.invalid/v1", apiKey: "fixture-key" },
    models: { "cx/local": { name: "User chosen name" } },
  } } }
  await (await OpenProxyModels()).config(config)
  assert.deepEqual(Object.keys(config.provider.ludka2.models), [
    "custom-cx/gpt-5.6-luna", "custom-ocg/gpt-5.6-luna", "cx/gpt-5.6-sol-fast", "cx/glm-5.2", "cx/glm-4.6v", "cx/explicit", "cx/same", "cx/local",
  ])
  assert.equal(config.provider.ludka2.models["custom-cx/gpt-5.6-luna"].name, "GPT-5.6 Luna · codex")
  assert.equal(config.provider.ludka2.models["custom-ocg/gpt-5.6-luna"].name, "GPT-5.6 Luna · opencode-go")
  assert.equal(config.provider.ludka2.models["cx/gpt-5.6-sol-fast"].name, "GPT-5.6 Sol Fast · codex")
  assert.equal(config.provider.ludka2.models["cx/glm-5.2"].name, "GLM 5.2 · codex")
  assert.equal(config.provider.ludka2.models["cx/glm-4.6v"].name, "GLM 4.6v · codex")
  assert.equal(config.provider.ludka2.models["cx/explicit"].name, "Public Luna · codex")
  assert.equal(config.provider.ludka2.models["cx/same"].name, "Same · codex")
  assert.equal(config.provider.ludka2.models["cx/local"].name, "User chosen name · codex")
  await (await OpenProxyModels()).config(config)
  assert.equal(config.provider.ludka2.models["cx/local"].name, "User chosen name · codex")
})
