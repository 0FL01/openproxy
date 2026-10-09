import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { once } from "node:events"
import { copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { createServer } from "node:http"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { promisify } from "node:util"
import { test } from "node:test"

test("installed OpenCode TUI supplies lazy UI imports and actually renders reactive usage rows", {
  skip: !process.env.OPENCODE_BINARY, timeout: 60000,
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "openproxy-tui-"))
  t.after(() => rm(directory, { recursive: true, force: true }))
  const configDir = join(directory, "config", "opencode")
  await mkdir(join(configDir, "plugins"), { recursive: true })
  await copyFile(new URL("../plugins/openproxy-models.js", import.meta.url), join(configDir, "plugins", "openproxy-models.js"))
  await copyFile(new URL("../plugins/openproxy-claude-mask.js", import.meta.url), join(configDir, "plugins", "openproxy-claude-mask.js"))
  await copyFile(new URL("../plugins/openproxy-tui.js", import.meta.url), join(configDir, "plugins", "openproxy-tui.mjs"))
  let requests = 0
  const server = createServer((req, res) => {
    res.setHeader("Content-Type", "application/json")
    if (req.url === "/api.json") { res.end("{}"); return }
    assert.equal(req.headers.authorization, "Bearer fixture-key")
    if (req.url === "/v1/models") {
      res.end(JSON.stringify({ object: "list", data: [{ id: "fixture" }] })); return
    }
    assert.equal(req.url, "/v1/usage/limits")
    requests++
    res.end(JSON.stringify({ refreshIntervalSeconds: 180, accounts: [{
      id: "fixture", provider: "codex", label: "Fixture account", plan: "Pro", error: null,
       observedAt: new Date(Date.now() - 3600000).toISOString(), status: requests === 1 ? "loading" : "stale",
       refreshing: requests > 1, nextRefreshAt: null, errorStatus: null,
      quotas: requests === 1 ? {} : { Session: {
        used: null, total: null, remaining: null, remainingPercentage: 20,
        resetAt: new Date(Date.now() + 3600000).toISOString(), unlimited: false,
      } },
    }, {
      id: "second", provider: "codex", label: "Second fixture", plan: "Pro", error: null,
       observedAt: new Date(Date.now() - 3600000).toISOString(), status: requests === 1 ? "loading" : "stale",
       refreshing: false, nextRefreshAt: null, errorStatus: null,
      quotas: requests === 1 ? {} : { Session: {
        used: null, total: null, remaining: null, remainingPercentage: 80,
        resetAt: new Date(Date.now() + 7200000).toISOString(), unlimited: false,
      } },
    }, {
      id: "a6", provider: "a6api", label: "A6 fixture", plan: null, error: null,
      observedAt: new Date(Date.now() - 3600000).toISOString(), status: requests === 1 ? "loading" : "stale",
      refreshing: false, nextRefreshAt: null, errorStatus: null,
      quotas: requests === 1 ? {} : { "API credits": {
        used: 2, total: 3, remaining: 1.00283, remainingPercentage: 1.00283 / 3 * 100,
        resetAt: new Date(Date.now() + 60 * 3600000).toISOString(), unit: "USD", unlimited: false,
      } },
    }] }))
  }).listen(0, "127.0.0.1")
  await once(server, "listening")
  t.after(() => { server.closeAllConnections(); server.close() })
  const origin = `http://127.0.0.1:${server.address().port}`
  await writeFile(join(configDir, "opencode.json"), JSON.stringify({
    $schema: "https://opencode.ai/config.json", model: "ludka2/fixture",
    provider: { ludka2: { npm: "@ai-sdk/openai", options: { baseURL: `${origin}/v1`, apiKey: "fixture-key" } } },
  }))
  // A test route renders exactly the registered sidebar slot in the real host,
  // avoiding inference or dependence on the user's session/sidebar preferences.
  await writeFile(join(configDir, "probe.mjs"), `import sidebar from "./plugins/openproxy-tui.mjs"
    export default { id: "fixture.sidebar", tui: async (api) => {
      let slot
      await sidebar.tui({ ...api, slots: { register(value) { slot = value; return api.slots.register(value) } } })
      api.route.register([{ name: "fixture", render: () => slot.slots.sidebar_content({ theme: api.theme }) }])
      const timer = setTimeout(() => api.route.navigate("fixture"), 1000)
      api.lifecycle.onDispose(() => clearTimeout(timer))
    } }`)
  await writeFile(join(configDir, "tui.json"), JSON.stringify({
    $schema: "https://opencode.ai/tui.json", plugin: ["./probe.mjs"],
  }))
  const env = {
    PATH: process.env.PATH, HOME: directory, TERM: "xterm-256color",
    XDG_CONFIG_HOME: join(directory, "config"), XDG_CACHE_HOME: join(directory, "cache"),
    XDG_DATA_HOME: join(directory, "data"), XDG_STATE_HOME: join(directory, "state"),
    OPENCODE_DISABLE_PROJECT_CONFIG: "true", OPENCODE_DISABLE_DEFAULT_PLUGINS: "true",
    OPENCODE_DISABLE_EXTERNAL_SKILLS: "true", OPENCODE_DISABLE_CLAUDE_CODE: "true",
    OPENCODE_DISABLE_AUTOUPDATE: "true", OPENCODE_DISABLE_MODELS_FETCH: "true", OPENCODE_MODELS_URL: origin,
  }
  const python = `import os, pty, subprocess, select, time, sys, fcntl, termios, struct, signal, re
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 60, 120, 0, 0))
child = subprocess.Popen([sys.argv[1]], stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
os.close(slave)
output = b""
deadline = time.monotonic() + 35
try:
 while time.monotonic() < deadline:
  if select.select([master], [], [], .2)[0]:
   try: data = os.read(master, 65536)
   except OSError: break
   if not data: break
   output += data
   rendered = re.sub(rb"\\x1b\\[[0-?]*[ -/]*[@-~]", b"", output)
   if (b"Codex" in rendered and b"50%" in rendered and b"A6API" in rendered
       and re.search(rb"Balance\\s*\\$1\\.00\\s*left", rendered)
       and re.search(rb"Expires\\s*2d12h", rendered)): break
finally:
 os.killpg(child.pid, signal.SIGTERM) if child.poll() is None else None
 try: child.wait(timeout=3)
 except subprocess.TimeoutExpired: os.killpg(child.pid, signal.SIGKILL); child.wait()
 os.close(master)
sys.stdout.buffer.write(output)
`
  const result = await promisify(execFile)("python3", ["-c", python, process.env.OPENCODE_BINARY], {
    cwd: directory, env, timeout: 45000, maxBuffer: 2 * 1024 * 1024,
  })
  const rendered = result.stdout.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
  assert.match(rendered, /Usage limits/)
  // Cursor moves can replace spaces between differently styled inline spans.
  assert.match(rendered, /Codex\s*·\s*Pro/)
  assert.match(rendered, /50%/)
  assert.match(rendered, /━{4}─{4}/)
  assert.match(rendered, /↻1h/)
  assert.match(rendered, /A6API/)
  assert.match(rendered, /Balance\s*\$1\.00\s*left/)
  assert.match(rendered, /Expires\s*2d12h/)
  assert.doesNotMatch(rendered, /API credits|1\.00283|━{5}─{3}|67%|↻2d12h/i)
  assert.doesNotMatch(rendered, /a6api|Fixture account|Second fixture|A6 fixture|updated just now|Data stale|retrying/)
  assert.equal(requests, 2)
})
