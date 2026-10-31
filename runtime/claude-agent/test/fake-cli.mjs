// Executable no-prompt native fixture for the real SDK + node bridge subprocess smoke.
import { createInterface } from 'node:readline'
import { initReply } from './fake-claude.mjs'

createInterface({ input: process.stdin }).on('line', (line) => {
  const message = JSON.parse(line)
  if (message.type === 'user') {
    process.stderr.write('Probe unexpectedly sent a user prompt\n')
    process.exitCode = 66
    return
  }
  if (message.type !== 'control_request') return
  const subtype = message.request.subtype
  const response = subtype === 'initialize' ? initReply
    : subtype === 'get_context_usage' ? { rawMaxTokens: 1_000_000, maxTokens: 1_000_000, totalTokens: 0 } : {}
  process.stdout.write(JSON.stringify({ type: 'control_response', response: { subtype: 'success', request_id: message.request_id, response } }) + '\n')
})
