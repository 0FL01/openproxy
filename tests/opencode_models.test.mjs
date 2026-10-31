import assert from "node:assert/strict"
import { once } from "node:events"
import { createServer } from "node:http"
import { registerHooks } from "node:module"
import { test } from "node:test"

// OpenCode supplies this package at runtime. Keep standalone Node tests free of
// dependencies and import the unmodified plugin through an identity define.
const hooks = registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "@opencode/plugin") {
      return { url: `data:text/javascript,${encodeURIComponent("export const Plugin = { define: (plugin) => plugin }")}`, shortCircuit: true }
    }
    return next(specifier, context)
  },
})
const { default: discovery } = await import("../plugins/openproxy-models.js").finally(() => hooks.deregister())

function environment(t, url = "https://example.invalid/v1", key = "fixture-key") {
  for (const [name, value] of Object.entries({ LUDKA2_API_URL: url, LUDKA2_API_KEY: key })) {
    const previous = process.env[name]
    process.env[name] = value
    t.after(() => {
      if (previous === undefined) delete process.env[name]
      else process.env[name] = previous
    })
  }
}

function nativeModel(id, overrides = {}) {
  return {
    id, providerID: "ludka2", name: id,
    limit: {}, capabilities: { input: ["text"], output: ["text"], tools: false },
    variants: [], settings: {}, ...overrides,
  }
}

// Only the model editor contract is simulated: no V2 service/lifecycle harness.
function context(initial = []) {
  const state = { models: structuredClone(initial), registrations: 0, disposals: 0 }
  state.ctx = { model: { async transform(edit) {
    state.registrations++
    edit({
      list: (providerID) => state.models.filter((model) => model.providerID === providerID),
      remove: (providerID, id) => { state.models = state.models.filter((model) => model.providerID !== providerID || model.id !== id) },
      update(providerID, id, update) {
        let model = state.models.find((model) => model.providerID === providerID && model.id === id)
        if (!model) { model = nativeModel(id, { providerID }); state.models.push(model) }
        update(model)
      },
    })
    return { dispose: async () => { state.disposals++ } }
  } } }
  state.get = (id) => state.models.find((model) => model.providerID === "ludka2" && model.id === id)
  return state
}

const response = (data) => new Response(JSON.stringify({ object: "list", data }))

test("V2 setup authenticates catalog discovery and registers one safe model transform", async (t) => {
  assert.equal(discovery.id, "openproxy.models")
  assert.deepEqual(Object.keys(discovery), ["id", "setup"])
  let requests = 0
  const server = createServer((req, res) => {
    requests++
    assert.equal(req.url, "/v1/models")
    assert.equal(req.headers.authorization, "Bearer fixture-key")
    assert.equal(req.headers.accept, "application/json")
    assert.equal(req.headers["x-openproxy-claude-mask"], undefined)
    res.writeHead(200, { "Content-Type": "application/json" })
    res.end(JSON.stringify({ object: "list", data: [{
      id: "not-codex/glm-5.3",
      context_length: 200000,
      max_completion_tokens: 64000,
      opencode: {
        name: "Glm 5.3 · CODEX · codex", source: "codex",
        limit: { context: 628000, input: 900000, output: 128000 },
        modalities: { input: ["text", "image"], output: ["text"] },
        attachment: true, reasoning: true, tool_call: true,
        variants: {
          high: { reasoningEffort: "high", settings: { baseURL: "https://untrusted.invalid" } },
          none: { disabled: true },
        },
        package: "untrusted-package", sdk: "untrusted-sdk",
        provider: { api: "https://untrusted.invalid" },
        headers: { "X-Untrusted": "ignore" }, options: { apiKey: "ignore" },
        settings: { baseURL: "https://untrusted.invalid" },
      },
    }, { id: "new/gpt-6-luna", opencode: { source: "a6api" } }] }))
  }).listen(0, "127.0.0.1")
  await once(server, "listening")
  t.after(() => { server.closeAllConnections(); server.close() })
  environment(t, `http://127.0.0.1:${server.address().port}/v1/`)
  const timeout = t.mock.method(AbortSignal, "timeout")
  const warnings = t.mock.method(console, "warn", () => {})
  const existing = nativeModel("not-codex/glm-5.3", { settings: { reasoningEffort: "low" } })
  const untouched = nativeModel("keep-me", { providerID: "untouched" })
  const state = context([existing, nativeModel("removed"), untouched])

  const dispose = await discovery.setup(state.ctx)

  assert.equal(requests, 1)
  assert.equal(state.registrations, 1)
  assert.deepEqual(timeout.mock.calls.map((call) => call.arguments[0]), [15000])
  assert.equal(warnings.mock.callCount(), 0)
  assert.deepEqual(state.get(existing.id), {
    ...existing,
    name: "GLM 5.3 · codex",
    limit: { context: 500000, input: 500000, output: 128000 },
    capabilities: { input: ["text", "image"], output: ["text"], tools: true },
    variants: [{ id: "high", settings: { reasoningEffort: "high" } }],
  })
  assert.equal(state.get("removed"), undefined)
  assert.equal(state.get("new/gpt-6-luna").name, "GPT-6 Luna · a6api")
  assert.deepEqual(state.models.find((model) => model.providerID === "untouched"), untouched)
  await dispose()
  assert.equal(state.disposals, 1)
})

