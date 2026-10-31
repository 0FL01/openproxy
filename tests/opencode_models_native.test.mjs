import assert from "node:assert/strict"
import { once } from "node:events"
import { createServer } from "node:http"
import { registerHooks } from "node:module"
import { test } from "node:test"

const shim = registerHooks({ resolve(specifier, context, next) {
  if (specifier === "@opencode/plugin") return {
    url: `data:text/javascript,${encodeURIComponent("export const Plugin = { define: (plugin) => plugin }")}`, shortCircuit: true,
  }
  return next(specifier, context)
} })
const { default: plugin } = await import("../plugins/openproxy-models.js").finally(() => shim.deregister())

const primary = { providerID: "ludka2", id: "custom-route/opus" }
const other = { providerID: "ludka2", id: "claude-agent/not-native" }
const start = { type: "stream-start", warnings: [] }
const finish = { type: "finish", finishReason: { unified: "stop", raw: "end_turn" }, usage: { inputTokens: { total: 10 }, outputTokens: { total: 3 } } }
const catalog = [
  { id: primary.id, context_length: 1000000, opencode: { name: "Native Opus", source: "claude-agent", tool_call: true,
    package: "evil-sdk", settings: { baseURL: "https://evil.invalid", apiKey: "do-not-forward" }, headers: { evil: "yes" },
    variants: { max: { reasoningEffort: "max" } } } },
  { id: "custom-route/claude-haiku-4-5", opencode: { source: "claude-agent", limit: { context: 1000000 } } },
  { id: "haiku-route/opus", opencode: { source: "claude-agent" } },
  { id: other.id, opencode: { source: "anthropic", package: "@openproxy/claude-agent", limit: { context: 1000000, input: 900000, output: 64000 } } },
]

function environment(t, url = "https://proxy.invalid/prefix/v1") {
  for (const [name, value] of Object.entries({ LUDKA2_API_URL: url, LUDKA2_API_KEY: "fixture-key" })) {
    const before = process.env[name]; process.env[name] = value
    t.after(() => { if (before === undefined) delete process.env[name]; else process.env[name] = before })
  }
}

// Simulates only the documented Promise plugin context. This is not a real
// OpenCode service/integration test; loopback below exercises actual HTTP I/O.
function context() {
  const callbacks = new Map()
  const models = []
  const state = { disposals: 0, callbacks, models, agents: [], sessions: [], eventOptions: undefined }
  let deliver
  const queue = []
  let closed = false
  const hook = (domain) => async (name, callback, options) => {
    assert.deepEqual(options, { providerID: "ludka2" })
    const key = `${domain}:${name}`
    callbacks.set(key, [...(callbacks.get(key) ?? []), callback])
    return { dispose: async () => { state.disposals++ } }
  }
  state.ctx = {
    location: { directory: "/client/workspace", project: { directory: "/client/workspace" } },
    model: { async transform(edit) {
      edit({
        list: () => models,
        remove() {},
        update(providerID, id, update) {
          const model = { id, modelID: id, providerID, name: id, package: "@opencode/ai/openai-compatible",
            settings: {}, limit: { context: 200000, output: 32000 }, family: "inherited-family",
            cost: [{ input: 3, output: 15, cache: { read: 0.3, write: 3.75 } }],
            capabilities: { input: ["text"], output: ["text"], tools: false }, variants: [] }
          update(model); models.push(model)
        },
      })
      return { dispose: async () => { state.disposals++ } }
    } },
    aisdk: { hook: hook("aisdk") },
    session: { hook: hook("session"), async get({ sessionID }) {
      state.sessions.push(sessionID)
      return { fork: { sessionID: "fork-parent" } }
    } },
    agent: { async get({ agentID }) {
      state.agents.push(agentID)
      return { location: {}, data: { system: `Role resolved for ${agentID}` } }
    } },
    event: { subscribe(options) {
      state.eventOptions = options
      options.signal.addEventListener("abort", () => { closed = true; deliver?.({ done: true }) })
      return { [Symbol.asyncIterator]() { return { async next() {
        if (queue.length) return { done: false, value: queue.shift() }
        if (closed) return { done: true }
        return await new Promise(resolve => { deliver = resolve })
      } } } }
    } },
  }
  state.emit = (event) => {
    if (deliver) { const next = deliver; deliver = undefined; next({ done: false, value: event }) }
    else queue.push(event)
  }
  state.run = async (domain, name, event) => {
    for (const callback of callbacks.get(`${domain}:${name}`) ?? []) await callback(event)
    return event
  }
  state.language = async (model = primary) => {
    const event = await state.run("aisdk", "sdk", { model, package: "@openproxy/claude-agent", options: { baseURL: "https://ignored.invalid", headers: { evil: "yes" } } })
    return event.sdk.languageModel(primary.id)
  }
  return state
}

