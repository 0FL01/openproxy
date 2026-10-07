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
  if (["text", "span", "b"].includes(node.type)) return [content(node.props.children).join("")]
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
  api.tree = () => api.slot.slots.sidebar_content({ theme: api.theme })
  api.render = () => content(api.tree()).join("\n")
  return api
}

function pollingClock(t) {
  let now = Date.now(), handle = 0, tick
  const scheduled = new Map()
  t.mock.method(Date, "now", () => now)
  t.mock.method(globalThis, "setTimeout", (fn, ms) => { const id = ++handle; scheduled.set(id, { fn, ms }); return id })
  t.mock.method(globalThis, "clearTimeout", (id) => scheduled.delete(id))
  t.mock.method(globalThis, "setInterval", (fn) => { tick = fn; return ++handle })
  t.mock.method(globalThis, "clearInterval", () => {})
  return {
    get now() { return now },
    scheduled,
    advance(ms) { now += ms; tick() },
    async poll(ms = 60000) {
      const entry = [...scheduled.entries()].find(([, timer]) => timer.ms === 60000)
      assert.ok(entry, "one regular poll scheduled")
      scheduled.delete(entry[0])
      now += ms; tick(); entry[1].fn()
      await flush()
    },
  }
}

test("sidebar shares provider quotas, uses theme colors, retries loading once and retains stale data", async (t) => {
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
  assert.match(api.render(), /Loading/)
  const retry = [...scheduled.entries()].find(([, value]) => value.ms === 2500)
  assert.ok(retry)
  body = response([
    { ...account("Primary", "fresh", {
      Session: quota({ remainingPercentage: 20, resetAt: new Date(now + 4200000).toISOString() }),
      Balance: quota({ remaining: 2.5, unit: "USD" }),
      Requests: quota({ used: 25, total: 100, remaining: 75, unit: "requests" }),
    }),
      observedAt: new Date(now).toISOString() },
    account("Secondary", "fresh", {
      Session: quota({ remainingPercentage: 80, resetAt: new Date(now + 7200000).toISOString() }),
      Weekly: quota(), Credits: quota({ used: 0, total: 100 }), Special: quota({ unlimited: true }),
      Balance: quota({ remaining: 12.5, unit: "USD" }),
      Requests: quota({ used: 185, total: 200, remaining: 15, unit: "requests" }),
      Empty: quota({ remaining: 0, unit: "credits" }),
      Critical: quota({ remainingPercentage: 10 }),
      Monthly: quota({ remainingPercentage: 96, resetAt: new Date(now + (26 * 24 + 4) * 3600000).toISOString() }),
    }),
    { ...account("Older", "fresh", { Session: quota({ remainingPercentage: 99 }) }),
      provider: "glm", plan: "Lite", observedAt: new Date(now - 180000).toISOString() },
    { ...account("Unsupported", "unsupported"), provider: "unsupported-fixture" },
    { ...account("Failed", "unavailable"), provider: "failed-fixture", error: "Provider temporarily unavailable" },
  ])
  scheduled.delete(retry[0]); retry[1].fn()
  await flush()
  assert.equal(api.render().match(/Codex · Pro/g).length, 1)
  assert.match(api.render(), /Session\s+━{4}─{4} 50% ↻1h10m/)
  assert.match(api.render(), /Monthly\s+─{8} 4% ↻26d4h/)
  assert.match(api.render(), /Weekly\s+unknown[\s\S]*Credits\s+─{8} 0%[\s\S]*Unlimited/)
  assert.match(api.render(), /GLM · Lite\nSession/)
  assert.doesNotMatch(api.render(), /Data stale/)
  assert.match(api.render(), /15 USD left/)
  assert.match(api.render(), /Requests\s+━{6}─{2} 70% · 90 requests left/)
  assert.match(api.render(), /0 credits left/)
  assert.doesNotMatch(api.render(), /Primary|Secondary|Older|Account|fresh|updated/)
  const codex = api.tree().props.children[1]
  assert.equal(codex.props.children[0].props.children[0].type, "b")
  const lines = codex.props.children.slice(1)
  assert.ok(lines.every((node) => node.type === "text"))
  const session = lines.find((line) => content(line).join("").startsWith("Session")).props.children
  assert.equal(session[1].props.fg, "green")
  assert.equal(session[2].props.fg, "green")
  assert.equal(session[3].type, "b")
  for (const [label, color] of [["Requests", "yellow"], ["Critical", "red"]]) {
    const row = lines.find((line) => content(line).join("").startsWith(label)).props.children
    assert.equal(row[1].props.fg, "green")
    assert.equal(row[2].props.fg, "green")
    assert.equal(row[3].props.fg, color)
  }
  Date.now.mock.mockImplementation(() => now + 120000)
  tick()
  assert.match(api.render(), /Codex · Pro\nSession/)
  assert.match(api.render(), /Limits unsupported[\s\S]*Limits unavailable/)
  assert.match(api.render(), /Provider temporarily unavailable/)
  assert.ok([...scheduled.values()].some((value) => value.ms === 60000))
  // Invalid data validates atomically and never overwrites the last good rows.
  body = response([{ ...account("Bad", "fresh"), quotas: { Bad: quota({ used: "fixture-key" }) } }])
  const poll = [...scheduled.entries()].find(([, value]) => value.ms === 60000)
  scheduled.delete(poll[0]); poll[1].fn()
  await flush()
  assert.match(api.render(), /Invalid usage response[\s\S]*Data stale[\s\S]*50%/)
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
  assert.equal(api.render().match(/Codex · Pro/g).length, 1)
  assert.match(api.render(), /Some limits omitted/)
  assert.match(api.render(), /25%[\s\S]*9600 requests left/)
  assert.doesNotMatch(api.render(), /Account|fresh|updated/)
  assert.ok(!api.render().includes("Invalid usage response"))
})

