import { RuntimeError, deadline } from './primitives.mjs'
import { MAX_FRAME_BYTES } from './native.mjs'

export class ProtocolWriter {
  #tail = Promise.resolve()
  #bytes = 0
  #frames = 0
  #failure
  constructor(output) {
    this.output = output
    // A disconnected supervisor must trigger controlled draining, not an unhandled EPIPE exit.
    output.on('error', (error) => { this.#failure = error })
  }
  send(value) {
    if (this.#failure) return Promise.reject(this.#failure)
    const line = JSON.stringify(value) + '\n'
    const bytes = Buffer.byteLength(line)
    if (bytes > MAX_FRAME_BYTES || this.#frames >= 256 || this.#bytes + bytes > 32 * 1024 * 1024) {
      return Promise.reject(new RuntimeError('Claude Agent protocol output queue exceeded its limit', 'RESOURCE_LIMIT'))
    }
    this.#bytes += bytes
    this.#frames++
    const task = this.#tail.then(() => {
      if (this.#failure) throw this.#failure
      return deadline(new Promise((resolve, reject) => {
        this.output.write(line, (error) => error ? reject(error) : resolve())
      }), 30_000, 'Claude Agent protocol output stalled')
    }).finally(() => { this.#bytes -= bytes; this.#frames-- })
    this.#tail = task.catch((error) => { this.#failure = error })
    return task
  }
  async flush() { await this.#tail; if (this.#failure) throw this.#failure }
}

export async function* commands(input) {
  let pending = Buffer.alloc(0)
  for await (const chunk of input) {
    pending = Buffer.concat([pending, Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)])
    let end
    while ((end = pending.indexOf(10)) >= 0) {
      if (end > MAX_FRAME_BYTES) throw new RuntimeError('Claude Agent command exceeds 32 MiB plus its IPC envelope', 'RESOURCE_LIMIT')
      const line = pending.subarray(0, end)
      pending = pending.subarray(end + 1)
      if (!line.length) continue
      try { yield JSON.parse(line.toString('utf8')) }
      catch (error) {
        if (!(error instanceof SyntaxError)) throw error
        yield { op: 'invalid', id: null }
      }
    }
    if (pending.length > MAX_FRAME_BYTES) throw new RuntimeError('Claude Agent command exceeds 32 MiB plus its IPC envelope', 'RESOURCE_LIMIT')
  }
  if (pending.length) throw new RuntimeError('Claude Agent command is missing its newline', 'INVALID_COMMAND')
}

const object = (value) => value && typeof value === 'object' && !Array.isArray(value)
export const validID = (id) => typeof id === 'string' && id.length > 0 && id.length <= 256
  || typeof id === 'number' && Number.isSafeInteger(id) && id >= 0
const sessionID = (id) => typeof id === 'string' && id.length > 0 && id.length <= 256
const invalid = (message) => { throw new RuntimeError(message, 'INVALID_REQUEST') }

export function validateRequest(value) {
  if (!object(value)) invalid('A run requires a request object')
  const allowed = new Set(['model', 'kind', 'sessionID', 'sourceID', 'system', 'prompt', 'tools', 'effort', 'maxOutputTokens'])
  if (Object.keys(value).some((name) => !allowed.has(name))) invalid('Unsupported Claude Agent request option')
  if (typeof value.model !== 'string' || !value.model || value.model.length > 256) invalid('A native model ID is required')
  if (!['primary', 'title', 'generate', 'compaction'].includes(value.kind)) invalid('Invalid Claude Agent request kind')
  if (value.sessionID !== undefined && !sessionID(value.sessionID)) invalid('Invalid native session ID')
  if (value.sourceID !== undefined && !sessionID(value.sourceID)) invalid('Invalid native source ID')
  if (value.effort !== undefined && !['low', 'medium', 'high', 'xhigh', 'max'].includes(value.effort)) invalid('Claude Agent effort must be low, medium, high, xhigh or max')
  if (value.maxOutputTokens !== undefined && (!Number.isSafeInteger(value.maxOutputTokens) || value.maxOutputTokens <= 0)) invalid('maxOutputTokens must be a positive integer')
  if (!object(value.system)) invalid('A run requires explicit OpenCode system instructions')
  for (const name of Object.keys(value.system)) {
    if (!['agent', 'instructions'].includes(name) || typeof value.system[name] !== 'string') invalid('Invalid OpenCode system instructions')
  }
  if (!Array.isArray(value.prompt) || value.prompt.length > 8192) invalid('Invalid or oversized LMV3 prompt')
  for (const message of value.prompt) {
    if (!object(message) || !['system', 'user', 'assistant', 'tool'].includes(message.role)) invalid('Invalid LMV3 message')
    if (message.role === 'system') {
      if (typeof message.content !== 'string') invalid('LMV3 system content must be text')
      continue
    }
    if (!Array.isArray(message.content) || message.content.length > 4096) invalid('Invalid LMV3 content')
    for (const part of message.content) {
      if (!object(part) || !['text', 'reasoning', 'file', 'tool-call', 'tool-result'].includes(part.type)) invalid('Unsupported LMV3 content part')
      if ((part.type === 'text' || part.type === 'reasoning') && typeof part.text !== 'string') invalid('Invalid LMV3 text')
      if (part.type === 'file' && (typeof part.data !== 'string' || typeof part.mediaType !== 'string')) invalid('Attachments must contain base64 data or an HTTP(S) URL string')
      if (part.type === 'tool-call' || part.type === 'tool-result') {
        if (typeof part.toolCallId !== 'string' || !part.toolCallId || part.toolCallId.length > 512 || typeof part.toolName !== 'string') invalid('Invalid LMV3 tool ID or name')
      }
      if (part.type === 'tool-result' && (!object(part.output) || !['text', 'error-text', 'json', 'error-json', 'content', 'execution-denied'].includes(part.output.type))) invalid('Invalid LMV3 tool output')
    }
  }
  if (!Array.isArray(value.tools) || value.tools.length > 256) invalid('Invalid or oversized LMV3 tool list')
  for (const tool of value.tools) {
    if (!object(tool) || typeof tool.type !== 'string') invalid('Invalid LMV3 tool')
    if (tool.type !== 'function') continue
    if (typeof tool.name !== 'string' || !tool.name || tool.name.length > 1024 || !object(tool.inputSchema)
      || tool.description !== undefined && typeof tool.description !== 'string') invalid('Invalid LMV3 function tool')
  }
  return value
}
