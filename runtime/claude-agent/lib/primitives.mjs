// OpenProxy's bounded transport/lifecycle support. No inference retry lives here.
export class RuntimeError extends Error {
  constructor(message, code = 'NATIVE_ERROR') {
    super(message)
    this.code = code
  }
}

export function abortError() {
  return new RuntimeError('Claude Agent request cancelled', 'CANCELLED')
}

export function checkAbort(signal) {
  if (signal?.aborted) throw abortError()
}

export async function abortable(promise, signal) {
  checkAbort(signal)
  if (!signal) return promise
  let listener
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        listener = () => reject(abortError())
        signal.addEventListener('abort', listener, { once: true })
      }),
    ])
  } finally {
    signal.removeEventListener('abort', listener)
  }
}

export async function deadline(promise, ms, message) {
  let timer
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new RuntimeError(message, 'TIMEOUT')), ms)
        timer.unref?.()
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

export class Mutex {
  #tail = Promise.resolve()
  #queued = 0

  async use(run, signal) {
    if (this.#queued >= 64) throw new RuntimeError('Claude Agent session queue is full', 'BUSY')
    checkAbort(signal)
    this.#queued++
    const previous = this.#tail
    const release = Promise.withResolvers()
    this.#tail = previous.then(() => release.promise)
    let acquired = false
    try {
      await abortable(previous, signal)
      acquired = true
      checkAbort(signal)
      return await run()
    } finally {
      this.#queued--
      // A cancelled waiter must not let its successor overtake the current owner.
      if (acquired) release.resolve()
      else previous.finally(() => release.resolve())
    }
  }
}

export function errorJSON(error) {
  return {
    message: String(error?.message ?? error ?? 'Claude Agent failed').slice(0, 16_384),
    ...(typeof error?.code === 'string' ? { code: error.code.slice(0, 128) } : {}),
  }
}
