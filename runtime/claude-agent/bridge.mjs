#!/usr/bin/env node
// Internal NDJSON bridge. This is not a public API or a server-side tool executor.
import { pathToFileURL } from 'node:url'
import { nativeIO, probe } from './lib/native.mjs'
import { recordStore } from './lib/records.mjs'
import { sessionRegistry } from './lib/sessions.mjs'
import { ProtocolWriter, commands, validID, validateRequest } from './lib/protocol.mjs'
import { RuntimeError, errorJSON } from './lib/primitives.mjs'

export async function serve({ input = process.stdin, output = process.stdout, env = process.env, io, storage } = {}) {
  const writer = new ProtocolWriter(output)
  const active = new Map()
  let registry
  let stopping = false
  let shutdown
  async function close() {
    shutdown ??= (async () => {
      stopping = true
      for (const task of active.values()) task.controller.abort()
      await registry?.close()
      await Promise.all([...active.values()].map((task) => task.done))
    })()
    return shutdown
  }
  const signal = () => { input.destroy(); void close().catch(() => {}) }
  process.once('SIGTERM', signal)
  process.once('SIGINT', signal)
  try {
    // SDK-side diagnostics read process.env, not the child environment passed in Options.
    // Suppress ambient SDK debug capture before the SDK's first (lazy) import.
    delete process.env.DEBUG_CLAUDE_AGENT_SDK
    io ??= nativeIO({ env })
    storage ??= recordStore(io.directory)
    registry = sessionRegistry({ io, storage })
    for await (const command of commands(input)) {
      const id = validID(command?.id) ? command.id : null
      if (active.has(id)) {
        // Never issue a second done for an already-active ID.
        await writer.send({ id: null, type: 'error', error: { message: 'Duplicate active command ID', code: 'INVALID_COMMAND' } })
        await writer.send({ id: null, type: 'done' })
        continue
      }
      if (command?.op === 'shutdown' && id !== null) {
        try { await close() }
        catch (error) { await writer.send({ id, type: 'error', error: errorJSON(error) }) }
        await writer.send({ id, type: 'done' })
        break
      }
      if (active.size >= 128 || active.size >= 64 && command?.op !== 'cancel' && command?.op !== 'idle') {
        await writer.send({ id, type: 'error', error: { message: 'Claude Agent command queue is full', code: 'BUSY' } })
        await writer.send({ id, type: 'done' })
        continue
      }
      const controller = new AbortController()
      const done = Promise.withResolvers()
      active.set(id, { controller, done: done.promise })
      // Do not await this task: a native turn can be parked until a later run supplies MCP results.
      void (async () => {
        try {
          if (id === null || !command || typeof command.op !== 'string') throw new RuntimeError('Invalid Claude Agent command', 'INVALID_COMMAND')
          if (stopping) throw new RuntimeError('Claude Agent runtime is shutting down', 'SHUTDOWN')
          switch (command.op) {
            case 'probe':
              await writer.send({ id, type: 'result', result: await probe(io, controller.signal) })
              break
            case 'run':
              await registry.run(validateRequest(command.request), (part) => writer.send({ id, type: 'part', part }), controller.signal, id)
              break
            case 'idle':
              if (typeof command.sessionID !== 'string') throw new RuntimeError('idle requires a sessionID', 'INVALID_COMMAND')
              registry.idle(command.sessionID)
              break
            case 'cancel': {
              if (!validID(command.targetId) && typeof command.sessionID !== 'string') throw new RuntimeError('cancel requires a targetId or sessionID', 'INVALID_COMMAND')
              if (command.targetId === id) throw new RuntimeError('A cancel cannot target itself', 'INVALID_COMMAND')
              const target = active.get(command.targetId)
              target?.controller.abort()
              await registry.cancel({ targetId: validID(command.targetId) ? command.targetId : undefined, sessionID: command.sessionID })
              if (target) await target.done
              break
            }
            default: throw new RuntimeError('Unknown Claude Agent command', 'INVALID_COMMAND')
          }
        } catch (error) {
          await writer.send({ id, type: 'error', error: errorJSON(error) })
        } finally {
          try { await writer.send({ id, type: 'done' }) }
          finally { active.delete(id); done.resolve() }
        }
      })().catch(() => { void close().catch(() => {}); input.destroy() })
    }
  } catch (error) {
    if (!stopping) {
      await writer.send({ id: null, type: 'error', error: errorJSON(error) }).catch(() => {})
      await writer.send({ id: null, type: 'done' }).catch(() => {})
    }
    if (!registry) process.exitCode = 1
  } finally {
    await close()
    await writer.flush()
    process.removeListener('SIGTERM', signal)
    process.removeListener('SIGINT', signal)
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  // Native stderr is intentionally not relayed/logged. stdout is strictly framed protocol.
  serve().catch(() => { process.exitCode = 1 })
}
