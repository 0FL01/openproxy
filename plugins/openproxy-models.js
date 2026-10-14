import { Plugin } from "@opencode/plugin"

const PROVIDER_ID = "ludka2"
// Stay above OpenProxy's ~10-second upstream discovery timeout on cold starts.
const DISCOVERY_ATTEMPT_TIMEOUT_MS = 15000
const DISCOVERY_TOTAL_TIMEOUT_MS = 30000
const DISCOVERY_RETRY_DELAYS_MS = [250, 750, 1500]
const MAX_CONTEXT_TOKENS = 500000

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

function sourceModelName(name, source) {
  if (!source) return name.replace(/\bglm\b/gi, "GLM")
  const suffix = ` · ${source}`
  while (name.slice(-suffix.length).toLowerCase() === suffix.toLowerCase()) {
    name = name.slice(0, -suffix.length)
  }
  return `${name.replace(/\bglm\b/gi, "GLM")}${suffix}`
}

// Only accept model metadata, never remote SDK/URL/header/options overrides.
function modelMetadata(row) {
  if (!record(row) || typeof row.id !== "string" || !row.id.trim() || row.id !== row.id.trim()) throw new Error()
  const result = { id: row.id, name: prettyModelName(row.id) }
  const limit = {}
  for (const [source, target] of [["context_length", "context"], ["max_completion_tokens", "output"]]) {
    if (row[source] === undefined) continue
    if (!positiveInteger(row[source])) throw new Error()
    limit[target] = row[source]
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
    const name = metadata.name.trim()
    const withoutPrefix = row.id.includes("/") ? row.id.slice(row.id.indexOf("/") + 1) : row.id
    if (name && ![row.id, withoutPrefix].includes(name)) result.name = name
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
    const capabilities = {}
    for (const key of ["input", "output"]) {
      const values = metadata.modalities[key]
      if (!Array.isArray(values) || !values.every((value) => ["text", "image", "audio", "video", "pdf"].includes(value))) throw new Error()
      capabilities[key] = [...values]
    }
    result.capabilities = capabilities
  }
  for (const key of ["attachment", "reasoning", "tool_call"]) {
    if (metadata[key] === undefined) continue
    if (typeof metadata[key] !== "boolean") throw new Error()
    // V2 expresses attachments via modalities; it has no reasoning boolean.
    if (key === "tool_call") result.capabilities = { ...result.capabilities, tools: metadata[key] }
  }
  if (metadata.variants !== undefined) {
    if (!record(metadata.variants)) throw new Error()
    result.variants = Object.entries(metadata.variants).flatMap(([id, variant]) => {
      if (!id.trim() || !record(variant)) throw new Error()
      if (typeof variant.reasoningEffort === "string" && variant.reasoningEffort.trim() && variant.disabled === undefined) {
        return [{ id, settings: { reasoningEffort: variant.reasoningEffort } }]
      }
      if (variant.reasoningEffort === undefined && variant.disabled === true) return []
      throw new Error()
    })
  }
  result.name = sourceModelName(result.name, source)
  return result
}

function modelsRequest() {
  const baseURL = process.env.LUDKA2_API_URL
  const apiKey = process.env.LUDKA2_API_KEY
  if (typeof baseURL !== "string" || typeof apiKey !== "string" || !apiKey.trim()) throw new Error()
  const url = new URL(baseURL)
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error()
  url.pathname = `${url.pathname.replace(/\/+$/, "")}/models`
  const headers = new Headers({ Authorization: `Bearer ${apiKey}`, Accept: "application/json" })
  // Marked requests include the Claude catalog as well as the other sources.
  headers.set("X-OpenProxy-Claude-Mask", "1")
  return { url, headers }
}

async function fetchModels(url, headers) {
  const deadline = Date.now() + DISCOVERY_TOTAL_TIMEOUT_MS
  const remaining = () => Math.max(0, deadline - Date.now())
  for (let attempt = 0; ; attempt++) {
    if (remaining() <= 0) throw new Error()
    let response
    try {
      response = await fetch(url, {
        headers,
        signal: AbortSignal.timeout(Math.min(DISCOVERY_ATTEMPT_TIMEOUT_MS, remaining())),
        redirect: "error",
      })
    } catch {
      // Raw network errors can contain the URL or credentials; discard them.
    }
    if (response && !response.ok) {
      const status = response.status
      if (!(status === 408 || status === 425 || status === 429 || status >= 500)) throw new Error()
    }
    if (response?.ok) {
      let body
      try { body = await response.json() } catch {}
      if (body !== undefined) {
        if (!record(body) || body.object !== "list" || !Array.isArray(body.data)) throw new Error()
        if (body.data.length) return body
      }
      // A cold proxy can return an empty catalog before discovery warms up.
    }
    if (attempt >= DISCOVERY_RETRY_DELAYS_MS.length) throw new Error()
    const delay = Math.min(DISCOVERY_RETRY_DELAYS_MS[attempt], remaining())
    if (delay > 0) await sleep(delay)
  }
}

export default Plugin.define({
  id: "openproxy.models",
  async setup(ctx) {
    // Local setup precedes ConfigProvider registration. Read env here and let
    // the model editor enforce provider availability when this transform runs.
    let rows
    let failure = "invalid provider URL or credentials"
    try {
      const { url, headers } = modelsRequest()
      failure = "model discovery failed"
      const body = await fetchModels(url, headers)
      failure = "invalid models response"
      rows = body.data.map(modelMetadata)
      if (new Set(rows.map((row) => row.id)).size !== rows.length) throw new Error()
    } catch {
      console.warn(`[openproxy-models] ${failure}; keeping native models.`)
      return
    }

    const ids = new Set(rows.map((row) => row.id))
    const registration = await ctx.model.transform((editor) => {
      // The successful catalog is authoritative; rebuilds cannot retain IDs
      // removed by the proxy. ConfigProvider's later explicit overrides win.
      for (const model of editor.list(PROVIDER_ID)) {
        if (!ids.has(model.id)) editor.remove(PROVIDER_ID, model.id)
      }
      for (const row of rows) {
        editor.update(PROVIDER_ID, row.id, (model) => {
          model.name = row.name
          const limit = { ...model.limit, ...row.limit }
          if (positiveInteger(limit.context) && positiveInteger(limit.output)) {
            limit.context = Math.min(limit.context, MAX_CONTEXT_TOKENS)
            if (positiveInteger(limit.input)) limit.input = Math.min(limit.input, MAX_CONTEXT_TOKENS)
            model.limit = limit
          }
          if (row.capabilities) {
            model.capabilities = { ...model.capabilities, ...row.capabilities,
              input: [...(row.capabilities.input ?? model.capabilities.input)],
              output: [...(row.capabilities.output ?? model.capabilities.output)],
            }
          }
          if (row.variants) model.variants = row.variants.map((variant) => ({ id: variant.id, settings: { ...variant.settings } }))
        })
      }
    })
    return () => registration.dispose()
  },
})
