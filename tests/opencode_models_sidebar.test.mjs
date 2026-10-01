import assert from "node:assert/strict"
import { registerHooks } from "node:module"
import { test } from "node:test"
import { OpenProxySidebar } from "../plugins/openproxy-models.js"

// Test-only module hooks exercise the real lazy import boundary without adding
// renderer dependencies or dependency injection to the production plugin.
const hooks = registerHooks({
  resolve(specifier, context, next) {
    const sources = {
      "solid-js": `export const createSignal = (initial) => { let value = initial; return [() => value, (next) => value = next] }
        export const createRoot = (fn) => fn(() => {})
        export const createEffect = (fn) => { globalThis.__sidebarEffect = fn; fn() }`,
      "@opentui/solid/jsx-runtime": `export const jsx = (type, props) => typeof type === "function" ? type(props) : ({ type, props })
        export const jsxs = jsx`,
    }
    if (sources[specifier]) return { url: `data:text/javascript,${encodeURIComponent(sources[specifier])}`, shortCircuit: true }
    return next(specifier, context)
  },
})
process.on("exit", () => hooks.deregister())

const quota = (fields = {}) => ({ used: null, total: null, remaining: null, remainingPercentage: null, resetAt: null, unlimited: false, ...fields })
const account = (id, status, quotas = {}) => ({ id, provider: "codex", label: id, status, observedAt: new Date().toISOString(), plan: "Pro", quotas, error: null })
const response = (accounts) => new Response(JSON.stringify({ refreshIntervalSeconds: 180, accounts }))
const flush = async () => { for (let i = 0; i < 12; i++) await new Promise((resolve) => setImmediate(resolve)) }
function content(node) {
  if (Array.isArray(node)) return node.flatMap(content)
  if (!node || node.props?.visible === false) return []
  if (typeof node === "string") return [node]
  return content(node.props.children)
}
function host(t, config = {}) {
  const lifecycle = new AbortController()
  const disposers = []
  const api = {
    state: { ready: false, config: { provider: { ludka2: { options: {
      baseURL: "https://example.invalid/v1/", apiKey: "fixture-key", headers: { "X-Fixture": "keep" },
    } } }, ...config } },
    lifecycle: { signal: lifecycle.signal, onDispose: (fn) => disposers.push(fn) },
    theme: { current: { text: "white", textMuted: "gray", warning: "yellow", error: "red", success: "green" } },
    slots: { register: (slot) => { api.slot = slot } },
  }
  api.dispose = () => { lifecycle.abort(); disposers.forEach((fn) => fn()) }
  t.after(api.dispose)
  api.ready = () => { api.state.ready = true; globalThis.__sidebarEffect() }
  api.render = () => content(api.slot.slots.sidebar_content({ theme: api.theme })).join("\n")
  return api
}

test("sidebar waits for resolved config, renders distinct accounts, retries loading once and retains stale data", async (t) => {
  const now = Date.now()
  t.mock.method(Date, "now", () => now)
  const scheduled = new Map()
  let handle = 0
  t.mock.method(globalThis, "setTimeout", (fn, ms) => { const id = ++handle; scheduled.set(id, { fn, ms }); return id })
  t.mock.method(globalThis, "clearTimeout", (id) => scheduled.delete(id))
  let tick
  t.mock.method(globalThis, "setInterval", (fn) => { tick = fn; return ++handle })
  const intervals = t.mock.method(globalThis, "clearInterval", () => {})
  let body = response([account("Primary", "loading")])
  const fetch = t.mock.method(globalThis, "fetch", async (url, options) => {
    assert.equal(url.href, "https://example.invalid/v1/usage/limits")
    assert.equal(options.headers.get("Authorization"), "Bearer fixture-key")
    assert.equal(options.headers.get("X-Fixture"), "keep")
    assert.equal(options.redirect, "error")
    return body
  })
  const api = host(t)
  await OpenProxySidebar(api)
  assert.equal(api.slot.order, 101)
  assert.equal(fetch.mock.callCount(), 0)
  assert.equal(api.render(), "")
  api.ready()
  await flush()
  assert.match(api.render(), /loading/)
  const retry = [...scheduled.entries()].find(([, value]) => value.ms === 2500)
  assert.ok(retry)
  body = response([
    { ...account("Primary", "fresh", { Session: quota({ remainingPercentage: 20, resetAt: new Date(Date.now() + 3600000).toISOString() }) }),
      observedAt: new Date(now).toISOString() },
    account("Secondary", "fresh", {
      Weekly: quota(), Credits: quota({ used: 0, total: 100 }), Special: quota({ unlimited: true }),
      Balance: quota({ remaining: 12.5, unit: "USD" }),
      Requests: quota({ used: 25, total: 100, remaining: 75, unit: "requests" }),
      Empty: quota({ remaining: 0, unit: "credits" }),
    }),
    { ...account("Older", "fresh"), observedAt: new Date(now - 180000).toISOString() },
    account("Unsupported", "unsupported"),
    { ...account("Failed", "unavailable"), error: "Provider temporarily unavailable" },
  ])
  scheduled.delete(retry[0]); retry[1].fn()
  await flush()
  assert.match(api.render(), /Primary · codex[\s\S]*80% used/)
  assert.match(api.render(), /reset 1h 0m/)
  assert.match(api.render(), /Secondary · codex[\s\S]*unknown[\s\S]*0% used[\s\S]*Unlimited/)
  assert.match(api.render(), /Older · codex\nPro · stale · updated 3m ago\nQuota unknown/)
  assert.match(api.render(), /12\.5 USD left/)
  assert.match(api.render(), /25% used · 75 requests left/)
  assert.match(api.render(), /0 credits left/)
  assert.match(api.render(), /updated just now/)
  Date.now.mock.mockImplementation(() => now + 120000)
  tick()
  assert.match(api.render(), /Primary · codex\nPro · fresh · updated 2m ago/)
  assert.match(api.render(), /unsupported[\s\S]*unavailable[\s\S]*Provider temporarily unavailable/)
  assert.ok([...scheduled.values()].some((value) => value.ms === 60000))
  // Invalid data validates atomically and never overwrites the last good rows.
  body = response([{ ...account("Bad", "fresh"), quotas: { Bad: quota({ used: "fixture-key" }) } }])
  const poll = [...scheduled.entries()].find(([, value]) => value.ms === 60000)
  scheduled.delete(poll[0]); poll[1].fn()
  await flush()
  assert.match(api.render(), /Invalid usage response[\s\S]*stale \(cached\)[\s\S]*80% used/)
  assert.ok(!api.render().includes("fixture-key"))
  assert.ok(!api.render().includes("Bad"))
  api.dispose()
  assert.equal(scheduled.size, 0)
  assert.ok(intervals.mock.callCount() > 0)
})

