// Native login is an I/O relay, not an OAuth implementation. Nothing here
// interprets terminal text as authentication state or stores the transcript.
export const NATIVE_LOGIN_POLL_MS = 500;
export const NATIVE_LOGIN_OUTPUT_LIMIT = 64 * 1024;

export interface NativeLoginState {
  status: "ready" | "starting" | "pending" | "success" | "error" | "cancelled";
  output: string;
  truncated: boolean;
  error?: string;
  connectionId?: string;
}

interface Attempt {
  id?: string;
  closed: boolean;
  terminal: boolean;
  controller: AbortController;
  timer?: ReturnType<typeof setTimeout>;
}

export const nativeLoginInitialState = (): NativeLoginState => ({ status: "ready", output: "", truncated: false });

export function nativeLoginLinks(output: string): string[] {
  const result = new Set<string>();
  for (const match of output.matchAll(/https:\/\/[^\s<>"'\x00-\x1f\x7f]+/g)) {
    try {
      const url = new URL(match[0]);
      if (url.protocol === "https:" && url.hostname && !url.username && !url.password) result.add(match[0]);
    } catch { /* Native output may contain incomplete URLs while polling. */ }
    if (result.size === 8) break;
  }
  return [...result];
}

export class ClaudeNativeLogin {
  private attempt?: Attempt;
  private state = nativeLoginInitialState();
  private readonly onState: (state: NativeLoginState) => void;
  private readonly onSuccess: (connectionId?: string) => Promise<void> | void;
  private readonly request: typeof fetch;

  constructor(options: {
    onState: (state: NativeLoginState) => void;
    onSuccess: (connectionId?: string) => Promise<void> | void;
    request?: typeof fetch;
  }) {
    this.onState = options.onState;
    this.onSuccess = options.onSuccess;
    this.request = options.request ?? fetch;
  }

  private current(attempt: Attempt) {
    return this.attempt === attempt && !attempt.closed;
  }

  private publish(attempt: Attempt, state: NativeLoginState) {
    if (!this.current(attempt)) return;
    this.state = state;
    this.onState(state);
  }

  private path(attempt: Attempt) {
    return `/api/claude-agent/login/${encodeURIComponent(attempt.id!)}`;
  }

  private async cancel(attempt: Attempt) {
    if (!attempt.id || attempt.terminal) return;
    // Always address this exact attempt. Never cancel a profile or whichever
    // attempt happens to be current when a delayed request resolves.
    try {
      await this.request(this.path(attempt), {
        method: "DELETE", credentials: "same-origin", redirect: "error", keepalive: true,
      });
    } catch { /* The server also owns a bounded login deadline. */ }
  }

  close() {
    const attempt = this.attempt;
    if (!attempt || attempt.closed) return;
    attempt.closed = true;
    clearTimeout(attempt.timer);
    attempt.controller.abort();
    void this.cancel(attempt);
  }

  async start(name: string) {
    this.close();
    const attempt: Attempt = { closed: false, terminal: false, controller: new AbortController() };
    this.attempt = attempt;
    this.publish(attempt, { ...nativeLoginInitialState(), status: "starting" });
    try {
      // Do not abort creation on modal close: consume the returned ID so even
      // an attempt created after unmount can be cancelled by its own ID.
      const response = await this.request("/api/claude-agent/login", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify(name.trim() ? { name: name.trim() } : {}),
        // Account changes first drain any existing native conversation process.
        credentials: "same-origin", redirect: "error", signal: AbortSignal.timeout(120000),
      });
      if (!response.ok) throw new Error("Could not start native Claude login. Try again.");
      const data = await response.json();
      if (typeof data.attemptId !== "string" || !data.attemptId) throw new Error("Invalid native login response.");
      attempt.id = data.attemptId;
      if (!this.current(attempt)) { await this.cancel(attempt); return; }
      this.publish(attempt, { ...this.state, status: "pending" });
      await this.poll(attempt);
    } catch (error) {
      if (this.current(attempt)) this.fail(attempt, error instanceof Error && !["TypeError", "TimeoutError"].includes(error.name)
        ? error.message : "Native login is unavailable. Check the connection and try again.");
    }
  }

  private fail(attempt: Attempt, message: string) {
    void this.cancel(attempt);
    attempt.terminal = true;
    clearTimeout(attempt.timer);
    this.publish(attempt, { ...this.state, status: "error", error: message.slice(0, 4096) });
  }

  private async poll(attempt: Attempt) {
    if (!this.current(attempt) || attempt.terminal) return;
    try {
      const response = await this.request(this.path(attempt), {
        cache: "no-store", credentials: "same-origin", redirect: "error",
        signal: AbortSignal.any([attempt.controller.signal, AbortSignal.timeout(15000)]),
      });
      if (!response.ok) throw new Error("Could not read native login status. Close and try again.");
      const data = await response.json();
      if (!this.current(attempt)) return;
      if (!["pending", "success", "error", "cancelled"].includes(data.status) || typeof data.output !== "string") {
        throw new Error("Invalid native login status response.");
      }
      attempt.terminal = data.status !== "pending";
      this.publish(attempt, {
        status: data.status,
        output: data.output.slice(-NATIVE_LOGIN_OUTPUT_LIMIT),
        truncated: data.output.length > NATIVE_LOGIN_OUTPUT_LIMIT,
        ...(typeof data.error === "string" ? { error: data.error.slice(0, 4096) } : {}),
        ...(typeof data.connectionId === "string" ? { connectionId: data.connectionId } : {}),
      });
      if (data.status === "success") {
        try { await this.onSuccess(data.connectionId); }
        catch {
          this.publish(attempt, { ...this.state, error: "Login succeeded, but refreshing connections/models failed. Refresh the provider page." });
        }
      } else if (!attempt.terminal) {
        attempt.timer = setTimeout(() => { void this.poll(attempt); }, NATIVE_LOGIN_POLL_MS);
      }
    } catch (error) {
      if (this.current(attempt)) this.fail(attempt, error instanceof Error && error.name !== "TypeError"
        ? error.message : "Could not read native login status. Check the connection and try again.");
    }
  }

  async input(input: string): Promise<boolean> {
    const attempt = this.attempt;
    if (!attempt || !this.current(attempt) || attempt.terminal || !attempt.id) return false;
    try {
      const response = await this.request(`${this.path(attempt)}/input`, {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ input }),
        credentials: "same-origin", redirect: "error",
        signal: AbortSignal.any([attempt.controller.signal, AbortSignal.timeout(15000)]),
      });
      if (!this.current(attempt)) return false;
      if (!response.ok) throw new Error("Native login input was not accepted. Check the native output before retrying.");
      return true;
    } catch {
      if (this.current(attempt) && !attempt.terminal) this.publish(attempt, {
        ...this.state, error: "Could not send native login input. Check the connection before retrying.",
      });
      return false;
    }
  }
}
