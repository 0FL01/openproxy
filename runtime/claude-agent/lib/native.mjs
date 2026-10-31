// Public SDK/native-process boundary. No credential files or Anthropic HTTP are accessed here.
import { spawn } from 'node:child_process'
import { Transform, Writable } from 'node:stream'
import { mkdir } from 'node:fs/promises'
import { join, isAbsolute } from 'node:path'
import { RuntimeError, abortable, deadline } from './primitives.mjs'
import { toolHooks, toolServer } from './tools.mjs'

export const STARTUP_MS = 30_000
export const DRAIN_MS = 30_000
// The public body is at most 32 MiB; reserve bounded room for its IPC envelope.
export const MAX_FRAME_BYTES = 32 * 1024 * 1024 + 4096
export const MAX_WRITE_BYTES = 32 * 1024 * 1024
export const MAX_PROCESSES = 8

// PR #58782 claude/query-options.ts: these are deliberately not user-configurable.
export function isolatedEnv(env) {
  const result = {
    ...env,
    CLAUDE_CODE_DISABLE_CLAUDE_MDS: '1', CLAUDE_CODE_DISABLE_AUTO_MEMORY: '1',
    DISABLE_AUTO_COMPACT: '1', CLAUDE_CODE_ENABLE_AWAY_SUMMARY: '0',
    ENABLE_TOOL_SEARCH: 'false', CLAUDE_CODE_DISABLE_ADVISOR_TOOL: '1',
    MAX_MCP_OUTPUT_TOKENS: '200000',
  }
  // SDK 0.3.301 also recognizes AWS/Google/Mantle/gateway selectors and federation
  // identity/profile/socket overrides. No ANTHROPIC_* override belongs to this profile.
  for (const name of Object.keys(result)) if (name.startsWith('ANTHROPIC_')) delete result[name]
  // One personal native profile: ambient API/cloud/gateway credentials must not change billing/auth.
  for (const name of [
    'CLAUDE_CODE_SIMPLE', 'CLAUDE_CODE_EFFORT_LEVEL', 'MCP_TOOL_TIMEOUT',
    'CLAUDE_CODE_MAX_OUTPUT_TOKENS', 'CLAUDE_CODE_EXTRA_BODY',
    'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'ANTHROPIC_BASE_URL', 'ANTHROPIC_CUSTOM_HEADERS',
    'CLAUDE_CODE_OAUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR', 'CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR',
    'CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY',
    'CLAUDE_CODE_USE_ANTHROPIC_AWS', 'CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD', 'CLAUDE_CODE_USE_MANTLE', 'CLAUDE_CODE_USE_GATEWAY',
    'ANTHROPIC_FOUNDRY_API_KEY', 'ANTHROPIC_FOUNDRY_BASE_URL', 'ANTHROPIC_FOUNDRY_RESOURCE',
    'AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_SESSION_TOKEN', 'AWS_PROFILE',
    'AWS_WEB_IDENTITY_TOKEN_FILE', 'AWS_CONTAINER_CREDENTIALS_RELATIVE_URI', 'AWS_CONTAINER_CREDENTIALS_FULL_URI',
    'GOOGLE_APPLICATION_CREDENTIALS',
    // Disable ambient SDK debugging, which can capture messages or auth details.
    'DEBUG_CLAUDE_AGENT_SDK', 'CLAUDE_CODE_DEBUG_LOGS_DIR', 'CLAUDE_CODE_DEBUG_LOG',
  ]) delete result[name]
  return result
}

class LimitedWritable extends Writable {
  constructor(target) {
    super({ highWaterMark: 16_384 })
    this.target = target
    target.on('error', (error) => this.destroy(error))
  }
  write(chunk, encoding, callback) {
    if (this.writableLength + Buffer.byteLength(chunk) > MAX_WRITE_BYTES) {
      this.destroy(new RuntimeError('Native Claude input queue exceeded its limit', 'RESOURCE_LIMIT'))
      return false
    }
    return super.write(chunk, encoding, callback)
  }
  _write(chunk, encoding, callback) { this.target.write(chunk, encoding, callback) }
  _final(callback) { this.target.end(callback) }
  _destroy(error, callback) { this.target.destroy(); callback(error) }
}

