import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { PassThrough, Writable } from 'node:stream'
import { once } from 'node:events'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { probe, MAX_PROCESSES, MAX_FRAME_BYTES } from '../lib/native.mjs'
import { sessionRegistry, IDLE_MS } from '../lib/sessions.mjs'
import { recordStore, MAX_HISTORY } from '../lib/records.mjs'
import { usageTracker } from '../lib/stream.mjs'
import { collectResults, toolResult } from '../lib/tools.mjs'
import { validateRequest, ProtocolWriter, commands } from '../lib/protocol.mjs'
import { serve } from '../bridge.mjs'
import { fakeClaude, fakeClock, initReply, memoryStorage } from './fake-claude.mjs'

const user = (text) => ({ role: 'user', content: [{ type: 'text', text }] })
const assistant = (text) => ({ role: 'assistant', content: [{ type: 'text', text }] })
const first = [{ role: 'system', content: 'TITLE-SYSTEM' }, user('Remember MARKER')]
const follow = (text = 'Next') => [...first, assistant('ANSWER'), user(text)]
const tools = [{ type: 'function', name: 'lookup', description: 'Fixture lookup', inputSchema: { type: 'object', properties: { key: { type: 'string' } } } }]
const request = (prompt = first, extra = {}) => ({ model: 'sonnet', kind: 'primary', sessionID: 'session-a', system: { agent: 'ROLE', instructions: 'INSTRUCTIONS' }, prompt, tools: [], ...extra })
const text = (parts) => parts.filter((part) => part.type === 'text-delta').map((part) => part.delta).join('')
const tick = () => new Promise((resolve) => setImmediate(resolve))
async function until(check) { for (let i = 0; i < 300; i++) { if (check()) return; await tick() } throw new Error('Test condition did not settle') }

async function setup(t, options = {}) {
  await mkdir('/tmp/opencode', { recursive: true })
  const directory = await mkdtemp('/tmp/opencode/claude-runtime-test-')
  const fake = fakeClaude({ directory, ...options })
  const storage = options.storage ?? memoryStorage()
  const registry = sessionRegistry({ io: fake.io, storage, clock: options.clock })
  t.after(async () => {
    await registry.close()
    await rm(directory, { recursive: true, force: true })
    assert.deepEqual(fake.errors, [])
  })
  async function run(input = request(), signal, id) {
    const parts = []
    await registry.run(input, (part) => { parts.push(part) }, signal, id)
    return parts
  }
  return { ...fake, directory, registry, storage, run }
}

function appendTools(prompt, parts, outputs = []) {
  const calls = parts.filter((part) => part.type === 'tool-call')
  prompt.push({ role: 'assistant', content: calls.map((part) => ({ type: 'tool-call', toolCallId: part.toolCallId, toolName: part.toolName, input: JSON.parse(part.input) })) })
  prompt.push({ role: 'tool', content: calls.map((part, i) => ({ type: 'tool-result', toolCallId: part.toolCallId, toolName: part.toolName, output: outputs[i] ?? { type: 'text', value: 'result-' + i } })) })
}

test('native probe sends zero prompts, uses the explicit binary, and whitelists safe account/model fields', async (t) => {
  const runtime = await setup(t, { plans: [{ init: { account: { ...initReply.account, accessToken: 'never-export-this' } } }] })
  const result = await probe(runtime.io)
  assert.equal(result.version, '2.1.301')
  assert.equal(result.account.subscriptionType, 'Pro')
  assert.equal(result.account.accessToken, undefined)
  assert.equal(result.models[1].contextWindow, 1_000_000)
  assert.equal(runtime.runs.length, 1)
  assert.equal(runtime.runs[0].command, 'fake-claude')
  assert.equal(runtime.runs[0].received.filter((message) => message.type === 'user').length, 0)
  assert.equal(runtime.io.liveCount, 0)
})

