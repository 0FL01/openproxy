import { Plugin } from "@opencode/plugin"

const PROVIDER_ID = "ludka2"
// Stay above OpenProxy's ~10-second upstream discovery timeout on cold starts.
const DISCOVERY_ATTEMPT_TIMEOUT_MS = 15000
const DISCOVERY_TOTAL_TIMEOUT_MS = 30000
const DISCOVERY_RETRY_DELAYS_MS = [250, 750, 1500]
const MAX_CONTEXT_TOKENS = 500000
const NATIVE_SOURCE = "claude-agent"
const NATIVE_PACKAGE = "@openproxy/claude-agent"
const NATIVE_MODEL_PACKAGE = `aisdk:${NATIVE_PACKAGE}`
const KIND_HEADER = "x-opencode-claude-agent-kind"
const SESSION_HEADER = "x-opencode-session-id"
const FORK_HEADER = "x-opencode-claude-agent-fork-source"
const AGENT_OPTION = "claudeCodeAgent"
const INSTRUCTIONS_OPTION = "claudeCodeInstructions"
const REQUEST_KINDS = ["primary", "title", "generate", "compaction"]
const MAX_FRAME_CHARS = 32 * 1024 * 1024 + 4096

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
  result.native = source === NATIVE_SOURCE
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
  return { url, headers }
}

// A fixed local adapter, not a package to download. Only the configured proxy
// receives the OpenProxy key; neither catalog metadata nor SDK options choose
// transport URLs, headers or credentials.
function nativeTransport(modelsURL, discoveryHeaders) {
  const root = new URL(modelsURL)
  if (!/\/v1\/models$/.test(root.pathname)) throw new Error("Claude Agent requires a /v1 provider URL")
  root.pathname = root.pathname.replace(/\/v1\/models$/, "")
  const endpoint = (path) => {
    const url = new URL(root)
    url.pathname = `${root.pathname}${path}`
    return url
  }
  const headers = new Headers(discoveryHeaders)
  headers.set("Content-Type", "application/json")
  headers.set("Accept", "application/x-ndjson")
  return {
    async stream(body, signal) {
      let response
      try {
        response = await fetch(endpoint("/v2/claude-agent/stream"), {
          method: "POST", headers, body: JSON.stringify(body), signal, redirect: "error",
        })
      } catch {
        signal.throwIfAborted()
        throw nativeError({ message: "Claude Agent transport failed", code: "transport_error" })
      }
      if (!response.ok) {
        let detail
        try { detail = await response.json() } catch {}
        throw nativeError(detail?.error ?? detail, `Claude Agent request failed (HTTP ${response.status})`)
      }
      if (response.headers.get("Content-Type")?.split(";")[0].trim().toLowerCase() !== "application/x-ndjson" || !response.body) {
        await response.body?.cancel()
        throw nativeError({ message: "Invalid Claude Agent stream content type", code: "invalid_stream" })
      }
      return response.body
    },
    async idle(sessionID, signal) {
      const response = await fetch(endpoint("/v2/claude-agent/idle"), {
        method: "POST", headers, body: JSON.stringify({ sessionID }),
        signal: AbortSignal.any([signal, AbortSignal.timeout(10000)]), redirect: "error",
      })
      await response.body?.cancel()
      if (!response.ok) throw new Error("Idle request failed")
    },
  }
}

function nativeError(detail, fallback = "Claude Agent request failed") {
  const message = typeof detail?.message === "string" && detail.message.trim() ? detail.message.slice(0, 4096) : fallback
  const error = new Error(message)
  error.name = "ClaudeAgentError"
  if (typeof detail?.code === "string") error.code = detail.code.slice(0, 128)
  error.value = { message, ...(error.code ? { code: error.code } : {}) }
  return error
}

function invalidStream() {
  return nativeError({ message: "Invalid or incomplete Claude Agent stream", code: "invalid_stream" })
}