function ndjson(parts = [start, finish]) {
  return new Response(parts.map(part => JSON.stringify(part) + "\n").join(""), { headers: { "Content-Type": "application/x-ndjson" } })
}

function mockTransport(t, stream = () => ndjson()) {
  const calls = []
  t.mock.method(globalThis, "fetch", async (url, options) => {
    if (new URL(url).pathname.endsWith("/models")) {
      assert.equal(options.headers.get("X-OpenProxy-Claude-Mask"), null)
      return Response.json({ object: "list", data: catalog })
    }
    calls.push({ url: String(url), options, body: JSON.parse(options.body) })
    if (new URL(url).pathname.endsWith("/idle")) return new Response(null, { status: 204 })
    return stream(url, options)
  })
  return calls
}

const options = (kind = "primary") => ({
  headers: { "x-opencode-claude-agent-kind": kind, "x-opencode-session-id": "chat-1", "x-opencode-claude-agent-fork-source": "fork-parent" },
  prompt: [{ role: "user", content: [{ type: "text", text: "fixture" }] }],
  tools: [{ type: "function", name: "client_tool", inputSchema: { type: "object" } }, { type: "provider-defined", id: "must-not-offer" }],
  providerOptions: { ludka2: { claudeCodeAgent: "custom role", claudeCodeInstructions: "project instructions", reasoningEffort: "max", apiKey: "must-not-forward" } },
})
const collect = async (language, request = options()) => {
  const result = await language.doStream(request)
  return await Array.fromAsync(result.stream)
}

test("only canonical source selects the fixed local SDK; unrelated hooks and metadata stay untouched", async (t) => {
  environment(t); mockTransport(t)
  const state = context()
  const dispose = await plugin.setup(state.ctx)
  t.after(dispose)
  const native = state.models.find(model => model.id === primary.id)
  assert.equal(native.package, "aisdk:@openproxy/claude-agent")
  assert.equal(native.modelID, primary.id)
  assert.equal(native.name, "Native Opus · claude-agent")
  assert.deepEqual(native.limit, { context: 1000000, output: 0 }, "native 1M context does not require or fabricate an output budget")
  assert.deepEqual(native.cost, [], "subscription usage does not inherit API billing metadata")
  assert.equal(native.family, undefined)
  assert.deepEqual(native.capabilities, { tools: true, input: ["text", "image", "pdf"], output: ["text"] })
  assert.deepEqual(native.settings, {})
  assert.deepEqual(native.variants, [{ id: "max", settings: { reasoningEffort: "max" } }])
  const haiku = state.models.find(model => model.id === "custom-route/claude-haiku-4-5")
  assert.equal(haiku.family, "claude-haiku")
  assert.deepEqual(haiku.limit, { context: 1000000, output: 0 })
  const defaultContext = state.models.find(model => model.id === "haiku-route/opus")
  assert.deepEqual(defaultContext.limit, { context: 200000, output: 0 }, "missing native context retains Model.Info's default")
  assert.equal(defaultContext.family, undefined, "configurable route prefixes cannot select the Haiku family")
  const unrelated = state.models.find(model => model.id === other.id)
  assert.equal(unrelated.package, "@opencode/ai/openai-compatible")
  assert.deepEqual(unrelated.limit, { context: 500000, input: 500000, output: 64000 })
  assert.equal(unrelated.family, "inherited-family")
  assert.deepEqual(unrelated.cost, [{ input: 3, output: 15, cache: { read: 0.3, write: 3.75 } }])
  assert.deepEqual(unrelated.capabilities, { tools: false, input: ["text"], output: ["text"] })
  for (const model of [other, { ...primary, providerID: "other" }]) {
    const sdk = await state.run("aisdk", "sdk", { model, package: "@openproxy/claude-agent", options: {} })
    assert.equal(sdk.sdk, undefined)
    const retry = { model, decision: { retry: true, delay: 10 } }
    await state.run("session", "retry", retry)
    assert.deepEqual(retry.decision, { retry: true, delay: 10 })
    const request = { model, options: { maxTokens: 32 }, messages: [], system: [] }
    await state.run("session", "context", request)
    assert.deepEqual(request.options, { maxTokens: 32 })
  }
  assert.equal((await state.run("aisdk", "sdk", { model: primary, package: "evil", options: {} })).sdk, undefined)
  const language = await state.language()
  assert.equal(language.specificationVersion, "v3")
  assert.equal(language.provider, "claude-agent")
  await assert.rejects(language.doGenerate({}), /only support streaming/)
  await state.run("session", "retry", { model: primary, decision: { retry: true } }).then(event => assert.deepEqual(event.decision, { retry: false }))
})