test('persistent chat reuses native context; isolation and the native system preset stay explicit', async (t) => {
  const runtime = await setup(t, { env: { ANTHROPIC_API_KEY: 'fixture-only', ANTHROPIC_BASE_URL: 'https://invalid.test',
    ANTHROPIC_AWS_AUTH: 'fixture-only', ANTHROPIC_GOOGLE_CLOUD_AUTH: 'fixture-only', ANTHROPIC_UNIX_SOCKET: '/fixture-only',
    CLAUDE_CODE_USE_ANTHROPIC_AWS: '1', CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD: '1', CLAUDE_CODE_USE_MANTLE: '1', CLAUDE_CODE_USE_GATEWAY: '1',
    CLAUDE_CODE_USE_BEDROCK: '1', CLAUDE_CODE_EFFORT_LEVEL: 'max', CLAUDE_CODE_SIMPLE: '1', MCP_TOOL_TIMEOUT: '100', CLAUDE_CODE_MAX_OUTPUT_TOKENS: '1' } })
  const initial = await runtime.run()
  assert.equal(text(initial), 'ANSWER')
  assert.equal(initial.find((part) => part.type === 'finish').providerMetadata['claude-agent'].inventory.version, '2.1.301')
  const parts = await runtime.run(request(follow()))
  assert.equal(text(parts), 'ANSWER')
  assert.equal(runtime.runs.length, 1)
  const run = runtime.runs[0]
  assert(!run.args.includes('--no-session-persistence'))
  for (const name of ['ANTHROPIC_API_KEY', 'ANTHROPIC_BASE_URL', 'CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_EFFORT_LEVEL', 'CLAUDE_CODE_SIMPLE', 'MCP_TOOL_TIMEOUT', 'CLAUDE_CODE_MAX_OUTPUT_TOKENS']) assert.equal(run.env[name], undefined)
  for (const name of ['ANTHROPIC_AWS_AUTH', 'ANTHROPIC_GOOGLE_CLOUD_AUTH', 'ANTHROPIC_UNIX_SOCKET', 'CLAUDE_CODE_USE_ANTHROPIC_AWS', 'CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD', 'CLAUDE_CODE_USE_MANTLE', 'CLAUDE_CODE_USE_GATEWAY']) assert.equal(run.env[name], undefined)
  for (const name of ['CLAUDE_CODE_DISABLE_CLAUDE_MDS', 'CLAUDE_CODE_DISABLE_AUTO_MEMORY', 'DISABLE_AUTO_COMPACT', 'CLAUDE_CODE_DISABLE_ADVISOR_TOOL']) assert.equal(run.env[name], '1')
  assert.equal(run.env.ENABLE_TOOL_SEARCH, 'false')
  const init = run.received.find((message) => message.request?.subtype === 'initialize').request
  assert.equal(init.systemPrompt, undefined)
  assert.match(init.appendSystemPrompt, /ROLE/)
  assert.match(init.appendSystemPrompt, /INSTRUCTIONS/)
  assert(!JSON.stringify(init).includes('TITLE-SYSTEM'))
  const users = run.received.filter((message) => message.type === 'user')
  assert.deepEqual(users[1].message.content, [{ type: 'text', text: 'Next' }])
  assert.equal(users[0].client_composed, true)
  const usage = parts.find((part) => part.type === 'finish').usage
  assert.equal(usage.inputTokens.total, 35)
  assert.equal(usage.outputTokens.total, 7)
})