function framedOutput(source) {
  let frameBytes = 0
  const bounded = new Transform({
    highWaterMark: 16_384,
    transform(chunk, _, callback) {
      for (let i = 0; i < chunk.length; i++) {
        if (chunk[i] === 10) frameBytes = 0
        else if (++frameBytes > MAX_FRAME_BYTES) return callback(new RuntimeError('Native Claude output frame exceeded its limit', 'RESOURCE_LIMIT'))
      }
      callback(null, chunk)
    },
  })
  source.on('error', (error) => bounded.destroy(error))
  source.pipe(bounded)
  return bounded
}

export function nativeIO({ env = process.env, spawnProcess, limit = MAX_PROCESSES } = {}) {
  const active = new Set()
  const executable = env.OPENPROXY_CLAUDE_PATH || 'claude'
  const directory = env.CLAUDE_CONFIG_DIR
  if (!directory || !isAbsolute(directory)) throw new RuntimeError('CLAUDE_CONFIG_DIR must name the parent-supplied absolute native profile directory', 'CONFIGURATION')
  const cwd = join(directory, 'openproxy-workspace')
  return {
    executable, directory, cwd,
    env: () => isolatedEnv(env),
    async prepare() { await mkdir(cwd, { recursive: true, mode: 0o700 }) },
    get liveCount() { return active.size },
    launch() {
      let child
      const exited = Promise.withResolvers()
      let launched = false
      return {
        spawn(options) {
          if (active.size >= limit) throw new RuntimeError('All eight native Claude process slots are busy', 'BUSY')
          launched = true
          child = spawnProcess ? spawnProcess(options) : spawn(options.command, options.args, {
            cwd: options.cwd, env: options.env, signal: options.signal,
            stdio: ['pipe', 'pipe', 'ignore'], shell: false,
          })
          active.add(child)
          const finish = () => { active.delete(child); exited.resolve() }
          child.once('exit', finish)
          // A spawned child can emit AbortError before it exits. It still occupies a slot until exit.
          child.once('error', () => { if (!child.pid) finish() })
          const stdout = framedOutput(child.stdout)
          stdout.once('error', () => child.kill('SIGTERM'))
          return {
            stdin: new LimitedWritable(child.stdin), stdout,
            get killed() { return child.killed }, get exitCode() { return child.exitCode },
            get signalCode() { return child.signalCode },
            kill: (signal) => child.kill(signal),
            on: (...args) => child.on(...args), once: (...args) => child.once(...args), off: (...args) => child.off(...args),
          }
        },
        async reap() {
          if (!launched || !child) return
          try { await deadline(exited.promise, 5000, 'Native Claude did not exit') }
          catch { child.kill('SIGKILL'); await deadline(exited.promise, 5000, 'Native Claude could not be reaped') }
        },
      }
    },
    async exists(id) {
      const { getSessionInfo } = await import('@anthropic-ai/claude-agent-sdk')
      return (await getSessionInfo(id, { dir: cwd })) !== undefined
    },
    async fork(id, messageID) {
      // Public SDK transcript API only. It never opens a login/credential file.
      const { deleteSession, forkSession, getSessionMessages } = await import('@anthropic-ai/claude-agent-sdk')
      const source = await getSessionMessages(id, { dir: cwd })
      const end = source.findIndex((message) => message.uuid === messageID)
      if (end < 0) return
      const fork = await forkSession(id, { dir: cwd, upToMessageId: messageID })
      try {
        const copied = await getSessionMessages(fork.sessionId, { dir: cwd })
        if (copied.length !== end + 1) throw new Error('Claude fork did not preserve the requested history boundary')
        const messages = new Map()
        for (let i = 0; i < copied.length; i++) {
          if (source[i].type !== copied[i].type || JSON.stringify(source[i].message) !== JSON.stringify(copied[i].message)) throw new Error('Claude fork changed the copied conversation')
          messages.set(source[i].uuid, copied[i].uuid)
        }
        return { sessionID: fork.sessionId, messages }
      } catch (error) {
        await deleteSession(fork.sessionId, { dir: cwd })
        throw error
      }
    },
    async remove(id) {
      const { deleteSession } = await import('@anthropic-ai/claude-agent-sdk')
      await deleteSession(id, { dir: cwd })
    },
  }
}

