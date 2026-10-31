// Port of PR #58782 language-model/convert-to-claude-message.ts and system-prompt.ts. MIT.
export function content(prompt) {
  const blocks = []
  const history = prompt.some((message) => message.role === 'assistant' || message.role === 'tool')
  for (const message of prompt) {
    if (message.role === 'system') continue
    if (history) blocks.push({ type: 'text', text: `<${message.role}>` })
    for (const part of message.content) {
      if (part.type === 'text') blocks.push({ type: 'text', text: part.text })
      if (part.type === 'file') blocks.push(file(part))
      if (part.type === 'tool-call') {
        const call = JSON.stringify({ id: part.toolCallId, name: part.toolName, input: part.input })
        blocks.push({ type: 'text', text: `<tool-call>\n${call}\n</tool-call>` })
      }
      if (part.type === 'tool-result') {
        const result = JSON.stringify({ id: part.toolCallId, name: part.toolName, output: part.output })
        blocks.push({ type: 'text', text: `<tool-result>\n${result}\n</tool-result>` })
      }
    }
    if (history) blocks.push({ type: 'text', text: `</${message.role}>` })
  }
  return blocks
}

export function file(part) {
  const type = part.mediaType
  let kind
  if (type === 'application/pdf') kind = 'document'
  if (['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(type)) kind = 'image'
  if (!kind) throw new Error(`Claude Code does not support ${type} attachments`)
  // LMV3 URL objects cross JSON transport as strings; binary strings are base64.
  const data = part.data
  const url = data instanceof URL ? data.href
    : typeof data === 'string' && /^https?:\/\//.test(data) ? data : undefined
  if (url) return { type: kind, source: { type: 'url', url } }
  return { type: kind, source: { type: 'base64', media_type: type, data: typeof data === 'string' ? data : Buffer.from(data).toString('base64') } }
}

export function systemPrompt(request) {
  if (request.kind === 'title') {
    return request.prompt.filter((message) => message.role === 'system').map((message) => message.content).join('\n\n')
      || 'You are a text generation helper. Return only the requested output.'
  }
  const blocks = ['<runtime_info>You are running in OpenCode through the Claude Code harness. Mention this only when asked. Tools run in the OpenCode client workspace; this process\'s directory is not that workspace.</runtime_info>']
  if (request.system.agent) blocks.push('# Agent role (from OpenCode)\n' + request.system.agent)
  if (request.system.instructions) blocks.push(request.system.instructions)
  return { type: 'preset', preset: 'claude_code', snapshot: false, append: blocks.join('\n\n') }
}