test('tools park the real SDK, preserve exact IDs, execute sequentially, and outlive five minutes', async (t) => {
  const clock = fakeClock()
  const runtime = await setup(t, { clock, plans: [{ actions: [{ type: 'tools' }] }] })
  const prompt = structuredClone(first)
  const one = await runtime.run(request(prompt, { tools }))
  assert.deepEqual(one.filter((part) => part.type === 'tool-call').map((part) => part.toolCallId), ['exact-tool-a', 'exact-tool-b'])
  assert.equal(one.at(-1).finishReason.unified, 'tool-calls')
  assert.equal(clock.pending, 0)
  await clock.advance(IDLE_MS * 2)
  assert.equal(runtime.io.liveCount, 1)
  assert.equal(runtime.runs[0].toolResults.length, 0)
  appendTools(prompt, one, [{ type: 'text', value: 'RESULT-A' }, { type: 'error-text', value: 'RESULT-B-ERROR' }])
  const two = await runtime.run(request(prompt, { tools }))
  assert.equal(text(two), 'TOOLS-OK')
  assert.deepEqual(runtime.runs[0].toolResults.map(({ id, output }) => [id, output.content[0].text, output.isError]), [
    ['exact-tool-a', 'RESULT-A', false], ['exact-tool-b', 'RESULT-B-ERROR', true],
  ])
  assert.equal(runtime.runs.length, 1)
  assert.equal(clock.pending, 1)
})

test('only explicit idle arms cleanup of abandoned tools, and cleanup drains before releasing handlers', async (t) => {
  const clock = fakeClock()
  const gate = Promise.withResolvers()
  const runtime = await setup(t, { clock, plans: [{ actions: [{ type: 'tools' }], interruptGate: gate.promise }] })
  await runtime.run(request(first, { tools }))
  runtime.registry.idle('session-a')
  assert.equal(clock.pending, 1)
  const cleanup = clock.advance(IDLE_MS)
  await until(() => runtime.runs[0].received.some((message) => message.request?.subtype === 'interrupt'))
  assert.equal(runtime.runs[0].toolResults.length, 0)
  assert.equal(runtime.io.liveCount, 1)
  gate.resolve()
  await cleanup
  assert.equal(runtime.io.liveCount, 0)
})

test('cancellation drains beyond its receipt and serializes the next turn without replaying partial text', async (t) => {
  const gate = Promise.withResolvers()
  const runtime = await setup(t, { plans: [{ actions: [{ type: 'partial' }, { type: 'text', text: 'CONTINUED' }], interruptGate: gate.promise }] })
  t.after(() => gate.resolve())
  const controller = new AbortController()
  const parts = []
  const running = runtime.registry.run(request(), (part) => { parts.push(part) }, controller.signal, 'cancel-me')
  await until(() => parts.some((part) => part.type === 'text-delta'))
  controller.abort()
  const cancelled = assert.rejects(running, { code: 'CANCELLED' })
  await until(() => runtime.runs[0].received.some((message) => message.request?.subtype === 'interrupt'))
  const next = runtime.run(request([...first, assistant('A DIFFERENT CLIENT PARTIAL'), user('Continue')]))
  await tick()
  assert.equal(runtime.runs[0].received.filter((message) => message.type === 'user').length, 1)
  gate.resolve()
  await cancelled
  assert.equal(text(await next), 'CONTINUED')
  assert.equal(runtime.runs.length, 1)
  const last = runtime.runs[0].received.filter((message) => message.type === 'user').at(-1)
  assert.deepEqual(last.message.content, [{ type: 'text', text: 'Continue' }])
})

test('native resume survives registry restart; edits/undo fork and remap safe checkpoints', async (t) => {
  const runtime = await setup(t)
  await runtime.run()
  const saved = structuredClone(await runtime.storage.get('session-a'))
  await runtime.registry.close()
  const next = sessionRegistry({ io: runtime.io, storage: runtime.storage })
  t.after(() => next.close())
  const run = async (input) => { const parts = []; await next.run(input, (part) => parts.push(part)); return parts }
  await run(request(follow()))
  assert(runtime.runs[1].args.includes('--resume=' + saved.claudeSessionID))
  const before = structuredClone(await runtime.storage.get('session-a'))
  await run(request(follow('Replacement after undo')))
  assert.equal(runtime.forks.length, 1)
  assert.equal(runtime.forks[0].boundary, before.history[1].claudeMessageID)
  const branched = await runtime.storage.get('session-a')
  assert.notEqual(branched.history[1].claudeMessageID, before.history[1].claudeMessageID)
  await run(request(follow('Another replacement')))
  assert.equal(runtime.forks[1].source, branched.claudeSessionID)
  assert.equal(runtime.forks[1].boundary, branched.history[1].claudeMessageID)
})

