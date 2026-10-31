// Bounded, non-authoritative checkpoint cache: hashes and native UUIDs, never messages or credentials.
import { createHash } from 'node:crypto'
import { mkdir, open, readFile, rename, stat, unlink } from 'node:fs/promises'
import { join } from 'node:path'
import { Mutex } from './primitives.mjs'

export const MAX_HISTORY = 8192
export const MAX_RECORDS = 512
export const MAX_RECORD_BYTES = 16 * 1024 * 1024
const key = (id) => createHash('sha256').update(id).digest('hex')

export function boundedRecord(record) {
  if (!record || !/^[\da-f-]{36}$/i.test(record.claudeSessionID ?? '')) return undefined
  const history = record.history
  if (history !== undefined && (!Array.isArray(history) || !history.every((point) => point && typeof point === 'object'
    && /^[\da-f]{64}$/.test(point.hash ?? '') && (point.claudeMessageID === undefined || /^[\da-f-]{36}$/i.test(point.claudeMessageID))))) return undefined
  return {
    claudeSessionID: record.claudeSessionID,
    // An over-budget cache rebuilds from the client's history; truncating a prefix would incorrectly resume.
    history: history?.length > MAX_HISTORY ? undefined : history,
    interrupted: record.interrupted === true,
  }
}

export function recordStore(directory) {
  const file = join(directory, 'openproxy-checkpoints.json')
  const lock = new Mutex()
  let records
  async function load() {
    if (records) return
    records = new Map()
    try {
      if ((await stat(file)).size > MAX_RECORD_BYTES) return
      const saved = JSON.parse(await readFile(file, 'utf8'))
      if (saved.version !== 1 || !Array.isArray(saved.records)) return
      for (const item of saved.records.slice(-MAX_RECORDS)) {
        if (!Array.isArray(item) || item.length !== 2) continue
        const [id, record] = item
        const valid = boundedRecord(record)
        if (/^[\da-f]{64}$/.test(id) && valid) records.set(id, valid)
      }
    } catch (error) {
      if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error
    }
  }
  return {
    async get(id) {
      return lock.use(async () => { await load(); return records.get(key(id)) })
    },
    async set(id, value) {
      return lock.use(async () => {
        await load()
        const record = boundedRecord(value)
        if (!record) throw new Error('Invalid native session checkpoint')
        const name = key(id)
        records.delete(name)
        records.set(name, record)
        let text
        while (true) {
          if (records.size > MAX_RECORDS) records.delete(records.keys().next().value)
          text = JSON.stringify({ version: 1, records: [...records] })
          if (Buffer.byteLength(text) <= MAX_RECORD_BYTES) break
          records.delete(records.keys().next().value)
        }
        await mkdir(directory, { recursive: true, mode: 0o700 })
        const temporary = file + '.' + process.pid + '.tmp'
        const handle = await open(temporary, 'w', 0o600)
        try {
          await handle.writeFile(text)
          await handle.sync()
          await handle.close()
          await rename(temporary, file)
        } catch (error) {
          await handle.close().catch(() => {})
          await unlink(temporary).catch(() => {})
          throw error
        }
        return record
      })
    },
  }
}
