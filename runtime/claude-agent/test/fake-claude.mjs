// Fake *native transport*, not a mock of the SDK. Protocol shapes follow the pinned upstream recordings.
// The real @anthropic-ai/claude-agent-sdk drives this process and the real MCP server.
import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import { createInterface } from 'node:readline'
import { nativeIO } from '../lib/native.mjs'

export const initReply = {
  commands: [], agents: [], output_style: 'default', available_output_styles: ['default'],
  claude_code_version: '2.1.301',
  account: { email: 'fixture@example.invalid', organization: 'Fixture', subscriptionType: 'Pro', tokenSource: 'oauth', apiProvider: 'firstParty' },
  models: [{ value: 'default', displayName: 'Default', description: 'Default' }, {
    value: 'sonnet', displayName: 'Sonnet', description: 'Fixture model', resolvedModel: 'claude-sonnet-fixture',
    supportsEffort: true, supportedEffortLevels: ['low', 'high', 'max'], supportsAdaptiveThinking: true,
  }],
}

const counts = (output = 7) => ({ input_tokens: 10, cache_read_input_tokens: 20, cache_creation_input_tokens: 5, output_tokens: output })

class FakeProcess extends EventEmitter {
  stdin = new PassThrough()
  stdout = new PassThrough()
  killed = false
  exitCode = null
  signalCode = null
  kill(signal) { this.killed = true; this.exit(null, signal); return true }
  exit(code = 0, signal = null) {
    if (this.exitCode !== null || this.signalCode !== null) return
    this.exitCode = code
    this.signalCode = signal
    this.stdout.end()
    this.emit('exit', code, signal)
  }
}