test('missing transcripts rebuild from client history and unsupported attachments fail before a fork', async (t) => {
  const runtime = await setup(t)
  await runtime.run()
  const edited = follow()
  edited[2] = assistant('EDITED')
  edited.push({ role: 'user', content: [{ type: 'file', mediaType: 'image/svg+xml', data: 'PHN2Zy8+' }] })
  await assert.rejects(runtime.run(request(edited)), /does not support image\/svg\+xml/)
  assert.equal(runtime.forks.length, 0)
  runtime.transcripts.clear()
  const parts = await runtime.run(request(follow()))
  assert.equal(text(parts), 'ANSWER')
  assert(!runtime.runs.at(-1).args.some((arg) => arg.startsWith('--resume=')))
  const rebuilt = runtime.runs.at(-1).received.find((message) => message.type === 'user').message.content
  assert.match(JSON.stringify(rebuilt), /conversation-checkpoint/)
  assert.match(JSON.stringify(rebuilt), /Remember MARKER/)
})

test('title/generate/compaction are disposable and do not alter the primary checkpoint record', async (t) => {
  const runtime = await setup(t)
  await runtime.run(request(first, { tools }))
  const before = structuredClone(await runtime.storage.get('session-a'))
  for (const kind of ['title', 'generate', 'compaction']) {
    assert.equal(text(await runtime.run(request(follow('Side task'), { kind, sourceID: 'session-a', tools }))), 'ANSWER')
    assert.deepEqual(await runtime.storage.get('session-a'), before)
  }
  assert.equal(runtime.forks.length, 2)
  assert.deepEqual(new Set(runtime.removed), new Set(runtime.forks.map((fork) => fork.sessionID)))
  const title = runtime.runs[1]
  const titleInit = title.received.find((message) => message.request?.subtype === 'initialize').request
  assert.deepEqual(titleInit.systemPrompt, ['TITLE-SYSTEM'])
  assert.deepEqual(titleInit.sdkMcpServers ?? [], [])
  for (const run of runtime.runs.slice(1)) assert(run.args.includes('--no-session-persistence'))
  const branchInit = runtime.runs[2].received.find((message) => message.request?.subtype === 'initialize').request
  const chatInit = runtime.runs[0].received.find((message) => message.request?.subtype === 'initialize').request
  assert.equal(branchInit.appendSystemPrompt, chatInit.appendSystemPrompt)
  assert.deepEqual(branchInit.sdkMcpServers, chatInit.sdkMcpServers)
  assert.equal(text(await runtime.run(request(follow()))), 'ANSWER')
})

test('model/effort use live controls; explicit output limits restart/resume and clearing restores native defaults', async (t) => {
  const runtime = await setup(t)
  const prompt = structuredClone(first)
  const settings = [
    { effort: 'low', maxOutputTokens: 2048 }, { model: 'opus', effort: 'high', maxOutputTokens: 2048 },
    { model: 'opus', maxOutputTokens: 2048 }, { model: 'opus', maxOutputTokens: 4096 }, { model: 'opus' },
  ]
  for (const setting of settings) {
    await runtime.run(request(prompt, setting))
    prompt.push(assistant('ANSWER'), user('Next'))
  }
  assert.equal(runtime.runs.length, 3)
  assert.equal(runtime.runs[0].env.CLAUDE_CODE_MAX_OUTPUT_TOKENS, '2048')
  assert.equal(runtime.runs[1].env.CLAUDE_CODE_MAX_OUTPUT_TOKENS, '4096')
  assert.equal(runtime.runs[2].env.CLAUDE_CODE_MAX_OUTPUT_TOKENS, undefined)
  const controls = runtime.runs[0].received.filter((message) => message.type === 'control_request').map((message) => message.request)
  assert(controls.some((control) => control.subtype === 'set_model' && control.model === 'opus'))
  assert(controls.some((control) => control.subtype === 'apply_flag_settings' && control.settings.effortLevel === 'high'))
  assert(controls.some((control) => control.subtype === 'apply_flag_settings' && control.settings.effortLevel === null))
})