test("public hooks preserve fork/kind, resolved agent role, keyed instructions and aborted tool errors", async (t) => {
  environment(t); mockTransport(t)
  const state = context(); t.after(await plugin.setup(state.ctx))
  const request = await state.run("session", "model.request", { model: primary, kind: "primary", sessionID: "chat-1", headers: {} })
  assert.deepEqual(request.headers, options().headers)
  assert.deepEqual(state.sessions, ["chat-1"])
  const title = await state.run("session", "model.request", { model: primary, kind: "title", sessionID: "chat-1", headers: { "x-opencode-claude-agent-fork-source": "stale" } })
  assert.equal(title.headers["x-opencode-claude-agent-kind"], "title")
  assert.equal(title.headers["x-opencode-claude-agent-fork-source"], undefined)
  const system = [
    { text: "native replaces this base prompt" },
    { text: "do not parse this rendered combined text", metadata: { experimental: { instructions: [
      { key: "core/date", text: "discard date" }, { key: "core/environment", text: "discard server-looking environment" },
      { key: "repo/AGENTS.md", text: "keep repository rules verbatim" },
    ] } } },
    { text: "unkeyed extension instructions" },
  ]
  const messages = [{ role: "tool", content: [{ type: "tool-result", result: { type: "error", value: { error: { type: "aborted" } } }, providerMetadata: { "claude-agent": { sessionID: "native-session", messageID: "checkpoint" } } }] }]
  const event = await state.run("session", "context", { model: primary, agent: "custom-planner", system, messages, options: { maxTokens: 128 } })
  assert.equal(event.options.maxTokens, undefined)
  assert.equal(event.options.claudeCodeAgent, "Role resolved for custom-planner")
  assert.match(event.options.claudeCodeInstructions, /keep repository rules verbatim/)
  assert.match(event.options.claudeCodeInstructions, /unkeyed extension instructions/)
  assert.doesNotMatch(event.options.claudeCodeInstructions, /discard|rendered combined|replaces this/)
  assert.deepEqual(event.messages[0].content[0].providerMetadata["claude-agent"], {
    sessionID: "native-session", messageID: "checkpoint", toolError: true, toolAborted: true,
  })
  assert.equal(messages[0].content[0].providerMetadata["claude-agent"].toolError, undefined)
  for (const kind of ["generate", "compaction"]) {
    const branch = await state.run("session", kind, { model: primary, agent: "custom-planner", system, options: { maxTokens: 64 } })
    assert.equal(branch.options.maxTokens, 64, "side calls retain explicitly chosen budgets")
    assert.equal(branch.options.claudeCodeAgent, event.options.claudeCodeAgent)
  }
})