function validPart(part) {
  if (!record(part) || typeof part.type !== "string") return false
  switch (part.type) {
    case "stream-start": return Array.isArray(part.warnings)
    case "response-metadata": return part.id === undefined || typeof part.id === "string"
    case "text-start": case "text-end": case "reasoning-start": case "reasoning-end": case "tool-input-end":
      return typeof part.id === "string"
    case "text-delta": case "reasoning-delta": case "tool-input-delta":
      return typeof part.id === "string" && typeof part.delta === "string"
    case "tool-input-start": return typeof part.id === "string" && typeof part.toolName === "string"
    case "tool-call": return typeof part.toolCallId === "string" && typeof part.toolName === "string" && typeof part.input === "string"
    case "tool-result": return typeof part.toolCallId === "string" && typeof part.toolName === "string" && "result" in part
    case "finish": {
      if (!record(part.finishReason) || !["stop", "length", "tool-calls", "content-filter", "error", "other"].includes(part.finishReason.unified)) return false
      if (part.finishReason.raw !== undefined && typeof part.finishReason.raw !== "string") return false
      if (!record(part.usage?.inputTokens) || !record(part.usage?.outputTokens)) return false
      return [part.usage.inputTokens, part.usage.outputTokens].every((tokens) =>
        Object.values(tokens).every((value) => value === null || (Number.isSafeInteger(value) && value >= 0)))
    }
    case "error": return record(part.error) && typeof part.error.message === "string"
    default: return false
  }
}

async function* nativeParts(body, signal, onComplete) {
  const reader = body.getReader()
  const decoder = new TextDecoder("utf-8", { fatal: true })
  let buffer = ""
  let started = false
  let finish
  let complete = false
  const abort = () => { void reader.cancel().catch(() => {}) }
  signal.addEventListener("abort", abort, { once: true })
  try {
    for (;;) {
      signal.throwIfAborted()
      let next
      try { next = await reader.read() } catch {
        signal.throwIfAborted()
        throw nativeError({ message: "Claude Agent stream transport failed", code: "transport_error" })
      }
      const { done, value } = next
      signal.throwIfAborted()
      try { buffer += decoder.decode(value, { stream: !done }) } catch { throw invalidStream() }
      let newline
      while ((newline = buffer.indexOf("\n")) !== -1) {
        if (newline > MAX_FRAME_CHARS) throw invalidStream()
        const line = buffer.slice(0, newline)
        buffer = buffer.slice(newline + 1)
        let part
        try { part = JSON.parse(line) } catch { throw invalidStream() }
        if (!validPart(part) || finish) throw invalidStream()
        if (part.type === "error") throw nativeError(part.error)
        if (!started && part.type !== "stream-start") throw invalidStream()
        if (part.type === "stream-start") {
          if (started) throw invalidStream()
          started = true
        }
        // Withhold the terminal part until framing/EOF have been verified.
        if (part.type === "finish") finish = part
        else yield part
      }
      if (buffer.length > MAX_FRAME_CHARS) throw invalidStream()
      if (!done) continue
      if (buffer.length || !finish) throw invalidStream()
      complete = true
      onComplete()
      yield finish
      return
    }
  } finally {
    signal.removeEventListener("abort", abort)
    if (!complete) await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
}

// LanguageModelV3 attachments can contain bytes/URL objects. Preserve their
// JSON shapes, encoding only binary values as base64, never object-index bytes.
function wireValue(value) {
  if (value instanceof URL) return value.href
  if (value instanceof ArrayBuffer) return Buffer.from(value).toString("base64")
  if (ArrayBuffer.isView(value)) return Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString("base64")
  if (Array.isArray(value)) return value.map(wireValue)
  if (record(value)) return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined).map(([key, item]) => [key, wireValue(item)]))
  if (value === null || ["string", "boolean"].includes(typeof value) || (typeof value === "number" && Number.isFinite(value))) return value
  throw new Error("Claude Agent prompt/tools must be JSON serializable")
}

class RemoteClaudeAgentModel {
  specificationVersion = "v3"
  provider = NATIVE_SOURCE
  supportedUrls = {}

  constructor(modelId, transport, onPrimary, clientEnvironment, lifetime, optionKey) {
    this.modelId = modelId
    this.transport = transport
    this.onPrimary = onPrimary
    this.clientEnvironment = clientEnvironment
    this.lifetime = lifetime
    this.optionKey = optionKey
  }

  async doGenerate() {
    throw new Error("Claude Agent models only support streaming")
  }