test('MCP disconnect fails closed, and unoffered built-ins stay invisible while native continues', async (t) => {
  const disconnected = await setup(t, { plans: [{ mcpConnected: false }] })
  await assert.rejects(disconnected.run(request(first, { tools })), { code: 'MCP_CONNECTION' })
  assert.equal(disconnected.runs[0].toolResults.length, 0)
  const runtime = await setup(t, { plans: [{ actions: [{ type: 'unknown-tool' }] }] })
  const parts = await runtime.run(request(first, { tools }))
  assert.equal(parts.filter((part) => part.type === 'tool-call').length, 0)
  assert.equal(text(parts), 'NATIVE-NO-SUCH-TOOL')
})

test('old binaries are rejected before a prompt; terminal errors are not generation-retried', async (t) => {
  const old = await setup(t, { plans: [{ init: { claude_code_version: '2.1.247' } }] })
  await assert.rejects(old.run(), { code: 'INCOMPATIBLE_VERSION' })
  assert.equal(old.runs[0].received.filter((message) => message.type === 'user').length, 0)
  const runtime = await setup(t, { plans: [{ actions: [{ type: 'error', text: 'Subscription limit reached' }] }] })
  const parts = await runtime.run()
  assert.equal(parts.filter((part) => part.type === 'error').length, 1)
  assert.match(parts.find((part) => part.type === 'error').error.message, /Subscription limit/)
  assert.equal(text(parts), '')
  assert.equal(runtime.runs[0].received.filter((message) => message.type === 'user').length, 1)
})

test('images/PDFs, linked images and tool metadata retain native representation', async (t) => {
  const runtime = await setup(t, { plans: [{ actions: [{ type: 'tools', calls: [{ id: 'media-id', name: 'mcp__opencode__lookup', input: {} }] }] }] })
  const prompt = [...first, { role: 'user', content: [
    { type: 'file', mediaType: 'image/png', data: 'aW1hZ2U=' },
    { type: 'file', mediaType: 'application/pdf', data: 'cGRm' },
  ] }]
  const one = await runtime.run(request(prompt, { tools }))
  const input = runtime.runs[0].received.find((message) => message.type === 'user').message.content
  assert(input.some((block) => block.type === 'image' && block.source.type === 'base64'))
  assert(input.some((block) => block.type === 'document' && block.source.type === 'base64'))
  appendTools(prompt, one, [{ type: 'content', value: [
    { type: 'text', text: 'MEDIA' }, { type: 'file-data', mediaType: 'application/pdf', data: 'cGRm' },
    { type: 'image-url', url: 'https://example.invalid/fixture.png' },
  ] }])
  prompt.at(-1).content[0].providerMetadata = { 'claude-agent': { toolError: true } }
  await runtime.run(request(prompt, { tools }))
  const result = runtime.runs[0].toolResults[0]
  assert.equal(result.output.isError, true)
  assert(result.replacement.some((block) => block.type === 'document'))
  assert(result.replacement.some((block) => block.type === 'image' && block.source.type === 'url'))
  const aborted = structuredClone(prompt)
  aborted.at(-1).content[0].providerOptions = { 'claude-agent': { toolAborted: true } }
  assert.equal(collectResults(aborted, aborted.length - 1).aborted, true)
  assert.equal(toolResult({ type: 'execution-denied' }).mcp.isError, true)
})