export function fakeClaude({ directory, plans = [], env = {}, limit } = {}) {
  const runs = []
  const transcripts = new Map()
  const forks = []
  const removed = []
  const errors = []
  const io = nativeIO({
    env: { PATH: process.env.PATH, CLAUDE_CONFIG_DIR: directory, OPENPROXY_CLAUDE_PATH: 'fake-claude', ...env }, limit,
    spawnProcess(options) {
      const process = new FakeProcess()
      const plan = plans[runs.length] ?? {}
      const id = options.args.find((arg) => arg.startsWith('--session-id=') || arg.startsWith('--resume='))?.split('=')[1] ?? crypto.randomUUID()
      const closed = Promise.withResolvers()
      const run = { ...options, id, received: [], sent: [], toolResults: [], closed: closed.promise, process, plan }
      runs.push(run)
      let initialize
      let actionIndex = 0
      let active
      let nativeRequest = 0
      const requests = new Map()
      function remember(message) {
        if (options.args.includes('--no-session-persistence') || !message.uuid || !['assistant', 'user'].includes(message.type)) return
        const saved = transcripts.get(id) ?? []
        if (!saved.some((item) => item.uuid === message.uuid)) saved.push(structuredClone(message))
        transcripts.set(id, saved)
      }
      function send(message) {
        if (process.exitCode !== null || process.signalCode !== null) return
        if (['assistant', 'user', 'stream_event', 'result', 'system'].includes(message.type)) message.session_id = id
        remember(message)
        run.sent.push(message)
        process.stdout.write(JSON.stringify(message) + '\n')
      }
      function response(request, value = {}) {
        send({ type: 'control_response', response: { subtype: 'success', request_id: request.request_id, response: value } })
      }
      function request(body) {
        const request_id = 'native_' + (++nativeRequest)
        const wait = Promise.withResolvers()
        requests.set(request_id, wait)
        send({ type: 'control_request', request_id, request: body })
        return wait.promise
      }
      function event(event) { send({ type: 'stream_event', uuid: crypto.randomUUID(), parent_tool_use_id: null, event }) }
      function assistant(content, apiID = 'msg_' + crypto.randomUUID(), error) {
        const message = { type: 'assistant', uuid: crypto.randomUUID(), parent_tool_use_id: null,
          message: { id: apiID, model: 'claude-sonnet-fixture', type: 'message', role: 'assistant', content, usage: counts() } }
        if (error) message.error = error
        send(message)
        return message
      }
      function result(text, num_turns = 1, error) {
        send({ type: 'result', subtype: 'success', uuid: crypto.randomUUID(), result: text,
          is_error: !!error, api_error_status: error ? 429 : null, stop_reason: plan.stopReason ?? 'end_turn',
          num_turns, usage: counts(), modelUsage: {}, permission_denials: [], total_cost_usd: 0, duration_ms: 1, duration_api_ms: 1 })
        active = undefined
      }
      function textReply(text = 'ANSWER', numTurns = 1) {
        const apiID = 'msg_' + crypto.randomUUID()
        event({ type: 'message_start', message: { id: apiID, model: 'claude-sonnet-fixture', usage: counts(0) } })
        event({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } })
        event({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } })
        assistant([{ type: 'text', text }], apiID)
        event({ type: 'content_block_stop', index: 0 })
        event({ type: 'message_delta', usage: counts(), delta: { stop_reason: 'end_turn' } })
        event({ type: 'message_stop' })
        result(text, numTurns)
      }
      async function hook(kind, call, output) {
        const callback_id = initialize.hooks[kind][0].hookCallbackIds[0]
        return request({ subtype: 'hook_callback', callback_id, tool_use_id: call.id,
          input: { hook_event_name: kind, tool_name: call.name, tool_use_id: call.id, tool_input: call.input,
            tool_response: output, session_id: id, cwd: options.cwd, transcript_path: 'fixture-only' } })
      }
      async function tools(action, turn) {
        const calls = action.calls ?? [{ id: 'exact-tool-a', name: 'mcp__opencode__lookup', input: { key: 'a' } }, { id: 'exact-tool-b', name: 'mcp__opencode__lookup', input: { key: 'b' } }]
        const apiID = 'msg_tools_' + crypto.randomUUID()
        event({ type: 'message_start', message: { id: apiID, model: 'claude-sonnet-fixture', usage: counts(0) } })
        assistant(calls.map((call) => ({ type: 'tool_use', ...call })), apiID)
        event({ type: 'message_stop' })
        for (const call of calls) {
          await hook('PreToolUse', call)
          if (active !== turn) return
          const envelope = await request({ subtype: 'mcp_message', server_name: 'opencode',
            message: { jsonrpc: '2.0', id: ++nativeRequest, method: 'tools/call',
              params: { name: call.name.slice('mcp__opencode__'.length), arguments: call.input, _meta: { 'claudecode/toolUseId': call.id } } } })
          if (active !== turn) return
          const output = envelope.mcp_response?.result
          if (!output) throw new Error('The real SDK did not return an MCP result')
          const after = await hook('PostToolUse', call, output)
          run.toolResults.push({ id: call.id, output, replacement: after.hookSpecificOutput?.updatedMCPToolOutput })
          send({ type: 'user', uuid: crypto.randomUUID(), parent_tool_use_id: null,
            message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: call.id, content: output.content, is_error: output.isError }] } })
        }
        if (active === turn) textReply(action.text ?? 'TOOLS-OK', 2)
      }
      async function answer(message) {
        run.received.push(message)
        if (message.type === 'control_response') {
          const wait = requests.get(message.response.request_id)
          requests.delete(message.response.request_id)
          if (message.response.subtype === 'error') wait?.reject(new Error(message.response.error))
          else wait?.resolve(message.response.response ?? {})
          return
        }
        const type = message.request?.subtype
        if (type === 'initialize') {
          initialize = message.request
          if (plan.startupGate) await plan.startupGate
          response(message, { ...initReply, ...plan.init })
        } else if (type === 'get_context_usage') response(message, { rawMaxTokens: 1_000_000, maxTokens: 997_000, totalTokens: 0 })
        else if (type === 'interrupt') {
          response(message)
          const turn = active
          if (!turn) return
          run.interruptReceived?.resolve()
          if (plan.interruptGate) await plan.interruptGate
          if (active !== turn) return
          if (turn.type === 'partial') {
            assistant([{ type: 'text', text: 'NATIVE-PARTIAL' }], turn.apiID)
            event({ type: 'content_block_stop', index: 0 })
            event({ type: 'message_stop' })
          }
          result('', 1)
        } else if (message.type === 'control_request') response(message)
        else if (message.type === 'user') {
          remember(message)
          // A priority=next message steers an already-running native turn; it is not a second inference.
          if (message.priority === 'next' && active) return
          const action = plan.actions?.[actionIndex++] ?? { type: 'text', text: 'ANSWER' }
          active = { ...action }
          if (!run.sent.some((item) => item.type === 'system' && item.subtype === 'init')) send({
            type: 'system', subtype: 'init', uuid: crypto.randomUUID(),
            mcp_servers: initialize.sdkMcpServers?.map((name) => ({ name, status: plan.mcpConnected === false ? 'failed' : 'connected' })) ?? [],
          })
          if (action.type === 'partial') {
            active.apiID = 'partial_' + crypto.randomUUID()
            event({ type: 'message_start', message: { id: active.apiID, model: 'claude-sonnet-fixture', usage: counts(0) } })
            event({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } })
            event({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'CLIENT-PARTIAL' } })
          } else if (action.type === 'tools') {
            void tools(action, active).catch((error) => errors.push(error))
          } else if (action.type === 'error') {
            assistant([{ type: 'text', text: 'Diagnostic, not a reply' }], undefined, action.code ?? 'billing_error')
            result(action.text ?? 'Plan usage limit reached', 1, true)
          } else if (action.type === 'unknown-tool') {
            assistant([{ type: 'tool_use', id: 'unoffered-id', name: 'glob', input: {} }])
            event({ type: 'message_stop' })
            textReply('NATIVE-NO-SUCH-TOOL', 2)
          } else textReply(action.text ?? 'ANSWER')
        }
      }
      createInterface({ input: process.stdin }).on('line', (line) => {
        void answer(JSON.parse(line)).catch((error) => errors.push(error))
      })
      process.stdin.on('end', () => process.exit())
      process.once('exit', () => {
        for (const wait of requests.values()) wait.resolve({})
        closed.resolve()
      })
      return process
    },
  })
  io.exists = async (id) => transcripts.has(id)
  io.fork = async (id, boundary) => {
    const saved = transcripts.get(id) ?? []
    const end = saved.findIndex((message) => message.uuid === boundary)
    if (end < 0) return
    const sessionID = crypto.randomUUID()
    const messages = new Map()
    const copied = saved.slice(0, end + 1).map((message) => {
      const uuid = crypto.randomUUID()
      messages.set(message.uuid, uuid)
      return { ...structuredClone(message), uuid, session_id: sessionID }
    })
    transcripts.set(sessionID, copied)
    forks.push({ source: id, boundary, sessionID })
    return { sessionID, messages }
  }
  io.remove = async (id) => { removed.push(id); transcripts.delete(id) }
  return { io, runs, transcripts, forks, removed, errors }
}

export function memoryStorage() {
  const data = new Map()
  return { data, get: async (key) => data.get(key), set: async (key, value) => { data.set(key, structuredClone(value)) } }
}

export function fakeClock() {
  let now = 0
  const timers = new Set()
  return {
    setTimeout(run, ms) { const timer = { run, at: now + ms }; timers.add(timer); return timer },
    clearTimeout(timer) { timers.delete(timer) },
    async advance(ms) {
      now += ms
      for (const timer of [...timers]) if (timer.at <= now) { timers.delete(timer); await timer.run() }
    },
    get pending() { return timers.size },
  }
}