  async doStream(options) {
    const headers = new Headers(options.headers)
    const kind = headers.get(KIND_HEADER) ?? "generate"
    if (!REQUEST_KINDS.includes(kind)) throw new Error("Invalid Claude Agent request kind")
    const session = headers.get(SESSION_HEADER) || undefined
    if (kind === "primary" && !session) throw new Error("Claude Agent primary requests require a client session")
    const primary = kind === "primary"
    const branch = kind === "generate" || kind === "compaction"
    // The public SDK bridge uses the model's resolved canonical provider (or
    // ludka2 when unset) as the namespace for this fixed package's settings.
    const settings = options.providerOptions?.[this.optionKey] ?? {}
    const effort = settings.effort ?? settings.reasoningEffort
    if (effort !== undefined && !["low", "medium", "high", "xhigh", "max"].includes(effort)) throw new Error("Claude Agent effort must be low, medium, high, xhigh or max")
    const tools = options.toolChoice?.type === "none" || !(primary || branch)
      ? [] : (options.tools ?? []).filter((tool) => tool.type === "function")
    const body = wireValue({
      model: this.modelId, kind,
      sessionID: primary ? session : undefined,
      sourceID: primary ? headers.get(FORK_HEADER) || undefined : branch ? session : undefined,
      system: {
        agent: typeof settings[AGENT_OPTION] === "string" ? settings[AGENT_OPTION] : "",
        instructions: [typeof settings[INSTRUCTIONS_OPTION] === "string" ? settings[INSTRUCTIONS_OPTION] : "", this.clientEnvironment].filter(Boolean).join("\n\n"),
      },
      prompt: options.prompt, tools, effort, maxOutputTokens: options.maxOutputTokens,
    })
    const abort = new AbortController()
    const signal = AbortSignal.any([abort.signal, this.lifetime, ...(options.abortSignal ? [options.abortSignal] : [])])
    signal.throwIfAborted()
    if (primary) this.onPrimary(session)
    const source = await this.transport.stream(body, signal)
    let complete = false
    const parts = nativeParts(source, signal, () => { complete = true })
    const stream = new ReadableStream({
      async pull(controller) {
        try {
          const part = await parts.next()
          if (part.done) controller.close()
          else controller.enqueue(part.value)
        } catch (error) {
          abort.abort(error)
          controller.error(error)
        }
      },
      async cancel() {
        if (!complete) abort.abort()
        if (!source.locked) await source.cancel().catch(() => {})
        await parts.return()
      },
    }, { highWaterMark: 0 })
    return { stream }
  }
}