test('eight live native processes are a hard cap, including indefinitely executing tools', async (t) => {
  const runtime = await setup(t, { plans: Array.from({ length: MAX_PROCESSES }, () => ({ actions: [{ type: 'tools' }] })) })
  for (let i = 0; i < MAX_PROCESSES; i++) await runtime.run(request(first, { sessionID: 'session-' + i, tools }))
  assert.equal(runtime.io.liveCount, 8)
  await assert.rejects(runtime.run(request(first, { sessionID: 'session-overflow', tools })), /eight native Claude process slots/)
  assert.equal(runtime.runs.length, 8)
})

test('bounded checkpoint cache persists hashes/UUIDs only, atomically, without becoming a history database', async (t) => {
  const runtime = await setup(t)
  const storage = recordStore(runtime.directory)
  const point = { hash: 'a'.repeat(64), claudeMessageID: crypto.randomUUID() }
  const record = { claudeSessionID: crypto.randomUUID(), history: [point], interrupted: true }
  await storage.set('scoped-session', record)
  assert.deepEqual(await recordStore(runtime.directory).get('scoped-session'), record)
  const file = runtime.directory + '/openproxy-checkpoints.json'
  const saved = await readFile(file, 'utf8')
  assert(!saved.includes('scoped-session'))
  assert(!saved.includes('Remember MARKER'))
  assert.equal((await stat(file)).mode & 0o777, 0o600)
  await storage.set('oversized', { ...record, history: Array(MAX_HISTORY + 1).fill(point) })
  assert.equal((await storage.get('oversized')).history, undefined)
  await writeFile(file, JSON.stringify({ version: 1, records: [null, ['a'.repeat(64), { ...record, history: [null] }]] }))
  assert.equal(await recordStore(runtime.directory).get('scoped-session'), undefined)
})

test('usage is deduplicated per API response, not per chunk or cumulative turn total', () => {
  const usage = usageTracker()
  usage.update('one', { input_tokens: 10, output_tokens: 2 })
  usage.update('one', { input_tokens: 10, output_tokens: 5 })
  usage.update('two', { input_tokens: 20, cache_read_input_tokens: 30, output_tokens: 3 })
  usage.result({ input_tokens: 999, output_tokens: 999 })
  assert.equal(usage.snapshot().inputTokens.total, 50)
  assert.equal(usage.snapshot().outputTokens.total, 8)
})

test('bridge handles controls during a run and issues exactly one done per command, including errors', async (t) => {
  const gate = Promise.withResolvers()
  const runtime = await setup(t, { plans: [{ actions: [{ type: 'partial' }], interruptGate: gate.promise }] })
  t.after(() => gate.resolve())
  const input = new PassThrough()
  const output = new PassThrough()
  const replies = []
  let buffer = ''
  output.on('data', (chunk) => {
    buffer += chunk.toString()
    let end
    while ((end = buffer.indexOf('\n')) >= 0) { replies.push(JSON.parse(buffer.slice(0, end))); buffer = buffer.slice(end + 1) }
  })
  const serving = serve({ input, output, io: runtime.io, storage: runtime.storage })
  const send = (command) => input.write(JSON.stringify(command) + '\n')
  send({ op: 'run', id: 'running', request: request() })
  await until(() => replies.some((reply) => reply.part?.type === 'text-delta'))
  send({ op: 'idle', id: 'idle', sessionID: 'session-a' })
  await until(() => replies.some((reply) => reply.id === 'idle' && reply.type === 'done'))
  send({ op: 'cancel', id: 'cancel', targetId: 'running' })
  await until(() => runtime.runs[0].received.some((message) => message.request?.subtype === 'interrupt'))
  assert(!replies.some((reply) => reply.id === 'cancel' && reply.type === 'done'))
  gate.resolve()
  await until(() => replies.some((reply) => reply.id === 'cancel' && reply.type === 'done'))
  send({ op: 'run', id: 'invalid', request: request(first, { connection: { apiKey: 'not-allowed' } }) })
  await until(() => replies.some((reply) => reply.id === 'invalid' && reply.type === 'done'))
  send({ op: 'shutdown', id: 'shutdown' })
  await serving
  for (const id of ['running', 'idle', 'cancel', 'invalid', 'shutdown']) assert.equal(replies.filter((reply) => reply.id === id && reply.type === 'done').length, 1)
  assert.equal(replies.find((reply) => reply.id === 'running' && reply.type === 'error').error.code, 'CANCELLED')
  assert.equal(replies.find((reply) => reply.id === 'invalid' && reply.type === 'error').error.code, 'INVALID_REQUEST')
  assert.equal(runtime.io.liveCount, 0)
})

