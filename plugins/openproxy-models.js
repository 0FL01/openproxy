// Auto-loaded from ~/.config/opencode/plugins/. The key stays in provider.options.
const PROVIDER_ID = "ludka2"
// Stay above OpenProxy's ~10-second upstream discovery timeout on cold starts.
const DISCOVERY_ATTEMPT_TIMEOUT_MS = 15000
const DISCOVERY_TOTAL_TIMEOUT_MS = 30000
const DISCOVERY_RETRY_DELAYS_MS = [250, 750, 1500]
const MAX_CONTEXT_TOKENS = 500000
const STANDARD_REASONING_VARIANTS = ["none", "minimal", "low", "medium", "high", "xhigh", "max"]

function record(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

function positiveInteger(value) {
  return Number.isSafeInteger(value) && value > 0
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function prettyModelName(id) {
  const modelId = id.includes("/") ? id.slice(id.indexOf("/") + 1) : id
  const words = modelId.split(/[-_]+/).filter(Boolean).map((word) => {
    const lower = word.toLowerCase()
    if (lower === "gpt") return "GPT"
    if (lower === "glm") return "GLM"
    return lower[0].toUpperCase() + lower.slice(1)
  })
  if (words[0] === "GPT" && /^\d/.test(words[1] ?? "")) {
    words.splice(0, 2, `${words[0]}-${words[1]}`)
  }
  return words.join(" ") || id
}

function normalizeModelName(name) {
  return name.replace(/\bglm\b/gi, "GLM")
}

function explicitModelName(name, id) {
  if (typeof name !== "string" || !name.trim()) return undefined
  const withoutPrefix = id.includes("/") ? id.slice(id.indexOf("/") + 1) : id
  return [id, withoutPrefix].includes(name.trim()) ? undefined : name.trim()
}

function sourceModelName(name, source) {
  if (!source) return normalizeModelName(name)
  const suffix = ` · ${source}`
  // Repeated config hooks can receive previously decorated names. Strip only
  // this source's trailing suffixes before normalizing the model name itself.
  while (name.slice(-suffix.length).toLowerCase() === suffix.toLowerCase()) {
    name = name.slice(0, -suffix.length)
  }
  return `${normalizeModelName(name)}${suffix}`
}

// Only accept model metadata, never remote SDK/URL/header/options overrides.
function modelConfig(row) {
  if (!record(row) || typeof row.id !== "string" || !row.id.trim()) throw new Error()
  const result = { name: prettyModelName(row.id) }
  const limit = {}
  for (const [source, target] of [["context_length", "context"], ["max_completion_tokens", "output"]]) {
    if (row[source] !== undefined) {
      if (!positiveInteger(row[source])) throw new Error()
      limit[target] = row[source]
    }
  }
  const metadata = row.opencode ?? {}
  if (!record(metadata)) throw new Error()
  let source
  if (metadata.source !== undefined) {
    if (typeof metadata.source !== "string" || !metadata.source.trim() || metadata.source !== metadata.source.trim()) throw new Error()
    source = metadata.source
  }
  if (metadata.name !== undefined) {
    if (typeof metadata.name !== "string") throw new Error()
    result.name = explicitModelName(metadata.name, row.id) ?? result.name
  }
  if (metadata.limit !== undefined) {
    if (!record(metadata.limit)) throw new Error()
    for (const key of ["context", "input", "output"]) {
      if (metadata.limit[key] === undefined) continue
      if (!positiveInteger(metadata.limit[key])) throw new Error()
      limit[key] = metadata.limit[key]
    }
  }
  if (Object.keys(limit).length) result.limit = limit
  if (metadata.modalities !== undefined) {
    if (!record(metadata.modalities)) throw new Error()
    const modalities = {}
    for (const key of ["input", "output"]) {
      const values = metadata.modalities[key]
      if (!Array.isArray(values) || !values.every((value) => ["text", "image", "audio", "video", "pdf"].includes(value))) {
        throw new Error()
      }
      modalities[key] = [...values]
    }
    result.modalities = modalities
  }
  for (const key of ["attachment", "reasoning", "tool_call"]) {
    if (metadata[key] === undefined) continue
    if (typeof metadata[key] !== "boolean") throw new Error()
    result[key] = metadata[key]
  }
  if (metadata.variants !== undefined) {
    if (!record(metadata.variants)) throw new Error()
    result.variants = Object.fromEntries(Object.entries(metadata.variants).map(([name, variant]) => {
      if (!name.trim() || !record(variant)) throw new Error()
      if (typeof variant.reasoningEffort === "string" && variant.reasoningEffort.trim() && variant.disabled === undefined) {
        return [name, { reasoningEffort: variant.reasoningEffort }]
      }
      if (variant.reasoningEffort === undefined && variant.disabled === true) {
        return [name, { disabled: true }]
      }
      throw new Error()
    }))
    // OpenCode adds SDK defaults before merging configured variants. An
    // authoritative proxy allowlist must explicitly suppress unsupported ones.
    for (const name of STANDARD_REASONING_VARIANTS) {
      if (!Object.hasOwn(result.variants, name)) result.variants[name] = { disabled: true }
    }
  }
  return { config: result, source }
}

function shouldRetryStatus(status) {
  return status === 408 || status === 425 || status === 429 || status >= 500
}

function remainingBudget(deadline) {
  return Math.max(0, deadline - Date.now())
}

async function fetchModels(url, headers) {
  const deadline = Date.now() + DISCOVERY_TOTAL_TIMEOUT_MS
  let lastFailure = "network error or discovery timeout"

  for (let attempt = 0; ; attempt++) {
    const remaining = remainingBudget(deadline)
    if (remaining <= 0) throw new Error(lastFailure)
    const timeout = Math.min(DISCOVERY_ATTEMPT_TIMEOUT_MS, remaining)

    try {
      const response = await fetch(url, {
        headers,
        signal: AbortSignal.timeout(timeout),
        redirect: "error",
      })

      if (!response.ok) {
        lastFailure = `HTTP ${response.status}`
        if (!shouldRetryStatus(response.status)) throw new Error(lastFailure)
        if (attempt >= DISCOVERY_RETRY_DELAYS_MS.length) throw new Error(lastFailure)
        const delay = Math.min(DISCOVERY_RETRY_DELAYS_MS[attempt], remainingBudget(deadline))
        if (delay > 0) await sleep(delay)
        continue
      }

      let body
      try {
        body = await response.json()
      } catch {
        lastFailure = "invalid models response"
        if (attempt >= DISCOVERY_RETRY_DELAYS_MS.length) throw new Error(lastFailure)
        const delay = Math.min(DISCOVERY_RETRY_DELAYS_MS[attempt], remainingBudget(deadline))
        if (delay > 0) await sleep(delay)
        continue
      }

      if (!record(body) || body.object !== "list" || !Array.isArray(body.data)) {
        throw new Error("invalid models response")
      }

      // A cold proxy can return an empty catalog while discovery warms up.
      // Retry before replacing models, and retain the fallback if it stays empty.
      if (body.data.length === 0) {
        lastFailure = "empty models response"
        if (attempt >= DISCOVERY_RETRY_DELAYS_MS.length) throw new Error(lastFailure)
        const delay = Math.min(DISCOVERY_RETRY_DELAYS_MS[attempt], remainingBudget(deadline))
        if (delay > 0) await sleep(delay)
        continue
      }

      return body
    } catch (error) {
      if (error instanceof Error && (
        error.message.startsWith("HTTP ") ||
        error.message === "invalid models response" ||
        error.message === "empty models response"
      )) {
        throw error
      }
      lastFailure = "network error or discovery timeout"
      if (attempt >= DISCOVERY_RETRY_DELAYS_MS.length) throw new Error(lastFailure)
      const delay = Math.min(DISCOVERY_RETRY_DELAYS_MS[attempt], remainingBudget(deadline))
      if (delay > 0) await sleep(delay)
    }
  }
}

function configuredProvider(config) {
  if (config.disabled_providers?.includes(PROVIDER_ID)) return
  if (config.enabled_providers && !config.enabled_providers.includes(PROVIDER_ID)) return
  return config.provider?.[PROVIDER_ID]
}

function providerRequest(provider, resource) {
  const { baseURL, apiKey, headers: configuredHeaders } = provider.options ?? {}
  if (typeof baseURL !== "string" || typeof apiKey !== "string" || !apiKey.trim()) throw new Error()
  const url = new URL(baseURL)
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error()
  url.pathname = `${url.pathname.replace(/\/+$/, "")}/${resource}`
  const headers = new Headers(configuredHeaders)
  headers.set("Authorization", `Bearer ${apiKey}`)
  headers.set("Accept", "application/json")
  return { url, headers }
}

async function OpenProxyModels() {
  return {
    async config(config) {
      const provider = configuredProvider(config)
      if (!provider) return

      // Snapshot this invocation's overrides before awaiting discovery. A later
      // hook invocation can still receive names generated by an earlier one.
      const configuredModels = { ...(provider.models ?? {}) }
      let failure = "invalid provider URL or credentials"
      try {
        const { url, headers } = providerRequest(provider, "models")
        failure = "network error or discovery timeout"
        let body
        try {
          body = await fetchModels(url, headers)
        } catch (error) {
          if (error instanceof Error && error.message) failure = error.message
          throw error
        }
        failure = "invalid models response"
        const entries = body.data.map((row) => {
          const { config: remote, source } = modelConfig(row)
          const local = Object.hasOwn(configuredModels, row.id) ? configuredModels[row.id] : {}
          const merged = { ...remote, ...local }
          if (typeof merged.name === "string") merged.name = sourceModelName(merged.name, source)
          if (remote.limit || local.limit) merged.limit = { ...remote.limit, ...local.limit }
          if (positiveInteger(merged.limit?.context)) {
            merged.limit.context = Math.min(merged.limit.context, MAX_CONTEXT_TOKENS)
          }
          if (positiveInteger(merged.limit?.input)) {
            merged.limit.input = Math.min(merged.limit.input, MAX_CONTEXT_TOKENS)
          }
          // OpenCode allows omitting limit, but requires context AND output
          // when present. Check after local overrides have filled any gaps.
          if (!positiveInteger(merged.limit?.context) || !positiveInteger(merged.limit?.output)) {
            delete merged.limit
          }
          if (remote.variants || local.variants) {
            merged.variants = { ...remote.variants, ...local.variants }
          }
          return [row.id, merged]
        })
        // Replace only after the entire response validates. Removed/disabled IDs
        // must not survive as local overrides after a successful discovery.
        provider.models = Object.fromEntries(entries)
      } catch {
        // Never log the request, response body, URL or raw exception (may contain secrets).
        console.warn(`[openproxy-models] ${failure}; keeping configured models.`)
      }
    },
  }
}

// TUI-only dependencies are supplied by OpenCode's runtime plugin loader.
// Discovery (including `opencode models`) never imports them.
export async function OpenProxySidebar(api) {
  if (api.lifecycle.signal.aborted) return
  const [{ createSignal, createRoot, createEffect }, { jsx, jsxs }] = await Promise.all([
    import("solid-js"), import("@opentui/solid/jsx-runtime"),
  ])
  if (api.lifecycle.signal.aborted) return
  const [accounts, setAccounts] = createSignal([])
  const [message, setMessage] = createSignal("Loading…")
  const [failed, setFailed] = createSignal(false)
  const [visible, setVisible] = createSignal(false)
  const [now, setNow] = createSignal(Date.now())
  let timer, clock, controller, disposeRoot, stopped = false, started = false, warmupRetried = false
  const stop = () => {
    if (stopped) return
    stopped = true
    clearTimeout(timer)
    clearInterval(clock)
    controller?.abort()
    disposeRoot?.()
    api.lifecycle.signal.removeEventListener("abort", stop)
  }
  api.lifecycle.signal.addEventListener("abort", stop, { once: true })
  api.lifecycle.onDispose(stop)

  async function poll(request) {
    if (stopped) return
    controller = new AbortController()
    const timeout = setTimeout(() => controller.abort(), 10000)
    let delay = 60000
    let failure = "Proxy unavailable"
    try {
      const response = await fetch(request.url, { headers: request.headers, redirect: "error", signal: controller.signal })
      if (!response.ok) { failure = `Proxy unavailable (HTTP ${response.status})`; throw new Error() }
      failure = "Invalid usage response"
      const body = await limitsResponse(response)
      const next = validateLimits(body)
      if (stopped) return
      setNow(Date.now())
      setAccounts(next)
      setFailed(false)
      setMessage(body.truncated ? "Showing first 128 accounts" : next.length ? "" : "No accounts")
      // One quick retry lets the proxy's background initial refresh complete.
      if (!warmupRetried && next.some((account) => account.status === "loading")) {
        warmupRetried = true
        delay = 2500
      }
    } catch {
      if (stopped) return
      setFailed(true)
      setMessage(failure) // No raw exception, URL, response body or credentials.
    } finally {
      clearTimeout(timeout)
      controller?.abort()
      controller = undefined
      if (!stopped) timer = setTimeout(() => poll(request), delay)
    }
  }

  createRoot((dispose) => {
    disposeRoot = dispose
    createEffect(() => {
      if (!api.state.ready || started || stopped) return
      started = true
      const provider = configuredProvider(api.state.config)
      if (!provider) return
      setVisible(true)
      try {
        const request = providerRequest(provider, "usage/limits")
        clock = setInterval(() => setNow(Date.now()), 30000)
        void poll(request)
      } catch {
        setFailed(true)
        setMessage("Invalid provider URL or credentials")
      }
    })
  })

  const clean = (value) => value.replace(/[\x00-\x1f\x7f-\x9f]/g, " ").slice(0, 180)
  const text = (value, tone = "textMuted", theme = api.theme) => jsx("text", {
    get fg() { return theme.current[tone] }, children: clean(value),
  })
  const reset = (date) => {
    if (!date) return "unknown"
    const minutes = Math.ceil((Date.parse(date) - now()) / 60000)
    if (minutes <= 0) return "due"
    if (minutes < 60) return `${minutes}m`
    if (minutes < 1440) return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
    return `${Math.floor(minutes / 1440)}d ${Math.floor(minutes % 1440 / 60)}h`
  }
  const quotaView = ([label, quota], theme) => {
    const percentage = quota.remainingPercentage !== null ? 100 - quota.remainingPercentage
      : quota.used !== null && quota.total > 0 ? quota.used / quota.total * 100 : null
    const used = percentage === null ? null : Math.max(0, Math.min(100, percentage))
    const filled = used === null ? 0 : Math.round(used / 10)
    const bar = used === null ? "??????????" : "━".repeat(filled) + "─".repeat(10 - filled)
    const amount = (value) => `${Number(value.toPrecision(6))}${quota.unit ? ` ${quota.unit}` : ""}`
    const balance = quota.remaining !== null ? `${amount(quota.remaining)} left` : null
    const usage = used === null ? balance ?? "unknown" : `${bar} ${Math.round(used)}% used`
    const detail = used !== null && quota.unit
      ? balance ?? (quota.used !== null ? `${amount(quota.used)} used` : null) : null
    return jsxs("box", { flexDirection: "column", children: [
      text(`${label} · reset ${reset(quota.resetAt)}`, "textMuted", theme),
      text(quota.unlimited ? "Unlimited" : `${usage}${detail ? ` · ${detail}` : ""}`,
        used === null || quota.unlimited ? "textMuted" : used >= 90 ? "error" : used >= 75 ? "warning" : "success", theme),
    ] })
  }
  const age = (observedAt) => {
    if (!observedAt) return "updated unknown"
    const minutes = Math.max(0, Math.floor((now() - Date.parse(observedAt)) / 60000))
    if (minutes < 1) return "updated just now"
    const elapsed = minutes < 60 ? `${minutes}m` : minutes < 1440 ? `${Math.floor(minutes / 60)}h` : `${Math.floor(minutes / 1440)}d`
    return `updated ${elapsed} ago`
  }
  function Panel({ theme }) {
    return jsx("box", { flexDirection: "column", gap: 1,
      get visible() { return visible() },
      get children() {
        return [text("Usage limits", "text", theme), ...(message() ? [text(message(), failed() ? "error" : "textMuted", theme)] : []),
          ...accounts().map((account) => {
            const stale = failed() || account.status === "stale" ||
              account.status === "fresh" && (!account.observedAt || now() - Date.parse(account.observedAt) >= 180000)
            const status = stale && account.status === "fresh" ? "stale" : account.status
            const tone = status === "unavailable" ? "error" : stale ? "warning" : "textMuted"
            const cached = failed() ? `${status === "stale" ? "" : " · stale"} (cached)` : ""
            return jsxs("box", { flexDirection: "column", children: [
              text(`${account.label} · ${account.provider}`, "text", theme),
              text(`${account.plan ? `${account.plan} · ` : ""}${status}${cached} · ${age(account.observedAt)}`, tone, theme),
              ...(account.error ? [text(account.error, tone, theme)] : []),
              ...(!Object.keys(account.quotas).length && ["fresh", "stale"].includes(status) ? [text("Quota unknown", "textMuted", theme)] : []),
              ...Object.entries(account.quotas).map((entry) => quotaView(entry, theme)),
            ] })
          }),
        ]
      },
    })
  }
  api.slots.register({ order: 101, slots: { sidebar_content(ctx) { return jsx(Panel, { theme: ctx.theme }) } } })
}

async function limitsResponse(response) {
  // Bound even a chunked response; the request timeout also covers its body.
  const reader = response.body.getReader()
  const chunks = []
  let size = 0
  try {
    for (;;) {
      const { value, done } = await reader.read()
      if (done) break
      size += value.length
      if (size > 2 * 1024 * 1024) throw new Error()
      chunks.push(value)
    }
  } finally { await reader.cancel(); reader.releaseLock() }
  const bytes = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length }
  return JSON.parse(new TextDecoder().decode(bytes))
}

function validateLimits(body) {
  const nullableString = (value) => value === null || typeof value === "string"
  const date = (value) => value === null || typeof value === "string" && Number.isFinite(Date.parse(value))
  const number = (value) => value === null || typeof value === "number" && Number.isFinite(value) && value >= 0
  if (!record(body) || body.refreshIntervalSeconds !== 180 || !Array.isArray(body.accounts) || body.accounts.length > 128 ||
      body.truncated !== undefined && typeof body.truncated !== "boolean") throw new Error()
  const ids = new Set()
  for (const account of body.accounts) {
    if (!record(account) || !["id", "provider", "label"].every((key) => typeof account[key] === "string" && account[key].trim()) ||
        ids.has(account.id) || !["loading", "fresh", "stale", "unavailable", "unsupported"].includes(account.status) ||
        !date(account.observedAt) || !nullableString(account.plan) || !nullableString(account.error) || !record(account.quotas)) throw new Error()
    ids.add(account.id)
    if (Object.keys(account.quotas).length > 64) throw new Error()
    for (const [label, quota] of Object.entries(account.quotas)) {
      if (!label.trim() || !record(quota) || !["used", "total", "remaining", "remainingPercentage"].every((key) => number(quota[key])) ||
          quota.remainingPercentage > 100 || !date(quota.resetAt) || typeof quota.unlimited !== "boolean" ||
          quota.unit !== undefined && typeof quota.unit !== "string") throw new Error()
    }
  }
  return body.accounts
}

export default { id: "openproxy.models", server: OpenProxyModels }