// Session/instruction behavior follows OpenCode PR #58782, f29e281322b35b525e337d12e84aedc7e129fd6d,
// packages/core/src/plugin/provider/claude-agent/{aisdk,session,instructions}.ts (MIT).
async function nativeHooks(ctx, ids, transport) {
  const registrations = []
  const lifetime = new AbortController()
  const sessions = new Set()
  const matches = (event) => event.model?.providerID === PROVIDER_ID && ids.has(event.model.id)
  const clientEnvironment = [
    "# Tool execution environment (OpenCode client)",
    "Tools and permissions run on the OpenCode client host, not the OpenProxy/Claude server.",
    `Client working directory: ${JSON.stringify(ctx.location.directory)}`,
    `Client platform: ${process.platform} (${process.arch})`,
    "The server working directory is not the client workspace. Use the client tools for files and commands.",
  ].join("\n")
  const hook = async (domain, name, callback) => {
    registrations.push(await ctx[domain].hook(name, async (event) => {
      if (matches(event)) await callback(event)
    }, { providerID: PROVIDER_ID }))
  }
  try {
    await hook("aisdk", "sdk", (event) => {
      if (event.package !== NATIVE_PACKAGE) return
      event.sdk = { languageModel(modelID) {
        if (!ids.has(modelID)) throw new Error("Unknown Claude Agent routed model")
        return new RemoteClaudeAgentModel(modelID, transport, (sessionID) => sessions.add(sessionID), clientEnvironment, lifetime.signal, event.model.canonical ?? PROVIDER_ID)
      } }
    })
    await hook("session", "model.request", async (event) => {
      event.headers[KIND_HEADER] = event.kind
      event.headers[SESSION_HEADER] = event.sessionID
      delete event.headers[FORK_HEADER]
      if (event.kind !== "primary") return
      sessions.add(event.sessionID)
      const session = await ctx.session.get({ sessionID: event.sessionID })
      if (session.fork) event.headers[FORK_HEADER] = session.fork.sessionID
    })
    await hook("session", "context", (event) => {
      delete event.options.maxTokens
      event.messages = event.messages.map((message) => ({ ...message, content: message.content.map((part) => {
        if (part.type !== "tool-result" || part.result.type !== "error") return part
        return { ...part, providerMetadata: { ...part.providerMetadata, [NATIVE_SOURCE]: {
          ...part.providerMetadata?.[NATIVE_SOURCE], toolError: true,
          toolAborted: part.result.value?.error?.type === "aborted",
        } } }
      }) }))
    })
    await hook("session", "retry", (event) => { event.decision = { retry: false } })
    for (const kind of ["context", "generate", "compaction"]) {
      await hook("session", kind, async (event) => {
        const agent = await ctx.agent.get({ agentID: event.agent })
        event.options[AGENT_OPTION] = agent.data.system ?? ""
        const instructions = []
        for (const part of event.system.slice(1)) {
          const sources = part.metadata?.experimental?.instructions
          if (!Array.isArray(sources) || !sources.every((source) => typeof source?.key === "string" && typeof source?.text === "string")) {
            instructions.push(part.text)
            continue
          }
          for (const source of sources) if (!["core/date", "core/environment"].includes(source.key)) instructions.push(source.text)
        }
        event.options[INSTRUCTIONS_OPTION] = instructions.join("\n\n")
      })
    }
    const events = ctx.event.subscribe({
      types: ["session.execution.succeeded", "session.execution.failed", "session.execution.interrupted"],
      signal: lifetime.signal,
    })
    // Only terminal execution events arm idle. A model's tool-call EOF is not
    // execution completion: OpenCode may still be waiting for permission.
    const task = (async () => {
      for await (const event of events) {
        if (!sessions.delete(event.data.sessionID)) continue
        try { await transport.idle(event.data.sessionID, lifetime.signal) }
        catch { if (!lifetime.signal.aborted) console.warn("[openproxy-models] Claude Agent idle notification failed.") }
      }
    })().catch(() => {
      if (!lifetime.signal.aborted) console.warn("[openproxy-models] Claude Agent execution events unavailable.")
    })
    return async () => {
      lifetime.abort()
      await Promise.allSettled(registrations.map((registration) => registration.dispose()))
      await task
      sessions.clear()
    }
  } catch (error) {
    lifetime.abort()
    await Promise.allSettled(registrations.map((registration) => registration.dispose()))
    throw error
  }
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
    let transportConfig
    let failure = "invalid provider URL or credentials"
    try {
      const { url, headers } = modelsRequest()
      transportConfig = { url, headers }
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
    const nativeIds = new Set(rows.filter((row) => row.native).map((row) => row.id))
    let disposeNative
    if (nativeIds.size) {
      try { disposeNative = await nativeHooks(ctx, nativeIds, nativeTransport(transportConfig.url, transportConfig.headers)) }
      catch {
        console.warn("[openproxy-models] Claude Agent requires the OpenCode V2 AI SDK/session hooks; keeping native models.")
        return
      }
    }
    const registration = await ctx.model.transform((editor) => {
      // The successful catalog is authoritative; rebuilds cannot retain IDs
      // removed by the proxy. ConfigProvider's later explicit overrides win.
      for (const model of editor.list(PROVIDER_ID)) {
        if (!ids.has(model.id)) editor.remove(PROVIDER_ID, model.id)
      }
      for (const row of rows) {
        editor.update(PROVIDER_ID, row.id, (model) => {
          model.name = row.name
          if (row.native) {
            model.package = NATIVE_MODEL_PACKAGE
            model.modelID = row.id
            // Match native toModel: the SDK reports context, not an output
            // budget. Keep Model.Info's default context when none was probed.
            model.limit = { context: row.limit?.context ?? model.limit.context, output: 0 }
            model.cost = []
            const upstreamID = row.id.slice(row.id.indexOf("/") + 1)
            model.family = upstreamID.includes("haiku") ? "claude-haiku" : undefined
            model.capabilities = { tools: true, input: ["text", "image", "pdf"], output: ["text"] }
          } else {
            if (model.package === NATIVE_MODEL_PACKAGE) delete model.package
            const limit = { ...model.limit, ...row.limit }
            if (positiveInteger(limit.context) && positiveInteger(limit.output)) {
              limit.context = Math.min(limit.context, MAX_CONTEXT_TOKENS)
              if (positiveInteger(limit.input)) limit.input = Math.min(limit.input, MAX_CONTEXT_TOKENS)
              model.limit = limit
            }
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
    }).catch(async (error) => { await disposeNative?.(); throw error })
    return async () => {
      await disposeNative?.()
      await registration.dispose()
    }
  },
})