test('protocol parser/write queue bounds are enforced, and arbitrary SDK overrides are rejected', async () => {
  assert.throws(() => validateRequest(request(first, { pathToClaudeCodeExecutable: 'anything' })), /Unsupported/)
  const input = new PassThrough()
  input.end(Buffer.alloc(MAX_FRAME_BYTES + 1, 32))
  await assert.rejects(async () => { for await (const _ of commands(input)) { /* consume */ } }, /exceeds 32 MiB/)
  const output = new PassThrough()
  output.on('error', () => {})
  const writer = new ProtocolWriter(output)
  await assert.rejects(writer.send({ text: 'x'.repeat(MAX_FRAME_BYTES) }), /queue exceeded/)
  const broken = new Writable({ write(_chunk, _encoding, callback) {
    callback(Object.assign(new Error('fixture disconnected'), { code: 'EPIPE' }))
  } })
  const disconnected = new ProtocolWriter(broken)
  await assert.rejects(disconnected.send({ id: 'broken', type: 'done' }), { code: 'EPIPE' })
  await assert.rejects(disconnected.flush(), { code: 'EPIPE' })
})

test('node bridge.mjs launches only the supplied native fixture and probes without a user prompt', async (t) => {
  const runtime = await setup(t)
  const bridge = fileURLToPath(new URL('../bridge.mjs', import.meta.url))
  const executable = fileURLToPath(new URL('./fake-cli.mjs', import.meta.url))
  const child = spawn(process.execPath, [bridge], {
    env: { PATH: process.env.PATH, CLAUDE_CONFIG_DIR: runtime.directory, OPENPROXY_CLAUDE_PATH: executable },
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  t.after(() => { if (child.exitCode === null) child.kill('SIGTERM') })
  const exit = once(child, 'exit')
  const replies = []
  let buffer = ''
  let stderr = ''
  child.stderr.on('data', (chunk) => { stderr += chunk })
  child.stdout.on('data', (chunk) => {
    buffer += chunk
    let end
    while ((end = buffer.indexOf('\n')) >= 0) { replies.push(JSON.parse(buffer.slice(0, end))); buffer = buffer.slice(end + 1) }
  })
  child.stdin.write(JSON.stringify({ op: 'probe', id: 'probe' }) + '\n')
  // A real child needs wall-clock scheduling; this is still bounded and never contacts Anthropic.
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('Bridge fixture probe timed out')), 10_000)
    const inspect = () => {
      if (replies.some((reply) => reply.id === 'probe' && reply.type === 'done')) { clearTimeout(timeout); child.stdout.off('data', inspect); resolve() }
    }
    child.stdout.on('data', inspect)
    inspect()
  })
  child.stdin.write(JSON.stringify({ op: 'shutdown', id: 'shutdown' }) + '\n')
  assert.deepEqual(await exit, [0, null])
  assert.equal(stderr, '')
  assert.equal(replies.find((reply) => reply.id === 'probe' && reply.type === 'result')?.result.version, '2.1.301')
  assert.equal(replies.filter((reply) => reply.id === 'probe' && reply.type === 'done').length, 1)
})