test("V2 metadata merges only complete limits, preserves unknown variants and never infers source", async (t) => {
  environment(t)
  t.mock.method(globalThis, "fetch", async () => response([
    { id: "legacy/glm-5.3", context_length: 1048576, max_completion_tokens: 131072 },
    { id: "merged", context_length: 204800, opencode: { tool_call: true } },
    { id: "partial", opencode: { limit: { context: 400000 } } },
    { id: "known-none", opencode: { variants: {} } },
  ]))
  const variant = { id: "native", settings: { reasoningEffort: "medium" } }
  const state = context([
    nativeModel("merged", { limit: { output: 32768 }, capabilities: { input: ["audio"], output: ["text"], tools: false }, variants: [variant] }),
    nativeModel("known-none", { variants: [variant] }),
  ])

  await discovery.setup(state.ctx)

  assert.equal(state.get("legacy/glm-5.3").name, "GLM 5.3")
  assert.deepEqual(state.get("legacy/glm-5.3").limit, { context: 500000, output: 131072 })
  assert.deepEqual(state.get("merged").limit, { context: 204800, output: 32768 })
  assert.deepEqual(state.get("merged").capabilities, { input: ["audio"], output: ["text"], tools: true })
  assert.deepEqual(state.get("merged").variants, [variant])
  assert.deepEqual(state.get("partial").limit, {})
  assert.deepEqual(state.get("known-none").variants, [])
})

test("invalid configuration, HTTP failure and invalid catalogs leave native models untouched without secrets", async (t) => {
  environment(t)
  const warnings = []
  t.mock.method(console, "warn", (message) => warnings.push(message))
  let reply = () => new Response("fixture-key https://example.invalid", { status: 401 })
  const fetch = t.mock.method(globalThis, "fetch", async () => reply())
  const initial = [nativeModel("existing")]
  const state = context(initial)

  process.env.LUDKA2_API_URL = "https://example.invalid/v1?token=fixture-key"
  assert.equal(await discovery.setup(state.ctx), undefined)
  assert.equal(fetch.mock.callCount(), 0)
  assert.equal(warnings.at(-1), "[openproxy-models] invalid provider URL or credentials; keeping native models.")

  process.env.LUDKA2_API_URL = "https://example.invalid/v1"
  assert.equal(await discovery.setup(state.ctx), undefined)
  assert.equal(fetch.mock.callCount(), 1)
  assert.equal(warnings.at(-1), "[openproxy-models] model discovery failed; keeping native models.")

  for (const data of [
    [{ id: "valid" }, { id: "invalid", opencode: { source: " codex " } }],
    [{ id: "duplicate" }, { id: "duplicate" }],
  ]) {
    reply = () => response(data)
    assert.equal(await discovery.setup(state.ctx), undefined)
    assert.equal(warnings.at(-1), "[openproxy-models] invalid models response; keeping native models.")
  }
  assert.equal(state.registrations, 0)
  assert.deepEqual(state.models, initial)
  assert.ok(warnings.every((message) => !message.includes("fixture-key") && !message.includes("example.invalid")))
})

test("cold empty discovery retries before registering a successful catalog", async (t) => {
  environment(t)
  const warnings = t.mock.method(console, "warn", () => {})
  let requests = 0
  t.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.redirect, "error")
    assert.ok(options.signal instanceof AbortSignal)
    return response(++requests === 1 ? [] : [{ id: "ready" }])
  })
  const state = context([nativeModel("removed")])

  await discovery.setup(state.ctx)

  assert.equal(requests, 2)
  assert.equal(state.registrations, 1)
  assert.deepEqual(state.models.map((model) => model.id), ["ready"])
  assert.equal(warnings.mock.callCount(), 0)
})

test("network retries stay bounded and never expose raw errors or credentials", async (t) => {
  environment(t)
  const warnings = []
  t.mock.method(console, "warn", (message) => warnings.push(message))
  const timeout = t.mock.method(AbortSignal, "timeout")
  const fetch = t.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.redirect, "error")
    throw new Error("fixture-key https://example.invalid/v1")
  })
  const initial = [nativeModel("existing")]
  const state = context(initial)

  assert.equal(await discovery.setup(state.ctx), undefined)

  assert.equal(fetch.mock.callCount(), 4)
  assert.deepEqual(timeout.mock.calls.map((call) => call.arguments[0]), [15000, 15000, 15000, 15000])
  assert.equal(state.registrations, 0)
  assert.deepEqual(state.models, initial)
  assert.deepEqual(warnings, ["[openproxy-models] model discovery failed; keeping native models."])
})