test("shared quotas keep different units separate and mark missing values without zero-filling", async (t) => {
  t.mock.method(globalThis, "fetch", async () => response([
    account("One", "fresh", {
      Weekly: quota({ remainingPercentage: 20 }), Balance: quota({ remaining: 10, unit: "USD" }),
      Special: quota({ unlimited: true }),
    }),
    { ...account("Two", "fresh", {
      Weekly: quota(), Balance: quota({ remaining: 5, unit: "EUR" }),
      Special: quota({ remainingPercentage: 10 }),
    }), plan: "Lite" },
  ]))
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  assert.match(api.render(), /Codex\nPartial data\nWeekly\s+━{6}─{2} 80%/)
  assert.match(api.render(), /10 USD left/)
  assert.match(api.render(), /5 EUR left/)
  assert.match(api.render(), /Special\s+Unlimited/)
  assert.doesNotMatch(api.render(), /Pro|Lite|One|Two|fresh|updated/)
})

test("successful minute polls keep aged stale refresh-pending quotas quiet for a full hour", async (t) => {
  const clock = pollingClock(t)
  const observedAt = new Date(clock.now - 86400000).toISOString()
  let polls = 0
  const fetch = t.mock.method(globalThis, "fetch", async () => response([
    { ...account("Pending", "stale", { Session: quota({ remainingPercentage: 75 }) }),
      observedAt, refreshing: polls++ % 2 === 0, nextRefreshAt: null, errorStatus: null },
    // Old backends omit all the new fields, and may have no observation time.
    { ...account("Old backend", "stale", { Session: quota({ remainingPercentage: 75 }) }), observedAt: null },
  ]))
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  for (let minute = 0; minute <= 60; minute++) {
    assert.match(api.render(), /Codex · Pro\nSession\s+━{2}─{6} 25%/)
    assert.doesNotMatch(api.render(), /stale|retry|refresh|updated|Pending|Old backend/)
    assert.equal(clock.scheduled.size, 1)
    if (minute < 60) await clock.poll()
  }
  assert.equal(fetch.mock.callCount(), 61)
})