test("remote transport preserves prefix, LMV3 history/media/tools/options and isolated request kinds", async (t) => {
  environment(t)
  const calls = mockTransport(t)
  const state = context(); t.after(await plugin.setup(state.ctx))
  const language = await state.language()
  const input = options()
  input.maxOutputTokens = 500
  input.prompt.push({ role: "user", content: [
    { type: "file", mediaType: "image/png", data: new Uint8Array([0, 1, 254, 255]) },
    { type: "file", mediaType: "application/pdf", data: new URL("https://files.invalid/a.pdf") },
  ] }, { role: "tool", content: [{ type: "tool-result", toolCallId: "call-1", toolName: "client_tool", output: { type: "text", value: "permission rejected" }, providerOptions: { "claude-agent": { toolError: true, toolAborted: true } } }] })
  assert.deepEqual(await collect(language, input), [start, finish])
  const call = calls[0]
  assert.equal(call.url, "https://proxy.invalid/prefix/v2/claude-agent/stream")
  assert.equal(call.options.headers.get("Authorization"), "Bearer fixture-key")
  assert.equal(call.options.headers.get("evil"), null)
  assert.equal(call.options.redirect, "error")
  assert.equal(call.body.model, primary.id)
  assert.equal(call.body.sessionID, "chat-1")
  assert.equal(call.body.sourceID, "fork-parent")
  assert.equal(call.body.system.agent, "custom role")
  assert.match(call.body.system.instructions, /^project instructions\n\n/)
  assert.match(call.body.system.instructions, /\/client\/workspace/)
  assert.match(call.body.system.instructions, /client host, not the OpenProxy\/Claude server/)
  assert.equal(call.body.prompt[1].content[0].data, "AAH+/w==")
  assert.equal(call.body.prompt[1].content[1].data, "https://files.invalid/a.pdf")
  assert.deepEqual(call.body.prompt[2], input.prompt[2])
  assert.deepEqual(call.body.tools, [input.tools[0]])
  assert.equal(call.body.effort, "max")
  assert.equal(call.body.maxOutputTokens, 500)
  assert.ok(!JSON.stringify(call.body).includes("must-not-forward"))
  assert.equal(call.options.signal.aborted, false, "normal EOF must not cancel parked client tools")
  for (const kind of ["title", "generate", "compaction"]) {
    await collect(language, options(kind))
    const body = calls.at(-1).body
    assert.equal(body.kind, kind)
    assert.equal(body.sessionID, undefined)
    assert.equal(body.sourceID, kind === "title" ? undefined : "chat-1")
    assert.equal(body.tools.length, kind === "title" ? 0 : 1)
  }
  await collect(language, { ...options(), toolChoice: { type: "none" } })
  assert.deepEqual(calls.at(-1).body.tools, [])
  const canonicalLanguage = await state.language({ ...primary, canonical: "openai" })
  await collect(canonicalLanguage, { ...options(), providerOptions: { openai: options().providerOptions.ludka2 } })
  assert.equal(calls.at(-1).body.system.agent, "custom role", "respect the public bridge's canonical option namespace")
  assert.equal(calls.at(-1).body.effort, "max")
})

test("tool-call completion never sends idle; only matching terminal execution events do", async (t) => {
  environment(t)
  const calls = mockTransport(t, () => ndjson([start,
    { type: "tool-call", toolCallId: "call-1", toolName: "client_tool", input: "{}", providerMetadata: { "claude-agent": { sessionID: "native", messageID: "uuid" } } },
    { ...finish, finishReason: { unified: "tool-calls", raw: "tool_use" } },
  ]))
  const state = context(); const dispose = await plugin.setup(state.ctx)
  await collect(await state.language())
  assert.deepEqual(state.eventOptions.types, ["session.execution.succeeded", "session.execution.failed", "session.execution.interrupted"])
  assert.equal(calls.length, 1)
  state.emit({ type: "session.execution.succeeded", data: { sessionID: "not-a-native-session" } })
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(calls.length, 1)
  for (const type of state.eventOptions.types) {
    await state.run("session", "model.request", { model: primary, kind: "primary", sessionID: "chat-1", headers: {} })
    state.emit({ type, data: { sessionID: "chat-1" } })
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(calls.at(-1).url, "https://proxy.invalid/prefix/v2/claude-agent/idle")
    assert.deepEqual(calls.at(-1).body, { sessionID: "chat-1" })
  }
  await dispose()
  assert.equal(state.disposals, 8)
  assert.equal(state.eventOptions.signal.aborted, true)
})

