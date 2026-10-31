// Port of PR #58782 language-model/{usage,convert-claude-stream,claude-agent-error}.ts. MIT.
import { originalName } from './tools.mjs'
import { errorJSON } from './primitives.mjs'

export function usageTracker() {
  const messages = new Map()
  let last
  function update(id, usage = {}) {
    const saved = messages.get(id)
    if (!saved && messages.size >= 4096) throw new Error('Too many native model responses in one step')
    const counts = saved ?? { input: 0, read: 0, write: 0, output: 0, reasoning: 0 }
    if (!saved) { messages.set(id, counts); last = id }
    counts.input = Math.max(counts.input, usage.input_tokens ?? 0)
    counts.read = Math.max(counts.read, usage.cache_read_input_tokens ?? 0)
    counts.write = Math.max(counts.write, usage.cache_creation_input_tokens ?? 0)
    counts.output = Math.max(counts.output, usage.output_tokens ?? 0)
    counts.reasoning = Math.max(counts.reasoning, usage.output_tokens_details?.thinking_tokens ?? 0)
  }
  return {
    update,
    result(usage) { if (messages.size <= 1) update(last ?? 'result', usage) },
    snapshot() {
      let output = 0
      let reasoning = 0
      for (const counts of messages.values()) { output += counts.output; reasoning += counts.reasoning }
      const counts = messages.get(last)
      return {
        inputTokens: {
          total: counts ? counts.input + counts.read + counts.write : undefined,
          noCache: counts?.input, cacheRead: counts?.read, cacheWrite: counts?.write,
        },
        outputTokens: { total: output, reasoning, text: output - reasoning },
      }
    },
  }
}

function errorTracker() {
  let code
  let rejected
  return (message) => {
    if (message.type === 'rate_limit_event') rejected = message.rate_limit_info.status === 'rejected' ? message.rate_limit_info : undefined
    if (message.type === 'assistant' && message.parent_tool_use_id === null && message.error) code = message.error
    if (message.type !== 'result') return
    const status = message.subtype === 'success' ? message.api_error_status ?? undefined : undefined
    if (message.subtype === 'success' && !message.is_error && !(status >= 400)) return
    const result = message.subtype === 'success' ? message.result : (message.errors ?? []).join('\n')
    const failure = status >= 500
      ? (status === 529 ? 'Anthropic is overloaded (529).' : `Anthropic returned HTTP ${status}.`) + ' Claude Code exhausted its retries. Try again shortly.'
      : result
    const limited = code === 'rate_limit' || (rejected && status === 429)
    const window = rejected?.rateLimitType?.replaceAll('_', ' ')
    const limit = limited && !failure ? `Claude Code subscription limit reached${window ? ` (${window})` : ''}.` : failure
    const reset = limited && rejected?.resetsAt ? ` Resets at ${new Date(rejected.resetsAt * 1000).toISOString()}.` : ''
    const billing = code === 'billing_error' ? ' Check your Claude plan and extra-usage settings.' : ''
    const auth = code === 'authentication_failed' ? ' Use the native Claude Agent login in OpenProxy.' : ''
    const error = new Error((limit ?? '') + reset + billing + auth || `Claude Code could not finish its reply${status ? ` (HTTP ${status})` : ''}`)
    if (code) error.code = code
    return error
  }
}

export function streamConverter(tools) {
  const usage = usageTracker()
  const failure = errorTracker()
  const streamed = new Set()
  const emittedTools = new Set()
  const blocks = new Map()
  let messageID = ''
  let calledTools = false
  let metadata
  return (message) => {
    const parts = []
    const error = failure(message)
    if (error) return [{ type: 'error', error: errorJSON(error) }]
    if (message.type === 'stream_event' && message.parent_tool_use_id === null) {
      const event = message.event
      switch (event.type) {
        case 'message_start':
          messageID = event.message.id
          metadata = undefined
          if (streamed.size >= 4096) throw new Error('Too many native response events')
          streamed.add(messageID)
          usage.update(messageID, event.message.usage)
          parts.push({ type: 'response-metadata', id: messageID, modelId: event.message.model })
          break
        case 'content_block_start': {
          const block = event.content_block
          const id = `${messageID}:${event.index}`
          if (blocks.size >= 4096) throw new Error('Too many native content blocks')
          if (block.type === 'text') {
            blocks.set(event.index, 'text')
            parts.push({ type: 'text-start', id })
            if (block.text) parts.push({ type: 'text-delta', id, delta: block.text })
          }
          if (block.type === 'thinking') {
            blocks.set(event.index, 'reasoning')
            parts.push({ type: 'reasoning-start', id })
            if (block.thinking) parts.push({ type: 'reasoning-delta', id, delta: block.thinking })
          }
          break
        }
        case 'content_block_delta': {
          const id = `${messageID}:${event.index}`
          if (event.delta.type === 'text_delta') parts.push({ type: 'text-delta', id, delta: event.delta.text })
          if (event.delta.type === 'thinking_delta' && event.delta.thinking) parts.push({ type: 'reasoning-delta', id, delta: event.delta.thinking })
          break
        }
        case 'content_block_stop': {
          const kind = blocks.get(event.index)
          if (kind) parts.push({ type: `${kind}-end`, id: `${messageID}:${event.index}`, providerMetadata: metadata })
          blocks.delete(event.index)
          break
        }
        case 'message_delta': usage.update(messageID, event.usage); break
        case 'message_stop':
          if (calledTools) parts.push({ type: 'finish', finishReason: { unified: 'tool-calls', raw: 'tool_use' }, usage: usage.snapshot() })
          break
      }
    }
    if (message.type === 'assistant' && message.parent_tool_use_id === null) {
      if (message.error) return parts
      metadata = { 'claude-agent': { sessionID: message.session_id, messageID: message.uuid } }
      usage.update(message.message.id, message.message.usage)
      for (const block of message.message.content) {
        if (block.type !== 'tool_use') continue
        const toolName = originalName(block.name, tools)
        if (!toolName || emittedTools.has(block.id)) continue
        emittedTools.add(block.id)
        calledTools = true
        parts.push({ type: 'tool-call', toolCallId: block.id, toolName, input: JSON.stringify(block.input), providerMetadata: metadata })
      }
      if (!streamed.has(message.message.id)) {
        for (let i = 0; i < message.message.content.length; i++) {
          const block = message.message.content[i]
          const id = `${message.uuid}:${i}`
          if (block.type === 'text') parts.push({ type: 'text-start', id }, { type: 'text-delta', id, delta: block.text }, { type: 'text-end', id, providerMetadata: metadata })
          if (block.type === 'thinking') parts.push({ type: 'reasoning-start', id }, { type: 'reasoning-delta', id, delta: block.thinking }, { type: 'reasoning-end', id })
        }
      }
    }
    if (message.type === 'result') {
      if (message.num_turns === 1) usage.result(message.usage)
      parts.push({ type: 'finish', finishReason: { unified: message.stop_reason === 'max_tokens' ? 'length' : 'stop', raw: message.stop_reason ?? undefined }, usage: usage.snapshot() })
    }
    return parts
  }
}
