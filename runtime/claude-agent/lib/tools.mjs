// Port of PR #58782 tools/{hooks,names,pending,result,server}.ts. MIT: ../LICENSE.upstream.
import { createHash } from 'node:crypto'
import { content } from './messages.mjs'
import { groups } from './sync.mjs'

export const PREFIX = 'mcp__opencode__'
export function shortName(name) {
  const limit = 64 - PREFIX.length
  if (name.length <= limit && /^[a-zA-Z0-9_-]+$/.test(name)) return name
  const hash = createHash('sha256').update(name).digest('hex').slice(0, 10)
  return name.replace(/[^a-zA-Z0-9_-]/g, '_').slice(0, limit - 11) + '_' + hash
}
export const originalName = (name, tools) => tools.find((tool) => PREFIX + shortName(tool.name) === name)?.name

function description(tool) {
  const text = tool.description ?? ''
  if (tool.name !== 'subagent' || text.length <= 2048) return text.slice(0, 2048)
  const agents = text.indexOf('Available subagents:')
  return agents === -1 ? text.slice(0, 2048) : (text.slice(agents) + '\n\n' + text.slice(0, agents)).slice(0, 2048)
}

export function pendingTools() {
  const calls = new Map()
  const replacements = new Map()
  let next
  function expect(id, name) {
    const saved = calls.get(id)
    if (!saved && calls.size >= 256) throw new Error('Too many pending Claude tool calls')
    const call = saved ?? { name, result: Promise.withResolvers() }
    if (!saved) calls.set(id, call)
    if (call.name !== name) throw new Error(`Mismatched Claude tool call ${id}`)
    return call
  }
  return {
    expect,
    before(id, name) {
      expect(id, name)
      if (next && next !== id) throw new Error('Claude attempted parallel OpenCode tools')
      next = id
    },
    async call(name) {
      const id = next
      next = undefined
      if (!id) throw new Error('Claude invoked an OpenCode tool without its PreToolUse ID')
      const call = expect(id, name)
      const result = await call.result.promise
      if (result.replacement) replacements.set(id, result.replacement)
      calls.delete(id)
      return result.mcp
    },
    resolve(id, result) {
      const call = calls.get(id)
      if (!call) throw new Error(`No waiting Claude tool call ${id}`)
      call.result.resolve(result)
    },
    after(id) {
      const result = replacements.get(id)
      replacements.delete(id)
      return result
    },
    // Only after interrupt AND drain; releasing an MCP handler first may trigger inference.
    clear() {
      for (const call of calls.values()) call.result.resolve({ mcp: { isError: true, content: [{ type: 'text', text: 'Cancelled by user.' }] } })
      calls.clear()
      replacements.clear()
      next = undefined
    },
  }
}

export function toolHooks(pending) {
  return {
    PreToolUse: [{ hooks: [async (input) => {
      if (input.hook_event_name === 'PreToolUse') pending.before(input.tool_use_id, input.tool_name)
      return {}
    }] }],
    PostToolUse: [{ hooks: [async (input) => {
      if (input.hook_event_name !== 'PostToolUse') return {}
      const replacement = pending.after(input.tool_use_id)
      return replacement ? { hookSpecificOutput: { hookEventName: 'PostToolUse', updatedMCPToolOutput: replacement } } : {}
    }] }],
  }
}

export async function toolServer(tools, pending) {
  const { Server } = await import('@modelcontextprotocol/server')
  const server = new Server({ name: 'opencode', version: '1.0.0' }, { capabilities: { tools: {} } })
  const definitions = tools.map((tool) => ({
    name: shortName(tool.name), description: description(tool), inputSchema: tool.inputSchema,
    annotations: { readOnlyHint: false },
    _meta: { 'anthropic/alwaysLoad': true, 'anthropic/maxResultSizeChars': 500_000 },
  }))
  if (new Set(definitions.map((tool) => tool.name)).size !== definitions.length) throw new Error('Conflicting OpenCode tool names')
  server.setRequestHandler('tools/list', async () => ({ tools: definitions }))
  server.setRequestHandler('tools/call', async (request) => {
    const name = PREFIX + request.params.name
    if (!originalName(name, tools)) throw new Error('Claude Code called an unregistered OpenCode tool')
    return pending.call(name)
  })
  return { type: 'sdk', name: 'opencode', instance: server }
}

export function toolResult(output, files = [], failed = false) {
  const parts = []
  switch (output.type) {
    case 'text': case 'error-text':
      parts.push({ type: 'text', text: output.value }); break
    case 'json': case 'error-json':
      parts.push({ type: 'text', text: JSON.stringify(output.value) }); break
    case 'execution-denied':
      parts.push({ type: 'text', text: output.reason ?? 'Tool execution was denied by the user.' }); break
    case 'content':
      for (const part of output.value) {
        if (part.type === 'text') parts.push({ type: 'text', text: part.text })
        else if (part.type === 'image-data' || part.type === 'file-data') parts.push({ type: 'file', mediaType: part.mediaType, data: part.data })
        else if (part.type === 'image-url') parts.push({ type: 'file', mediaType: 'image/png', data: new URL(part.url) })
        else throw new Error(`Claude Code does not support ${part.type} tool output`)
      }
      break
    default: throw new Error(`Claude Code does not support ${output.type} tool output`)
  }
  parts.push(...files)
  const blocks = content([{ role: 'user', content: parts }])
  const mcp = { content: [], isError: failed || ['error-text', 'error-json', 'execution-denied'].includes(output.type) }
  let replace = false
  for (const block of blocks) {
    if (block.type === 'text') mcp.content.push({ type: 'text', text: block.text })
    else if (block.type === 'image' && block.source.type === 'base64') mcp.content.push({ type: 'image', mimeType: block.source.media_type, data: block.source.data })
    else {
      replace = true
      mcp.content.push({ type: 'text', text: 'Attached document or image follows.' })
    }
  }
  return { mcp, replacement: replace ? blocks : undefined }
}

export function collectResults(prompt, start) {
  const results = new Map()
  let end = start
  let aborted = false
  for (const group of groups(prompt.slice(start))) {
    if (group.role !== 'tool') break
    const messages = prompt.slice(start + group.start, start + group.end)
    const tools = []
    const files = []
    for (const message of messages) {
      if (message.role === 'tool') tools.push(...message.content.filter((part) => part.type === 'tool-result'))
      if (message.role === 'user') files.push(...message.content.filter((part) => part.type === 'file'))
    }
    for (let i = 0; i < tools.length; i++) {
      const tool = tools[i]
      const flags = { ...tool.providerMetadata?.['claude-agent'], ...tool.providerOptions?.['claude-agent'] }
      if (flags.toolAborted === true) aborted = true
      results.set(tool.toolCallId, toolResult(tool.output, i === tools.length - 1 ? files : [], flags.toolError === true))
    }
    end = start + group.end
  }
  return { results, end, aborted }
}