test("framing, terminal finish and structured errors fail closed without retrying generation", async (t) => {
  environment(t)
  let reply
  const calls = mockTransport(t, () => reply())
  const state = context(); t.after(await plugin.setup(state.ctx))
  const language = await state.language()
  for (const parts of [[], [start], [finish], [start, finish, finish], [start, { type: "text-delta", id: "a", delta: 3 }, finish], [start, { ...finish, usage: {} }]]) {
    reply = () => ndjson(parts)
    await assert.rejects(collect(language), error => error.code === "invalid_stream")
  }
  for (const text of [JSON.stringify(start) + "\n" + JSON.stringify(finish), "not-json\n", JSON.stringify(start) + "\n" + "x".repeat(8 * 1024 * 1024 + 1)]) {
    reply = () => new Response(text, { headers: { "Content-Type": "application/x-ndjson" } })
    await assert.rejects(collect(language), error => error.code === "invalid_stream")
  }
  reply = () => ndjson([start, { type: "error", error: { message: "Native account unavailable", code: "not_logged_in" } }])
  await assert.rejects(collect(language), error => error.message === "Native account unavailable" && error.code === "not_logged_in")
  reply = () => Response.json({ error: { message: "Native capacity reached", code: "capacity" } }, { status: 429 })
  await assert.rejects(collect(language), error => error.code === "capacity")
  reply = () => new Response("fixture-key raw failure", { status: 500 })
  await assert.rejects(collect(language), error => error.message === "Claude Agent request failed (HTTP 500)")
  reply = () => new Response("wrong protocol")
  await assert.rejects(collect(language), error => error.code === "invalid_stream")
  assert.equal(calls.length, 13)
})

test("abort propagates to pending fetch/body and cancelling the consumer cancels native transport", async (t) => {
  environment(t)
  let cancelCount = 0
  const calls = mockTransport(t, () => new Response(new ReadableStream({
    start(controller) { controller.enqueue(new TextEncoder().encode(JSON.stringify(start) + "\n")) },
    cancel() { cancelCount++ },
  }), { headers: { "Content-Type": "application/x-ndjson" } }))
  const state = context(); t.after(await plugin.setup(state.ctx))
  const language = await state.language()
  const abort = new AbortController()
  const reader = (await language.doStream({ ...options(), abortSignal: abort.signal })).stream.getReader()
  assert.deepEqual((await reader.read()).value, start)
  const waiting = reader.read()
  abort.abort(new DOMException("Stopped", "AbortError"))
  await assert.rejects(waiting, error => error.name === "AbortError")
  assert.equal(calls[0].options.signal.aborted, true)
  assert.equal(cancelCount, 1)
  const next = (await language.doStream(options())).stream.getReader()
  await next.read()
  await next.cancel()
  assert.equal(calls[1].options.signal.aborted, true)
  assert.equal(cancelCount, 2)
  const unread = (await language.doStream(options())).stream.getReader()
  await unread.cancel()
  assert.equal(calls[2].options.signal.aborted, true)
  assert.equal(cancelCount, 3)
  const alreadyStopped = new AbortController()
  alreadyStopped.abort()
  await assert.rejects(language.doStream({ ...options(), abortSignal: alreadyStopped.signal }), error => error.name === "AbortError")
  assert.equal(calls.length, 3, "an already-aborted request must not start generation")
})

test("loopback HTTP stream decodes split UTF-8/frames with unchanged routed model ID", async (t) => {
  const requests = []
  const server = createServer(async (req, res) => {
    requests.push(req.url)
    assert.equal(req.headers.authorization, "Bearer fixture-key")
    if (req.url === "/reverse/proxy/v1/models") { res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify({ object: "list", data: catalog })); return }
    const chunks = []; for await (const chunk of req) chunks.push(chunk)
    assert.equal(JSON.parse(Buffer.concat(chunks)).model, primary.id)
    res.writeHead(200, { "Content-Type": "application/x-ndjson; charset=utf-8" })
    const body = Buffer.from([start, { type: "text-start", id: "a" }, { type: "text-delta", id: "a", delta: "Привет 🌍" }, { type: "text-end", id: "a" }, finish].map(part => JSON.stringify(part) + "\n").join(""))
    for (let i = 0; i < body.length; i += 7) res.write(body.subarray(i, i + 7))
    res.end()
  }).listen(0, "127.0.0.1")
  await once(server, "listening")
  t.after(() => { server.closeAllConnections(); server.close() })
  environment(t, `http://127.0.0.1:${server.address().port}/reverse/proxy/v1/`)
  const state = context(); t.after(await plugin.setup(state.ctx))
  const parts = await collect(await state.language())
  assert.equal(parts[2].delta, "Привет 🌍")
  assert.deepEqual(parts.at(-1), finish)
  assert.deepEqual(requests, ["/reverse/proxy/v1/models", "/reverse/proxy/v2/claude-agent/stream"])
})
