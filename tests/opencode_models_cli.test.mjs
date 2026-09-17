import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { once } from "node:events"
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { createServer } from "node:http"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { promisify } from "node:util"
import { test } from "node:test"

test("OpenCode auto-loads the plugin and refresh discovers new IDs without rewriting JSONC", {
  skip: !process.env.OPENCODE_BINARY,
  timeout: 120000,
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "openproxy-opencode-"))
  t.after(() => rm(directory, { recursive: true, force: true }))
  const configDir = join(directory, "config", "opencode")
  await mkdir(join(configDir, "plugins"), { recursive: true })
  await copyFile(new URL("../plugins/openproxy-models.js", import.meta.url), join(configDir, "plugins", "openproxy-models.js"))
  let id = "cx/first"
  let limit = { context: 628000, input: 450000, output: 128000 }
  let name = "Fixture model"
  let discoveries = 0
  const server = createServer((req, res) => {
    res.setHeader("Content-Type", "application/json")
    if (req.url === "/api.json") { res.end("{}"); return }
    assert.equal(req.url, "/v1/models")
    assert.equal(req.headers.authorization, "Bearer fixture-key")
    discoveries++
    res.end(JSON.stringify({ object: "list", data: [{
      id,
      opencode: {
        ...(name ? { name } : {}),
        source: "codex",
        limit,
        modalities: { input: ["text", "image"], output: ["text"] },
        attachment: true,
        reasoning: true,
        variants: { high: { reasoningEffort: "high" } },
      },
    }] }))
  }).listen(0, "127.0.0.1")
  await once(server, "listening")
  t.after(() => { server.closeAllConnections(); server.close() })
  const origin = `http://127.0.0.1:${server.address().port}`
  const configPath = join(configDir, "opencode.jsonc")
  const config = `// Keep this comment and do not generate models on disk.\n${JSON.stringify({
    $schema: "https://opencode.ai/config.json",
    provider: { ludka2: { npm: "@ai-sdk/openai", options: {
      baseURL: "{env:LUDKA2_API_URL}", apiKey: "{env:LUDKA2_API_KEY}", timeout: false,
    } } },
  }, null, 2)}\n`
  await writeFile(configPath, config)
  const env = {
    PATH: process.env.PATH,
    HOME: directory,
    XDG_CONFIG_HOME: join(directory, "config"),
    XDG_CACHE_HOME: join(directory, "cache"),
    XDG_DATA_HOME: join(directory, "data"),
    XDG_STATE_HOME: join(directory, "state"),
    OPENCODE_DISABLE_PROJECT_CONFIG: "true",
    OPENCODE_DISABLE_DEFAULT_PLUGINS: "true",
    OPENCODE_DISABLE_EXTERNAL_SKILLS: "true",
    OPENCODE_DISABLE_CLAUDE_CODE: "true",
    OPENCODE_DISABLE_AUTOUPDATE: "true",
    OPENCODE_DISABLE_MODELS_FETCH: "true",
    OPENCODE_MODELS_URL: origin,
    LUDKA2_API_URL: `${origin}/v1`,
    LUDKA2_API_KEY: "fixture-key",
  }
  const run = promisify(execFile)
  const first = await run(process.env.OPENCODE_BINARY, ["models", "ludka2", "--refresh"], { cwd: directory, env, timeout: 50000 })
  assert.match(first.stdout, /ludka2\/cx\/first/)
  id = "cx/added"
  const second = await run(process.env.OPENCODE_BINARY, ["models", "ludka2", "--refresh", "--verbose"], { cwd: directory, env, timeout: 50000 })
  assert.ok(!second.stdout.includes("ludka2/cx/first"))
  const modelOutput = second.stdout.split("ludka2/cx/added\n")[1]
  assert.ok(modelOutput, second.stdout)
  const model = JSON.parse(modelOutput)
  assert.equal(model.api.id, "cx/added")
  assert.equal(model.api.npm, "@ai-sdk/openai")
  assert.equal(model.name, "Fixture model · codex")
  assert.deepEqual(model.limit, { context: 500000, input: 450000, output: 128000 })
  assert.equal(model.capabilities.attachment, true)
  assert.equal(model.capabilities.input.image, true)
  assert.equal(model.variants.high.reasoningEffort, "high")
  assert.deepEqual(Object.keys(model.variants), ["high"])
  assert.equal(discoveries, 2)
  // Codex discovery can supply context without an output limit. The config
  // consumed by clients must not contain a partial OpenCode limit object.
  id = "cx/gpt-5.5"
  name = undefined
  limit = { context: 400000 }
  const resolved = await run(process.env.OPENCODE_BINARY, ["debug", "config"], { cwd: directory, env, timeout: 50000 })
  const discovered = JSON.parse(resolved.stdout).provider.ludka2.models[id]
  assert.ok(discovered, "context-only model must remain discoverable")
  assert.equal(discovered.name, "GPT-5.5 · codex")
  assert.equal(Object.hasOwn(discovered, "limit"), false)
  assert.equal(discoveries, 3)
  assert.equal(await readFile(configPath, "utf8"), config)
})
