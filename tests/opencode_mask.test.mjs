import assert from "node:assert/strict"
import { test } from "node:test"
import discovery from "../plugins/openproxy-models.js"

const OpenProxyModels = discovery.server

function stubInput() {
  return {
    client: {},
    project: { id: "p" },
    directory: "/tmp/x",
    worktree: "/tmp/x",
    serverUrl: new URL("http://127.0.0.1:4096"),
    $: undefined,
  }
}

const maskCases = () => [
  {
    label: "mask replaces the system prompt wholesale for ludka2",
    hook: "experimental.chat.system.transform",
    input: {
      sessionID: "s1",
      model: { providerID: "ludka2", modelID: "claude-sonnet-4-5" },
    },
    output: { system: ["You are opencode, an interactive CLI tool that helps users with software engineering tasks.", "extra block"] },
    check: (output) => {
      assert.equal(output.system.length, 3)
      assert.equal(output.system[0], "You are Claude Code, Anthropic's official CLI for Claude.")
      assert.ok(output.system[1].includes("interactive agent"))
      assert.ok(output.system[2].includes("# Memory"))
      const joined = output.system.join("\n").toLowerCase()
      assert.ok(!joined.includes("opencode"), "client identity must not survive")
    },
  },
  {
    label: "mask leaves other providers' system prompts untouched",
    hook: "experimental.chat.system.transform",
    input: {
      sessionID: "s1",
      model: { providerID: "anthropic", modelID: "claude-sonnet-4-5" },
    },
    output: { system: ["You are opencode, an interactive CLI tool."] },
    check: (output) => {
      assert.deepEqual(output.system, ["You are opencode, an interactive CLI tool."])
    },
  },
  {
    label: "mask pins the claude-cli User-Agent for ludka2",
    hook: "chat.headers",
    input: {
      sessionID: "s1",
      model: { providerID: "ludka2", modelID: "claude-sonnet-4-5" },
    },
    output: { headers: { "User-Agent": "opencode/1.18.31" } },
    check: (output) => {
      assert.equal(output.headers["User-Agent"], "claude-cli/2.1.289 (external, cli)")
    },
  },
  {
    label: "mask leaves other providers' headers untouched",
    hook: "chat.headers",
    input: {
      sessionID: "s1",
      model: { providerID: "anthropic", modelID: "claude-sonnet-4-5" },
    },
    output: { headers: { "User-Agent": "opencode/1.18.31" } },
    check: (output) => {
      assert.equal(output.headers["User-Agent"], "opencode/1.18.31")
    },
  },
  {
    label: "tool.definition scrubs client identity strings",
    hook: "tool.definition",
    input: { toolID: "bash" },
    output: {
      description: "Executes a bash command. Part of opencode; see https://opencode.ai/docs and https://github.com/anomalyco/opencode/issues.",
      parameters: {},
    },
    check: (output) => {
      const lower = output.description.toLowerCase()
      assert.ok(!lower.includes("opencode"), `scrubbed: ${output.description}`)
      assert.ok(!lower.includes("anomalyco"), `scrubbed: ${output.description}`)
      assert.ok(output.description.includes("Executes a bash command"))
    },
  },
  {
    label: "tool.definition leaves clean descriptions untouched",
    hook: "tool.definition",
    input: { toolID: "bash" },
    output: { description: "Executes a bash command and returns its output.", parameters: {} },
    check: (output) => {
      assert.equal(output.description, "Executes a bash command and returns its output.")
    },
  },
]

test("server module exposes the claude mask hooks", async () => {
  const hooks = await OpenProxyModels(stubInput())
  assert.equal(typeof hooks["chat.headers"], "function")
  assert.equal(typeof hooks["experimental.chat.system.transform"], "function")
  assert.equal(typeof hooks["tool.definition"], "function")
})

for (const testCase of maskCases()) {
  test(testCase.label, async () => {
    const hooks = await OpenProxyModels(stubInput())
    const hook = hooks[testCase.hook]
    assert.equal(typeof hook, "function", `${testCase.hook} present`)
    await hook(testCase.input, testCase.output)
    testCase.check(testCase.output)
  })
}

test("mask system prompt carries no client identity markers", async () => {
  const hooks = await OpenProxyModels(stubInput())
  const output = { system: ["replaced"] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "s", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  const joined = output.system.join("\n").toLowerCase()
  for (const marker of ["opencode", "anomalyco", "opencode.ai", "sst/"]) {
    assert.ok(!joined.includes(marker), `marker ${marker} must not appear`)
  }
  // Provider-context gating also works when only provider.info.id is present.
  const output2 = { system: ["original"] }
  await hooks["experimental.chat.system.transform"](
    {
      sessionID: "s",
      agent: "build",
      model: { providerID: "other", modelID: "x" },
      provider: { info: { id: "ludka2" }, options: {} },
    },
    output2,
  )
  assert.equal(output2.system[0], "You are Claude Code, Anthropic's official CLI for Claude.")
})