test('an aborted client tool result drains the parked turn before starting a new native turn', async (t) => {
  const runtime = await setup(t, { plans: [{ actions: [{ type: 'tools' }, { type: 'text', text: 'AFTER-ABORT' }] }] })
  const prompt = structuredClone(first)
  const one = await runtime.run(request(prompt, { tools }))
  appendTools(prompt, one)
  prompt.at(-1).content[0].providerMetadata = { 'claude-agent': { toolError: true, toolAborted: true } }
  assert.equal(text(await runtime.run(request(prompt, { tools }))), 'AFTER-ABORT')
  assert.equal(runtime.runs.length, 1)
  assert.equal(runtime.runs[0].toolResults.length, 0)
  const inputs = runtime.runs[0].received.filter((message) => message.type === 'user')
  assert.equal(inputs.length, 2)
  assert.equal(inputs[1].priority, undefined)
  assert.match(JSON.stringify(inputs[1].message.content), /conversation-checkpoint/)
})

test('cancelling a generate snapshot deletes only its disposable fork and leaves the primary reusable', async (t) => {
  const runtime = await setup(t, { plans: [{}, { actions: [{ type: 'partial' }] }] })
  await runtime.run()
  const before = structuredClone(await runtime.storage.get('session-a'))
  const controller = new AbortController()
  const parts = []
  const running = runtime.registry.run(request(follow('Side task'), { kind: 'generate', sourceID: 'session-a' }),
    (part) => parts.push(part), controller.signal, 'side-call')
  await until(() => parts.some((part) => part.type === 'text-delta'))
  controller.abort()
  await assert.rejects(running, { code: 'CANCELLED' })
  assert.deepEqual(await runtime.storage.get('session-a'), before)
  assert.deepEqual(runtime.removed, [runtime.forks[0].sessionID])
  assert(!runtime.runs[0].received.some((message) => message.request?.subtype === 'interrupt'))
  assert.equal(text(await runtime.run(request(follow()))), 'ANSWER')
  assert.equal(runtime.io.liveCount, 1)
})

test('native client-session forks preserve the shared prefix without mutating their parent', async (t) => {
  const runtime = await setup(t)
  await runtime.run()
  const parent = structuredClone(await runtime.storage.get('session-a'))
  await runtime.run(request(follow('Child question'), { sessionID: 'session-child', sourceID: 'session-a' }))
  assert.deepEqual(await runtime.storage.get('session-a'), parent)
  const child = await runtime.storage.get('session-child')
  assert.notEqual(child.claudeSessionID, parent.claudeSessionID)
  assert.notEqual(child.history[1].claudeMessageID, parent.history[1].claudeMessageID)
  assert(runtime.runs[1].args.includes('--resume=' + child.claudeSessionID))
  const childInput = runtime.runs[1].received.find((message) => message.type === 'user').message.content
  assert.deepEqual(childInput, [{ type: 'text', text: 'Child question' }])
})

test('startup cancellation reaps the process without a checkpoint; native max_tokens maps to length', async (t) => {
  const startup = Promise.withResolvers()
  const runtime = await setup(t, { plans: [{ startupGate: startup.promise }, { stopReason: 'max_tokens' }] })
  t.after(() => startup.resolve())
  const controller = new AbortController()
  const running = runtime.run(request(), controller.signal)
  await until(() => runtime.runs[0]?.received.some((message) => message.request?.subtype === 'initialize'))
  controller.abort()
  await assert.rejects(running, { code: 'CANCELLED' })
  assert.equal(runtime.io.liveCount, 0)
  assert.equal(runtime.storage.data.size, 0)
  const parts = await runtime.run()
  assert.equal(parts.find((part) => part.type === 'finish').finishReason.unified, 'length')
  assert(!runtime.runs[1].args.some((arg) => arg.startsWith('--resume=')))
})