test("every upstream error is visible beside healthy quotas with retry diagnostics and clears on recovery", async (t) => {
  const clock = pollingClock(t)
  let rows = [
    account("Healthy", "fresh", { Session: quota({ remainingPercentage: 75 }) }),
    { ...account("Failed", "stale", { Session: quota({ remainingPercentage: 75 }) }),
      error: "Quota rate limited", errorStatus: 429, refreshing: false,
      nextRefreshAt: new Date(clock.now + 120000).toISOString() },
    { ...account("Missing", "unavailable"), error: "Quota authentication failed", errorStatus: 401,
      refreshing: false, nextRefreshAt: null },
  ]
  const fetch = t.mock.method(globalThis, "fetch", async () => response(rows))
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  assert.match(api.render(), /Partial data/)
  assert.match(api.render(), /Quota rate limited · HTTP 429 · retry 2m/)
  assert.match(api.render(), /Quota authentication failed · HTTP 401/)
  assert.match(api.render(), /25%/)
  assert.doesNotMatch(api.render(), /Healthy|Failed|Missing|Data stale/)
  clock.advance(60000)
  assert.match(api.render(), /retry 1m/)
  assert.equal(fetch.mock.callCount(), 1, "countdown does not trigger requests")
  rows[1].refreshing = true
  await clock.poll(0)
  assert.match(api.render(), /Quota rate limited · HTTP 429 · retrying/)
  assert.doesNotMatch(api.render(), /retry 1m/)
  rows = rows.map((row) => ({ ...row, status: "fresh", error: null, errorStatus: null, refreshing: false,
    nextRefreshAt: null, quotas: { Session: quota({ remainingPercentage: 60 }) } }))
  await clock.poll()
  assert.match(api.render(), /Codex · Pro\nSession\s+━{3}─{5} 40%/)
  assert.doesNotMatch(api.render(), /Quota|HTTP|retry|Partial|stale/)
  // Even a non-stale status and an empty error string cannot hide an error.
  rows[0].error = ""
  rows[1].error = "Invalid quota response\n" + "x".repeat(200)
  await clock.poll()
  assert.match(api.render(), /Quota request failed/)
  assert.match(api.render(), /Invalid quota response/)
  assert.ok(!api.render().includes("x".repeat(80)))
})

test("proxy errors retain last quotas and clear only on a validated successful read", async (t) => {
  const clock = pollingClock(t)
  let mode = "ok"
  t.mock.method(globalThis, "fetch", async () => {
    if (mode === "network") throw new Error("fixture-key")
    if (mode === "http") return new Response("fixture-key", { status: 503 })
    return response([account("Hidden", "stale", { Session: quota({ remainingPercentage: 75 }) })])
  })
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  for (const failure of ["network", "http"]) {
    mode = failure
    await clock.poll()
    assert.match(api.render(), /Proxy unavailable[\s\S]*Data stale[\s\S]*25%/)
    if (failure === "http") assert.match(api.render(), /HTTP 503/)
    assert.doesNotMatch(api.render(), /fixture-key|Hidden/)
    mode = "ok"
    await clock.poll()
    assert.match(api.render(), /25%/)
    assert.doesNotMatch(api.render(), /unavailable|stale|HTTP/)
  }
})

test("proxy silence and server clock skew never add a stale warning to retained quotas", async (t) => {
  const clock = pollingClock(t)
  let reads = 0, finish
  const fetch = t.mock.method(globalThis, "fetch", () => {
    const body = response([{
      ...account("Skewed", "fresh", { Session: quota({ remainingPercentage: 75 }) }),
      observedAt: new Date(clock.now + (reads++ % 2 ? 1 : -1) * 86400000).toISOString(),
    }])
    if (reads === 3) return new Promise((resolve) => { finish = () => resolve(body) })
    return Promise.resolve(body)
  })
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  assert.doesNotMatch(api.render(), /stale/)
  await clock.poll()
  assert.doesNotMatch(api.render(), /stale/)
  await clock.poll()
  // The third read is still in flight; however long it takes, retained rows
  // stay quiet — aged values refresh silently in the background.
  clock.advance(130000)
  assert.match(api.render(), /25%/)
  assert.doesNotMatch(api.render(), /stale|unavailable|retry/)
  assert.equal(fetch.mock.callCount(), 3)
  finish()
  await flush()
  assert.match(api.render(), /25%/)
  assert.doesNotMatch(api.render(), /stale/)
})

