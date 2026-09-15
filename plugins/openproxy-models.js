// Auto-loaded from ~/.config/opencode/plugins/. The key stays in provider.options.
const PROVIDER_ID = "ludka2"
const DISCOVERY_TIMEOUT_MS = 10000
const MAX_CONTEXT_TOKENS = 500000
const STANDARD_REASONING_VARIANTS = ["none", "minimal", "low", "medium", "high", "xhigh", "max"]

function record(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

function positiveInteger(value) {
  return Number.isSafeInteger(value) && value > 0
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
  const suffix = ` · ${source}`
  return name.endsWith(suffix) ? name : `${name}${suffix}`
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
    result.name = normalizeModelName(explicitModelName(metadata.name, row.id) ?? result.name)
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

export default async function OpenProxyModels() {
  return {
    async config(config) {
      const provider = config.provider?.[PROVIDER_ID]
      if (!provider || config.disabled_providers?.includes(PROVIDER_ID)) return
      if (config.enabled_providers && !config.enabled_providers.includes(PROVIDER_ID)) return

      let failure = "invalid provider URL or credentials"
      try {
        const { baseURL, apiKey, headers: configuredHeaders } = provider.options ?? {}
        if (typeof baseURL !== "string" || typeof apiKey !== "string" || !apiKey.trim()) throw new Error()
        const url = new URL(baseURL)
        if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
          throw new Error()
        }
        url.pathname = `${url.pathname.replace(/\/+$/, "")}/models`
        const headers = new Headers(configuredHeaders)
        headers.set("Authorization", `Bearer ${apiKey}`)
        headers.set("Accept", "application/json")
        failure = "network error or discovery timeout"
        const response = await fetch(url, {
          headers,
          signal: AbortSignal.timeout(DISCOVERY_TIMEOUT_MS),
          redirect: "error",
        })
        failure = `HTTP ${response.status}`
        if (!response.ok) throw new Error()
        failure = "invalid models response"
        const body = await response.json()
        if (!record(body) || body.object !== "list" || !Array.isArray(body.data)) throw new Error()
        const entries = body.data.map((row) => {
          const { config: remote, source } = modelConfig(row)
          const local = Object.hasOwn(provider.models ?? {}, row.id) ? provider.models[row.id] : {}
          const merged = { ...remote, ...local }
          if (typeof merged.name === "string") merged.name = normalizeModelName(merged.name)
          if (source && typeof merged.name === "string") merged.name = sourceModelName(merged.name, source)
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
