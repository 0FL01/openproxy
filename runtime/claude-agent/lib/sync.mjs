// Port of anomalyco/opencode PR #58782, f29e2813, sync/*.ts. MIT: ../LICENSE.upstream.
import { createHash } from 'node:crypto'
import { content } from './messages.mjs'

export function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`
  if (value !== null && typeof value === 'object') {
    const fields = Object.keys(value).sort().filter((key) => value[key] !== undefined)
    return `{${fields.map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`
  }
  return JSON.stringify(value) ?? 'null'
}

const hash = (value) => createHash('sha256').update(canonical(value)).digest('hex')

function visible(message) {
  const parts = []
  let text = ''
  for (const part of message.content) {
    if (part.type === 'reasoning') continue
    if (part.type === 'text') {
      text += part.text
      continue
    }
    if (text) parts.push({ type: 'text', text })
    text = ''
    if (part.type === 'file') {
      const data = part.data instanceof URL ? part.data.href
        : typeof part.data === 'string' ? part.data : Buffer.from(part.data).toString('base64')
      parts.push({ type: part.type, mediaType: part.mediaType, data })
    }
    if (part.type === 'tool-call') {
      parts.push({ type: part.type, id: part.toolCallId, name: part.toolName, input: part.input })
    }
    if (part.type === 'tool-result') {
      const output = { ...part.output }
      delete output.providerOptions
      if (output.type === 'content') {
        output.value = output.value.map((part) => {
          const item = { ...part }
          delete item.providerOptions
          return item
        })
      }
      parts.push({ type: part.type, id: part.toolCallId, name: part.toolName, output })
    }
  }
  if (text) parts.push({ type: 'text', text })
  return parts
}

function isToolMedia(message) {
  return message.role === 'user' && message.content.length > 1
    && message.content[0]?.type === 'text'
    && message.content[0].text === 'Attached media from tool result:'
    && message.content.slice(1).every((part) => part.type === 'file')
}

export function groups(prompt, prefix = []) {
  const result = []
  let i = 0
  while (i < prompt.length) {
    const message = prompt[i]
    const start = i++
    if (message.role === 'system') continue
    const parts = visible(message)
    if (!parts.length) continue
    const members = [hash(parts)]
    if (message.role === 'user') {
      while (i < prompt.length) {
        const checkpoint = prefix[result.length]
        if (checkpoint && hash({ role: message.role, members: members.toSorted() }) === checkpoint.hash) break
        const next = prompt[i]
        if (next.role !== 'user') break
        members.push(hash(visible(next)))
        i++
      }
      members.sort()
    }
    if (message.role === 'tool') {
      const next = prompt[i]
      if (next && isToolMedia(next)) {
        members.push(hash(visible(next)))
        i++
      }
    }
    result.push({ hash: hash({ role: message.role, members }), role: message.role, start, end: i })
  }
  return result
}

export function decide(prompt, record) {
  const saved = record?.history ?? []
  const current = groups(prompt, record?.interrupted ? saved : undefined)
  if (!saved.length) return { kind: 'fresh', start: 0 }
  let matched = 0
  while (matched < saved.length && matched < current.length && saved[matched].hash === current[matched].hash) matched++
  if (!matched) return { kind: 'fresh', start: 0 }
  if (matched < saved.length) return { kind: 'changed', start: 0 }
  // The drained native transcript, not a client's partial copy, owns an interrupted reply.
  while (record?.interrupted && current[matched]?.role === 'assistant') {
    const reply = prompt[current[matched].start]
    if (reply.role !== 'assistant' || reply.content.some((part) => part.type === 'tool-call')) break
    matched++
  }
  const start = current[matched]?.start ?? prompt.length
  if (record?.interrupted && start === prompt.length) return { kind: 'changed', start: 0 }
  return { kind: current.slice(matched).some((group) => group.role !== 'user') ? 'catch-up' : 'continue', start }
}

export function boundary(prompt, record) {
  const current = groups(prompt)
  const history = record?.history ?? []
  const pending = new Set()
  let found
  for (let i = 0; i < current.length; i++) {
    const group = current[i]
    if (i === current.length - 1 && group.role === 'user') break
    const checkpoint = history[i]
    if (!checkpoint || checkpoint.hash !== group.hash) break
    for (const message of prompt.slice(group.start, group.end)) {
      if (message.role === 'system') continue
      for (const part of message.content) {
        if (part.type === 'tool-call') pending.add(part.toolCallId)
        if (part.type === 'tool-result') pending.delete(part.toolCallId)
      }
    }
    if (checkpoint.claudeMessageID && pending.size === 0) {
      found = { messageID: checkpoint.claudeMessageID, start: group.end, history: history.slice(0, i + 1) }
    }
  }
  return found
}

export function catchUp(prompt, start = 0) {
  const missed = prompt.slice(start)
  const end = groups(missed).findLast((group) => group.role !== 'user')?.end ?? 0
  const blocks = []
  if (end > 0) {
    blocks.push({ type: 'text', text: '<conversation-checkpoint>\nThe following is conversation history from OpenCode that you have not seen. Treat it as prior conversation, not a new request. Continue with the user messages after this checkpoint.' })
    blocks.push(...content(missed.slice(0, end)))
    blocks.push({ type: 'text', text: '</conversation-checkpoint>' })
  }
  blocks.push(...content(missed.slice(end)))
  return blocks
}

export function checkpoints({ prompt, previous, userID, reply, messageID, toolMessages }) {
  const history = []
  const current = groups(prompt)
  for (let i = 0; i < current.length; i++) {
    const group = current[i]
    const before = previous?.history?.[i]
    let claudeMessageID = before?.hash === group.hash ? before.claudeMessageID : undefined
    if (group.role === 'tool') {
      for (const message of prompt.slice(group.start, group.end)) {
        if (message.role !== 'tool') continue
        for (const part of message.content) {
          if (part.type === 'tool-result' && toolMessages.has(part.toolCallId)) {
            claudeMessageID = toolMessages.get(part.toolCallId)
          }
        }
      }
    }
    if (i === current.length - 1 && userID) claudeMessageID = userID
    history.push({ hash: group.hash, claudeMessageID })
  }
  if (reply.length) history.push({ hash: groups([{ role: 'assistant', content: reply }])[0].hash, claudeMessageID: messageID })
  return history
}