test("optional quota diagnostics validate atomically with the complete response", async (t) => {
  const clock = pollingClock(t)
  const good = { ...account("Good", "fresh", { Session: quota({ remainingPercentage: 75 }) }),
    refreshing: true, nextRefreshAt: new Date(clock.now + 60000).toISOString(), errorStatus: 100 }
  let rows = [good, { ...good, id: "Other", refreshing: false, nextRefreshAt: null, errorStatus: 599 }]
  t.mock.method(globalThis, "fetch", async () => response(rows))
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  assert.match(api.render(), /25%/)
  for (const invalid of [
    { refreshing: null }, { refreshing: "true" }, { refreshing: 1 },
    { nextRefreshAt: false }, { nextRefreshAt: "tomorrow" }, { nextRefreshAt: 0 },
    { errorStatus: "429" }, { errorStatus: 99 }, { errorStatus: 600 }, { errorStatus: 429.5 },
  ]) {
    rows = [{ ...good, quotas: { Session: quota({ remainingPercentage: 5 }) } }, { ...good, id: "Bad", ...invalid }]
    await clock.poll()
    assert.match(api.render(), /Invalid usage response[\s\S]*Data stale[\s\S]*25%/)
    assert.doesNotMatch(api.render(), /95%|Good|Bad/)
  }
  rows = [{ ...good, errorStatus: null }]
  await clock.poll()
  assert.match(api.render(), /25%/)
  assert.doesNotMatch(api.render(), /Invalid|stale/)
})

test("claude passive quota windows render under the Claude name", async (t) => {
  const now = Date.now()
  t.mock.method(Date, "now", () => now)
  const scheduled = new Map()
  let handle = 0
  t.mock.method(globalThis, "setTimeout", (fn, ms) => { const id = ++handle; scheduled.set(id, { fn, ms }); return id })
  t.mock.method(globalThis, "clearTimeout", (id) => scheduled.delete(id))
  t.mock.method(globalThis, "setInterval", () => ++handle)
  t.mock.method(globalThis, "clearInterval", () => {})
  let observed = true
  t.mock.method(globalThis, "fetch", async () => new Response(JSON.stringify({
    refreshIntervalSeconds: 180,
    accounts: [observed ? {
      id: "claude-1", provider: "claude", label: "Account 1", status: "fresh",
      observedAt: new Date(now).toISOString(), plan: "Max x5",
      quotas: {
        "session (5h)": { used: 0, total: 100, remaining: 100, remainingPercentage: 100, resetAt: new Date(now + 170 * 60000).toISOString(), unlimited: false },
        "weekly (7d)": { used: 22, total: 100, remaining: 78, remainingPercentage: 78, resetAt: new Date(now + 73 * 3600000).toISOString(), unlimited: false },
      }, error: null,
    } : {
      id: "claude-1", provider: "claude", label: "Account 1", status: "loading",
      observedAt: null, plan: null, quotas: {}, error: null,
    }],
  })))
  const api = host(t)
  await OpenProxySidebar(api)
  api.ready()
  await flush()
  let render = api.render()
  assert.match(render, /Claude · Max x5\n5h/)
  assert.match(render, /5h\s+─{8} 0%/)
  assert.match(render, /Weekly\s+━{2}─{6} 22% ↻3d1h/)
  assert.ok(!render.includes("claude\n"), "raw provider id must not render")
  assert.doesNotMatch(render, /Limits unsupported|Loading|unavailable/)
  // Before the first observed snapshot the group says it is waiting.
  observed = false
  const poll = [...scheduled.entries()].find(([, value]) => value.ms === 60000)
  scheduled.delete(poll[0]); poll[1].fn()
  await flush()
  render = api.render()
  assert.match(render, /Claude\nLoading…/)
  api.dispose()
})