test("sidebar disposal aborts its only in-flight request and prevents late updates or polling", async (t) => {
  const api = host(t)
  let signal, finish
  const fetch = t.mock.method(globalThis, "fetch", (_url, options) => {
    signal = options.signal
    return new Promise((resolve) => { finish = resolve })
  })
  await OpenProxySidebar(api)
  api.ready()
  assert.equal(fetch.mock.callCount(), 1)
  const before = api.render()
  api.dispose()
  assert.equal(signal.aborted, true)
  finish(response([account("Late", "fresh")]))
  await flush()
  assert.equal(api.render(), before)
})

test("disabled providers and invalid credentials never send a usage request", async (t) => {
  const fetch = t.mock.method(globalThis, "fetch", () => { throw new Error("unexpected request") })
  for (const config of [
    { disabled_providers: ["ludka2"] },
    { enabled_providers: ["other"] },
    { provider: {} },
    { provider: { ludka2: { options: { baseURL: "https://user:secret@example.invalid/v1", apiKey: "fixture-key" } } } },
  ]) {
    const api = host(t, config)
    await OpenProxySidebar(api)
    api.ready()
    await flush()
    assert.equal(fetch.mock.callCount(), 0)
    assert.ok(!api.render().includes("secret"))
    api.dispose()
  }
})

test("request timeout, HTTP errors and oversized bodies stay bounded and sanitized", async (t) => {
  const scheduled = new Map()
  let handle = 0
  t.mock.method(globalThis, "setTimeout", (fn, ms) => { const id = ++handle; scheduled.set(id, { fn, ms }); return id })
  t.mock.method(globalThis, "clearTimeout", (id) => scheduled.delete(id))
  t.mock.method(globalThis, "setInterval", () => ++handle)
  t.mock.method(globalThis, "clearInterval", () => {})
  const api = host(t)
  let mode = "timeout", signal
  const fetch = t.mock.method(globalThis, "fetch", (_url, options) => {
    signal = options.signal
    if (mode === "http") return Promise.resolve(new Response("fixture-key", { status: 401 }))
    if (mode === "large") return Promise.resolve(new Response("fixture-key".repeat(220000)))
    return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("fixture-key")), { once: true }))
  })
  await OpenProxySidebar(api)
  api.ready()
  assert.equal(fetch.mock.callCount(), 1)
  const timeout = [...scheduled.entries()].find(([, value]) => value.ms === 10000)
  assert.ok(timeout)
  timeout[1].fn()
  await flush()
  assert.equal(signal.aborted, true)
  assert.match(api.render(), /Proxy unavailable/)
  for (const [nextMode, expected] of [["http", /HTTP 401/], ["large", /Invalid usage response/]]) {
    mode = nextMode
    const poll = [...scheduled.entries()].find(([, value]) => value.ms === 60000)
    scheduled.delete(poll[0]); poll[1].fn()
    await flush()
    assert.match(api.render(), expected)
    assert.ok(!api.render().includes("fixture-key"))
  }
  api.dispose()
  assert.equal(scheduled.size, 0)
})

test("legitimate bounded aggregates larger than 256KiB are accepted", async (t) => {
  const rows = Array.from({ length: 128 }, (_, i) => account(`Account ${i}`, "fresh", Object.fromEntries(
    Array.from({ length: 32 }, (_, j) => [`Window ${j} ${"q".repeat(48)}`, quota({
      used: 25, total: 100, remaining: 75, unit: "requests", resetAt: new Date().toISOString(),
    })]),
  )))
  const serialized = JSON.stringify({ refreshIntervalSeconds: 180, accounts: rows, truncated: true })
  assert.ok(Buffer.byteLength(serialized) > 262144)
  assert.ok(Buffer.byteLength(serialized) < 2 * 1024 * 1024)
  t.mock.method(globalThis, "fetch", async () => new Response(serialized))
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  assert.match(api.render(), /Account 127 · codex/)
  assert.match(api.render(), /Showing first 128 accounts/)
  assert.match(api.render(), /25% used · 75 requests left/)
  assert.ok(!api.render().includes("Invalid usage response"))
})