export function baseOptions(io, launch) {
  return {
    pathToClaudeCodeExecutable: io.executable,
    spawnClaudeCodeProcess: launch.spawn,
    env: io.env(), cwd: io.cwd,
    settingSources: [], settings: { disableAllHooks: true },
    thinking: { type: 'adaptive', display: 'summarized' },
    strictMcpConfig: true, skills: [], tools: [], permissionMode: 'dontAsk',
    allowedTools: ['mcp__opencode__*'], verbatimPrompts: true,
    stderr: () => {},
  }
}

export async function sessionOptions(io, launch, request, pending, resume) {
  const options = {
    ...baseOptions(io, launch), model: request.model, effort: request.effort,
    systemPrompt: request.systemPrompt, includePartialMessages: true,
    persistSession: request.sessionID !== undefined, hooks: toolHooks(pending), mcpServers: {},
  }
  if (request.maxOutputTokens !== undefined) options.env.CLAUDE_CODE_MAX_OUTPUT_TOKENS = String(request.maxOutputTokens)
  if (request.tools.length) options.mcpServers.opencode = await toolServer(request.tools, pending)
  if (resume) options.resume = resume
  else options.sessionId = crypto.randomUUID()
  return options
}

export function atLeast(version, minimum) {
  if (typeof version !== 'string' || !/^\d+\.\d+\.\d+(?:[-+].*)?$/.test(version)) return false
  const parts = version.split(/[-+]/)[0].split('.').map(Number)
  const required = minimum.split('.').map(Number)
  const index = required.findIndex((part, position) => parts[position] !== part)
  return index === -1 || parts[index] > required[index]
}

export function checkVersion(version) {
  if (atLeast(version, '2.1.248')) return
  throw new RuntimeError(version ? `Claude Code ${version} is too old; 2.1.248 or later is required. Run \`claude update\`.`
    : 'Claude Code did not report its version. Run `claude update`.', 'INCOMPATIBLE_VERSION')
}

export function inventory(init) {
  const account = {}
  for (const name of ['email', 'organization', 'subscriptionType', 'tokenSource', 'apiKeySource', 'apiProvider']) {
    if (typeof init.account?.[name] === 'string') account[name] = init.account[name].slice(0, 4096)
  }
  const models = (init.models ?? []).slice(0, 256).map((model) => {
    const result = {}
    for (const name of ['value', 'displayName', 'description', 'resolvedModel']) if (typeof model[name] === 'string') result[name] = model[name].slice(0, 4096)
    for (const name of ['supportsEffort', 'supportsAdaptiveThinking', 'supportsFastMode', 'supportsAutoMode', 'supportsThinkingOff']) if (typeof model[name] === 'boolean') result[name] = model[name]
    if (Array.isArray(model.supportedEffortLevels)) result.supportedEffortLevels = model.supportedEffortLevels.filter((effort) => ['low', 'medium', 'high', 'xhigh', 'max'].includes(effort))
    if (Number.isSafeInteger(model.contextWindow) && model.contextWindow > 0) result.contextWindow = model.contextWindow
    return result
  })
  return { version: typeof init.claude_code_version === 'string' ? init.claude_code_version : undefined, account, models }
}

export async function probe(io, signal) {
  await io.prepare()
  const { query } = await import('@anthropic-ai/claude-agent-sdk')
  const stop = Promise.withResolvers()
  const launch = io.launch()
  async function* noPrompt() { await stop.promise }
  const session = query({ prompt: noPrompt(), options: { ...baseOptions(io, launch), persistSession: false } })
  try {
    const init = await abortable(deadline(session.initializationResult(), STARTUP_MS, 'claude did not start within 30 seconds'), signal)
    const result = inventory(init)
    // No user message: set_model/get_context_usage are native control-plane queries, not inference.
    if (atLeast(result.version, '2.1.257')) {
      await abortable(deadline((async () => {
        for (const model of result.models) {
          if (model.value === 'default') continue
          try {
            await session.setModel(model.value)
            const usage = await session.getContextUsage({ detail: 'summary' })
            if (Number.isSafeInteger(usage.rawMaxTokens) && usage.rawMaxTokens > 0) model.contextWindow = usage.rawMaxTokens
          } catch { /* Native context discovery is optional; retain the conservative fallback. */ }
        }
      })(), STARTUP_MS, 'Native context discovery timed out'), signal).catch((error) => {
        if (signal?.aborted) throw error
      })
    }
    return result
  } finally {
    stop.resolve()
    session.close()
    await launch.reap()
  }
}
