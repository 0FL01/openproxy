import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { once } from "node:events"
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { createServer } from "node:http"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { promisify } from "node:util"
import { test } from "node:test"

function parseModels(stdout) {
  // Verbose output repeats a model ID line followed by a complete JSON object.
  // Split at those headers, rather than parsing the remainder as one record.
  const records = stdout.trim().split(/^ludka2\/([^\r\n]+)\r?$/m)
  assert.ok(records.length > 1, stdout)
  assert.equal(records[0].trim(), "", stdout)
  const models = new Map()
  for (let index = 1; index < records.length; index += 2) {
    const id = records[index]
    const model = JSON.parse(records[index + 1])
    assert.equal(model.api.id, id)
    assert.equal(models.has(id), false, `duplicate model ${id}`)
    models.set(id, model)
  }
  return models
}

test("OpenCode auto-loads inventory and A6API effort defaults without rewriting JSONC", {
  skip: !process.env.OPENCODE_BINARY,
  timeout: 220000,
}, async (t) => {
  const binary = process.env.OPENCODE_BINARY
  assert.ok(binary, "OPENCODE_BINARY must point to an existing executable")
  const directory = await mkdtemp(join(tmpdir(), "openproxy-opencode-"))
  t.after(() => rm(directory, { recursive: true, force: true }))
  const configDir = join(directory, "config", "opencode")
  await mkdir(join(configDir, "plugins"), { recursive: true })
  await copyFile(new URL("../plugins/openproxy-models.js", import.meta.url), join(configDir, "plugins", "openproxy-models.js"))
  await copyFile(new URL("../plugins/openproxy-claude-mask.js", import.meta.url), join(configDir, "plugins", "openproxy-claude-mask.js"))
  await copyFile(new URL("../plugins/openproxy-tui.js", import.meta.url), join(configDir, "plugins", "openproxy-tui.mjs"))
  let data = [{
    id: "cx/first",
    opencode: {
      name: "Fixture model",
      source: "codex",
      limit: { context: 628000, input: 450000, output: 128000 },
      modalities: { input: ["text", "image"], output: ["text"] },
      attachment: true,
      reasoning: true,
      variants: { high: { reasoningEffort: "high" } },
    },
  }]
  let discoveries = 0
  const server = createServer((req, res) => {
    res.setHeader("Content-Type", "application/json")
    if (req.url === "/api.json") { res.end("{}"); return }
    assert.equal(req.url, "/v1/models")
    assert.equal(req.headers.authorization, "Bearer fixture-key")
    discoveries++
    res.end(JSON.stringify({ object: "list", data }))
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
  await writeFile(join(configDir, "tui.json"), JSON.stringify({
    $schema: "https://opencode.ai/tui.json", plugin: ["./plugins/openproxy-tui.mjs"],
  }))
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
  const first = await run(binary, ["models", "ludka2", "--refresh"], { cwd: directory, env, timeout: 50000 })
  assert.match(first.stdout, /ludka2\/cx\/first/)
  data[0].id = "cx/added"
  const second = await run(binary, ["models", "ludka2", "--refresh", "--verbose"], { cwd: directory, env, timeout: 50000 })
  assert.ok(!second.stdout.includes("ludka2/cx/first"))
  const models = parseModels(second.stdout)
  assert.equal(models.size, 1)
  const model = models.get("cx/added")
  assert.ok(model, second.stdout)
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
  data = [{ id: "cx/gpt-5.5", opencode: { source: "codex", limit: { context: 400000 } } }]
  const resolved = await run(binary, ["debug", "config"], { cwd: directory, env, timeout: 50000 })
  const discovered = JSON.parse(resolved.stdout).provider.ludka2.models["cx/gpt-5.5"]
  assert.ok(discovered, "context-only model must remain discoverable")
  assert.equal(discovered.name, "GPT-5.5 · codex")
  assert.equal(Object.hasOwn(discovered, "limit"), false)
  assert.ok(!resolved.stderr.includes("[openproxy-models]"), "partial limits must not emit plugin warnings")
  assert.equal(discoveries, 3)

  const a6Efforts = [
    ["my-a6/gpt-6-luna", ["low", "medium", "high", "xhigh", "max"]],
    ["a6api/glm-5.3", ["low", "high", "max"]],
    ["a6api/deepseek-v4.1-flash", ["low", "high", "max"]],
  ]
  data = a6Efforts.map(([id, efforts]) => ({
    id, opencode: {
      source: "a6api", reasoning: true,
      variants: Object.fromEntries(efforts.map((effort) => [effort, { reasoningEffort: effort }])),
    },
  }))
  const a6 = await run(binary, ["models", "ludka2", "--refresh", "--verbose"], { cwd: directory, env, timeout: 50000 })
  const activeModels = parseModels(a6.stdout)
  assert.deepEqual([...activeModels.keys()].sort(), a6Efforts.map(([id]) => id).sort())
  for (const [id, efforts] of a6Efforts) {
    const active = activeModels.get(id)
    assert.equal(active.api.npm, "@ai-sdk/openai")
    assert.ok(active.name.endsWith(" · a6api"), active.name)
    assert.equal(active.capabilities.reasoning, true)
    assert.deepEqual(Object.keys(active.variants).sort(), [...efforts].sort(), id)
    for (const effort of efforts) assert.equal(active.variants[effort].reasoningEffort, effort, `${id}: ${effort}`)
  }
  assert.equal(discoveries, 4)
  assert.equal(await readFile(configPath, "utf8"), config)
})
