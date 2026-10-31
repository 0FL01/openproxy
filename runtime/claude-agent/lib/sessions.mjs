// Promise/AbortSignal port of PR #58782 claude/sessions.ts. MIT: ../LICENSE.upstream.
// OpenCode owns the history. These entries only track native processes and hash/UUID checkpoints.
import { Mutex, RuntimeError, abortable, checkAbort, deadline } from './primitives.mjs'
import { sessionOptions, STARTUP_MS, MAX_PROCESSES } from './native.mjs'
import { startSession } from './session.mjs'
import { pendingTools, originalName, collectResults } from './tools.mjs'
import { content, systemPrompt } from './messages.mjs'
import { catchUp, checkpoints, decide, boundary, canonical } from './sync.mjs'
import { boundedRecord } from './records.mjs'
import { streamConverter } from './stream.mjs'

export const IDLE_MS = 5 * 60_000
const MAX_ENTRIES = 512
const MAX_REQUESTS = 64
const launchKey = (request) => canonical([request.maxOutputTokens, request.systemPrompt, request.tools])

export function sessionRegistry({ io, storage, clock = { setTimeout, clearTimeout }, onInventory = async () => {} }) {
  const entries = new Map()
  const temporary = new Set()
  const tasks = new Map()
  let closing = false
  let closingTask

  function cancelIdle(entry) {
    if (entry.idle !== undefined) clock.clearTimeout(entry.idle)
    entry.idle = undefined
  }
  async function interrupt(entry) {
    const live = entry.process
    if (!live) return
    try { await live.session.interrupt() }
    finally { live.pending.clear(); live.waiting = undefined }
  }
  async function stop(entry) {
    const live = entry.process
    if (!live) return
    try { await live.session.stop() }
    finally { live.pending.clear(); entry.process = undefined }
  }
  function armIdle(entry) {
    cancelIdle(entry)
    entry.idle = clock.setTimeout(() => {
      entry.idle = undefined
      // Serialized with a new turn, including when shutdown has already begun.
      return entry.lock.use(() => stop(entry)).catch(() => {})
    }, IDLE_MS)
    entry.idle?.unref?.()
  }

  async function reclaim(entry) {
    if (io.liveCount < MAX_PROCESSES) return
    const unused = [...entries.values()].filter((candidate) => candidate !== entry && candidate.process
      && !candidate.active && !candidate.process.waiting && !candidate.process.session.running)
      .sort((a, b) => a.touched - b.touched)[0]
    if (unused) await unused.lock.use(async () => {
      if (unused.active || unused.process?.waiting || unused.process?.session.running) return
      cancelIdle(unused)
      await stop(unused)
    })
  }

  async function open(entry, request, signal) {
    const starting = !entry.process
    if (starting) {
      checkAbort(signal)
      await reclaim(entry)
      const pending = pendingTools()
      const launch = io.launch()
      try {
        const options = await sessionOptions(io, launch, request, pending, entry.record?.claudeSessionID)
        // Assign before awaiting ready, so cancellation always stops/reaps a started process.
        const session = await startSession(options, launch)
        entry.process = { session, pending, model: request.model, effort: request.effort, launch: launchKey(request) }
      } catch (error) { await launch.reap(); throw error }
    }
    const live = entry.process
    let probe
    try {
      probe = await abortable(deadline(live.session.ready, STARTUP_MS, 'claude did not start within 30 seconds'), signal)
    } catch (error) { await stop(entry).catch(() => {}); throw error }
    if (starting) {
      live.inventory = probe
      live.reportInventory = true
      await onInventory(probe)
    }
    checkAbort(signal)
    // Controls precede new input or the release of any parked MCP handlers.
    if (live.model !== request.model) {
      await abortable(live.session.setModel(request.model), signal)
      live.model = request.model
    }
    if (live.effort !== request.effort) {
      await abortable(live.session.setEffort(request.effort), signal)
      live.effort = request.effort
    }
    return live
  }

  async function turn(entry, request, start, continuation, emit, signal, id) {
    const blocks = request.sessionID || entry.record ? catchUp(request.prompt, continuation?.end ?? start) : content(request.prompt)
    const live = await open(entry, request, signal)
    const session = live.session
    const userID = !continuation || blocks.length ? crypto.randomUUID() : undefined
    const reply = []
    const toolMessages = new Map()
    const seen = new Set()
    const waiting = []
    let messageID
    let completed = false
    let started = false
    let parked = false
    const convert = streamConverter(request.tools)
    async function receive(message) {
      if (message.type === 'system' && message.subtype === 'init' && request.tools.length) {
        if (!message.mcp_servers?.some((server) => server.name === 'opencode' && server.status === 'connected')) {
          throw new RuntimeError('The OpenCode tool server did not connect to Claude Code', 'MCP_CONNECTION')
        }
      }
      if (message.type === 'assistant' && message.parent_tool_use_id === null && !message.error && !seen.has(message.uuid)) {
        seen.add(message.uuid)
        messageID = message.uuid
        for (const part of message.message.content) {
          if (part.type === 'text') { reply.push({ type: 'text', text: part.text }); continue }
          if (part.type !== 'tool_use') continue
          const name = originalName(part.name, request.tools)
          if (!name) continue
          live.pending.expect(part.id, part.name)
          waiting.push(part.id)
          reply.push({ type: 'tool-call', toolCallId: part.id, toolName: name, input: part.input })
        }
      }
      if (message.type === 'user' && message.uuid && Array.isArray(message.message.content)) {
        for (const part of message.message.content) if (part.type === 'tool_result') toolMessages.set(part.tool_use_id, message.uuid)
      }
      for (const part of convert(message)) {
        // A finish part supports providerMetadata in LMV3. Unlike a top-level result frame this
        // does not terminate the parent's stream, and refreshes discovery after native startup.
        if (part.type === 'finish' && live.reportInventory) {
          part.providerMetadata = { ...part.providerMetadata, 'claude-agent': { inventory: live.inventory } }
          live.reportInventory = false
        }
        await emit(part)
      }
    }
    try {
      checkAbort(signal)
      if (!continuation) session.begin(blocks, userID)
      else {
        if (userID) session.addUser(blocks, userID)
        for (const item of continuation.results) live.pending.resolve(item.id, item.result)
      }
      started = true
      entry.lastRequestId = id
      const outcome = await session.readStep(receive, request.tools, signal)
      checkAbort(signal)
      parked = outcome === 'tools'
      completed = true
      live.waiting = parked ? waiting : undefined
    } finally {
      try {
        if (!parked) await interrupt(entry)
      } finally {
        if (request.sessionID && session.persisted && started) {
          const interrupted = signal?.aborted === true && !completed
          const history = completed || interrupted ? checkpoints({
            prompt: request.prompt, previous: entry.record, userID,
            reply: completed ? reply : [], messageID: completed ? messageID : undefined, toolMessages,
          }) : undefined
          entry.record = boundedRecord({ claudeSessionID: session.id, history, interrupted })
          await storage.set(request.sessionID, entry.record)
        }
      }
    }
  }

  async function branch(entry, request, source) {
    const point = boundary(request.prompt, source)
    if (!source || !point) return 0
    // Reject unsupported content before creating an on-disk native branch.
    catchUp(request.prompt, point.start)
    const fork = await io.fork(source.claudeSessionID, point.messageID)
    if (!fork) return 0
    entry.record = {
      claudeSessionID: fork.sessionID,
      history: point.history.map((checkpoint) => ({
        hash: checkpoint.hash,
        claudeMessageID: checkpoint.claudeMessageID ? fork.messages.get(checkpoint.claudeMessageID) : undefined,
      })),
    }
    return point.start
  }

  async function snapshot(entry, request, signal) {
    if (!request.sourceID || request.sourceID === request.sessionID) return 0
    const source = entries.get(request.sourceID)
    // Reject cyclic simultaneous fork acquisitions rather than deadlocking two session locks.
    let ancestor = source
    const visited = new Set()
    while (ancestor?.active?.sourceID) {
      if (ancestor.active.sourceID === request.sessionID || visited.has(ancestor)) throw new RuntimeError('Cyclic native session fork', 'INVALID_REQUEST')
      visited.add(ancestor)
      ancestor = entries.get(ancestor.active.sourceID)
    }
    const copy = async () => {
      if (request.kind === 'compaction' && source) { cancelIdle(source); await stop(source) }
      const record = source?.record ?? await storage.get(request.sourceID)
      checkAbort(signal)
      return branch(entry, request, record)
    }
    return source ? source.lock.use(copy, signal) : copy()
  }

  async function prepare(entry, request, signal) {
    if (entry.process && (entry.process.session.ended || entry.process.launch !== launchKey(request))) await stop(entry)
    if (!request.sessionID) return { start: await snapshot(entry, request, signal) }
    entry.record ??= await storage.get(request.sessionID)
    const decision = decide(request.prompt, entry.record)
    let start = decision.start
    if (decision.kind === 'fresh' || decision.kind === 'changed') {
      await stop(entry)
      const previous = entry.record
      entry.record = undefined
      if (decision.kind === 'changed') start = await branch(entry, request, previous)
      else if (!previous) start = await snapshot(entry, request, signal)
    } else if (!entry.process && entry.record && !await io.exists(entry.record.claudeSessionID)) {
      entry.record = undefined
      start = 0
    }
    checkAbort(signal)
    const live = entry.process
    if (!live?.session.running || !live.waiting) return { start }
    const collected = collectResults(request.prompt, start)
    const continuation = { end: collected.end, results: [] }
    for (const id of live.waiting) {
      const result = collected.results.get(id)
      if (!result) { await stop(entry); entry.record = undefined; return { start: 0 } }
      continuation.results.push({ id, result })
    }
    if (collected.aborted) {
      await interrupt(entry)
      return { start }
    }
    return { start, continuation }
  }

  function entryFor(request) {
    if (request.sessionID) {
      const existing = entries.get(request.sessionID)
      if (existing) return existing
      if (entries.size >= MAX_ENTRIES) {
        const idle = [...entries].find(([, entry]) => !entry.active && !entry.process && !entry.queued)
        if (!idle) throw new RuntimeError('Native session cache is full', 'BUSY')
        cancelIdle(idle[1])
        entries.delete(idle[0])
      }
      const entry = { lock: new Mutex(), queued: 0, touched: Date.now() }
      entries.set(request.sessionID, entry)
      return entry
    }
    const entry = { lock: new Mutex(), touched: Date.now() }
    temporary.add(entry)
    return entry
  }

  async function run(input, emit, signal, id = crypto.randomUUID()) {
    if (closing) throw new RuntimeError('Claude Agent runtime is shutting down', 'SHUTDOWN')
    if (tasks.size >= MAX_REQUESTS) throw new RuntimeError('Native request queue is full', 'BUSY')
    if (tasks.has(id)) throw new RuntimeError('Duplicate native request ID', 'INVALID_REQUEST')
    const request = {
      ...input,
      sessionID: input.kind === 'primary' ? input.sessionID : undefined,
      sourceID: input.kind === 'title' ? undefined : input.sourceID,
      tools: input.kind === 'title' ? [] : input.tools.filter((tool) => tool.type === 'function'),
      systemPrompt: systemPrompt(input),
    }
    const controller = new AbortController()
    const combined = signal ? AbortSignal.any([controller.signal, signal]) : controller.signal
    const done = Promise.withResolvers()
    tasks.set(id, { controller, done: done.promise, sessionID: request.sessionID })
    let entry
    try {
      entry = entryFor(request)
      entry.queued = (entry.queued ?? 0) + 1
      await entry.lock.use(async () => {
        cancelIdle(entry)
        entry.active = { id, sourceID: request.sourceID }
        try {
          await io.prepare()
          const prepared = await prepare(entry, request, combined)
          await emit({ type: 'stream-start', warnings: [] })
          await turn(entry, request, prepared.start, prepared.continuation, emit, combined, id)
        } finally {
          entry.active = undefined
          entry.touched = Date.now()
          if (!request.sessionID) {
            try { await stop(entry) }
            finally {
              if (entry.record) await io.remove(entry.record.claudeSessionID)
              temporary.delete(entry)
            }
          } else if (!entry.process?.waiting) armIdle(entry)
        }
      }, combined)
    } finally {
      if (entry) entry.queued--
      tasks.delete(id)
      done.resolve()
    }
  }

  return {
    run,
    idle(sessionID) {
      const entry = entries.get(sessionID)
      // Pending tools are not idle until OpenCode explicitly says its execution has ended.
      if (entry?.process?.waiting && !entry.active) armIdle(entry)
    },
    async cancel({ targetId, sessionID }) {
      const affected = [...tasks].filter(([id, task]) => targetId !== undefined ? id === targetId : sessionID && task.sessionID === sessionID)
      for (const [, task] of affected) task.controller.abort()
      await Promise.all(affected.map(([, task]) => task.done))
      for (const [key, entry] of entries) {
        if (targetId !== undefined ? entry.lastRequestId !== targetId : key !== sessionID) continue
        await entry.lock.use(async () => {
          // A newer request may have acquired the session while this cancellation waited.
          if (targetId !== undefined && entry.lastRequestId !== targetId) return
          await interrupt(entry)
          armIdle(entry)
        })
      }
    },
    close() {
      closingTask ??= (async () => {
        closing = true
        for (const task of tasks.values()) task.controller.abort()
        await Promise.all([...tasks.values()].map((task) => task.done))
        const settled = await Promise.allSettled([...entries.values(), ...temporary].map((entry) => {
          cancelIdle(entry)
          return entry.lock.use(() => stop(entry))
        }))
        entries.clear()
        temporary.clear()
        const failure = settled.find((result) => result.status === 'rejected')
        if (failure) throw failure.reason
      })()
      return closingTask
    },
    // Safe counts are useful for diagnostics/tests; never expose conversation data.
    stats() { return { entries: entries.size, temporary: temporary.size, active: tasks.size, processes: io.liveCount } },
  }
}
