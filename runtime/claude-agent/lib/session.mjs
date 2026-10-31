// Port of PR #58782 claude/session.ts. One SDK iterator owns all reads, including cancellation drains.
import { checkVersion, inventory, DRAIN_MS } from './native.mjs'
import { originalName } from './tools.mjs'
import { RuntimeError, abortable, checkAbort, deadline } from './primitives.mjs'

const MAX_MESSAGES = 1024
const MAX_BUFFER_BYTES = 32 * 1024 * 1024

export async function startSession(options, launch) {
  const { query } = await import('@anthropic-ai/claude-agent-sdk')
  let input
  const prompt = new ReadableStream({ start(controller) { input = controller } })
  const session = query({ prompt, options })
  let turn
  let initialized = false
  let ended = false
  let failure
  let stopping
  let persisted = false
  const ready = session.initializationResult().then((init) => {
    checkVersion(init.claude_code_version)
    initialized = true
    return inventory(init)
  })
  ready.catch(() => {})

  function finishTurn() {
    if (!turn) return
    turn.finished = true
    turn.drained.resolve()
    turn.wake?.()
  }
  const pump = (async () => {
    try {
      for await (const message of session) {
        if (turn) {
          const size = Buffer.byteLength(JSON.stringify(message))
          if (turn.messages.length >= MAX_MESSAGES || turn.bytes + size > MAX_BUFFER_BYTES) throw new RuntimeError('Native Claude event queue exceeded its limit', 'RESOURCE_LIMIT')
          turn.messages.push({ message, size })
          turn.bytes += size
          turn.wake?.()
        }
        if (message.type === 'assistant' || message.type === 'result') persisted = true
        if (message.type === 'result') finishTurn()
      }
    } catch (error) {
      failure = error
      session.close()
    } finally {
      ended = true
      finishTurn()
    }
  })()

  async function interrupt() {
    const active = turn
    if (!active || active.finished || ended) return
    // The receipt precedes the final result and transcript flush. Do NOT release MCP calls first.
    try {
      await deadline((async () => { await session.interrupt(); await active.drained.promise })(), DRAIN_MS, 'Native Claude cancellation did not drain within 30 seconds')
    } catch (error) {
      session.close()
      await deadline(pump, 5000, 'Native Claude reader did not close').catch(() => {})
      await launch.reap()
      throw error
    }
  }

  async function shutdown() {
    try {
      if (initialized && !ended) {
        await deadline((async () => {
          await session.interrupt()
          if (turn && !turn.finished) await turn.drained.promise
        })(), DRAIN_MS, 'Native Claude shutdown did not drain')
      }
      input.close()
      if (!initialized) session.close()
      await deadline(pump, DRAIN_MS, 'Native Claude shutdown timed out')
    } finally {
      session.close()
      await launch.reap()
    }
  }

  return {
    id: options.resume ?? options.sessionId,
    ready,
    get ended() { return ended }, get persisted() { return persisted },
    get running() { return !!turn && !turn.finished },
    begin(content, uuid = crypto.randomUUID()) {
      if (ended || stopping) throw failure ?? new Error('Claude Code session has stopped')
      if (turn && !turn.finished) throw new Error('Claude Code is still finishing the previous turn')
      turn = { drained: Promise.withResolvers(), messages: [], bytes: 0, finished: false }
      input.enqueue({ type: 'user', uuid, message: { role: 'user', content }, parent_tool_use_id: null })
    },
    async readStep(emit, tools, signal) {
      const active = turn
      if (!active) throw new Error('No Claude turn to read')
      let calledTools = false
      while (true) {
        checkAbort(signal)
        const item = active.messages.shift()
        if (!item) {
          if (ended || active.finished) throw failure ?? new Error('Claude Code stopped before finishing its reply')
          await abortable(new Promise((resolve) => { active.wake = resolve }), signal)
          continue
        }
        active.bytes -= item.size
        const message = item.message
        await emit(message)
        if (message.type === 'assistant' && message.parent_tool_use_id === null && !message.error) {
          if (message.message.content.some((part) => part.type === 'tool_use' && originalName(part.name, tools))) calledTools = true
        }
        if (calledTools && message.type === 'stream_event' && message.parent_tool_use_id === null && message.event.type === 'message_stop') return 'tools'
        if (message.type === 'result') return message
      }
    },
    addUser(content, uuid) {
      input.enqueue({ type: 'user', uuid, priority: 'next', message: { role: 'user', content }, parent_tool_use_id: null })
    },
    interrupt,
    stop() { stopping ??= shutdown(); return stopping },
    setModel: (model) => deadline(session.setModel(model), 30_000, 'Native model control timed out'),
    setEffort: (effort) => deadline(session.applyFlagSettings({ effortLevel: effort ?? null }), 30_000, 'Native effort control timed out'),
  }
}
