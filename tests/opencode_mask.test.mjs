import assert from "node:assert/strict"
import os from "node:os"
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
      assert.equal(output.system.length, 5)
      assert.equal(output.system[0], "You are Claude Code, Anthropic's official CLI for Claude.")
      assert.ok(output.system[1].includes("interactive agent"))
      assert.ok(output.system[2].includes("# Memory"))
      assert.ok(output.system[2].includes(`${os.homedir()}/.claude/projects/-tmp-x/memory/`))
      assert.ok(output.system[3].startsWith("# Environment"))
      assert.ok(output.system[3].includes("Primary working directory: /tmp/x"))
      assert.ok(output.system[3].includes(`Platform: ${process.platform}`))
      assert.ok(output.system[3].includes(`OS Version: ${os.type()} ${os.release()}`))
      assert.ok(output.system[4].startsWith("While bypass permissions mode is active:"))
      const joined = output.system.join("\n").toLowerCase()
      assert.ok(!joined.includes("opencode"), "client identity must not survive")
      assert.ok(!joined.includes("/home/user/"), "placeholder path must not survive")
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

function sessionClientStub(directory, fail = false) {
  return {
    session: {
      get() {
        if (fail) return Promise.reject(new Error("boom"))
        return Promise.resolve({ directory })
      },
    },
  }
}

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
  for (const marker of ["opencode", "anomalyco", "opencode.ai", "sst/", "/home/user/"]) {
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

test("mask resolves the session working directory through the client", async () => {
  const hooks = await OpenProxyModels({ ...stubInput(), client: sessionClientStub("/tmp/deep/dir/") })
  const output = { system: ["original"] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-1", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.ok(output.system[2].includes(`${os.homedir()}/.claude/projects/-tmp-deep-dir/memory/`))
  assert.ok(output.system[3].includes("Primary working directory: /tmp/deep/dir"))
})

test("mask falls back to the instance directory when the session lookup fails", async () => {
  const hooks = await OpenProxyModels({ ...stubInput(), client: sessionClientStub("/unreachable", true) })
  const output = { system: ["original"] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-2", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.ok(output.system[2].includes(`${os.homedir()}/.claude/projects/-tmp-x/memory/`))
  assert.ok(output.system[3].includes("Primary working directory: /tmp/x"))
})

test("plan agent sessions carry the claude code plan mode block", async () => {
  const hooks = await OpenProxyModels(stubInput())
  await hooks["chat.headers"](
    { sessionID: "sess-plan", agent: "plan", model: { providerID: "ludka2", modelID: "x" } },
    { headers: {} },
  )
  const output = { system: ["original"] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-plan", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.ok(output.system[4].startsWith("Plan mode is active."))
  assert.ok(output.system[4].includes("MUST NOT make any edits"))
  assert.ok(output.system[4].includes("ExitPlanMode"))
})

test("non-plan agent profiles keep the bypass permissions block", async () => {
  const hooks = await OpenProxyModels(stubInput())
  await hooks["chat.params"](
    { sessionID: "sess-build", agent: "build", model: { providerID: "ludka2", modelID: "x" }, provider: { info: { id: "ludka2" } }, message: {} },
    { temperature: 0, topP: 0, topK: 0, options: {} },
  )
  const output = { system: ["original"] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-build", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.ok(output.system[4].startsWith("While bypass permissions mode is active:"))
})

test("sessions without a remembered agent default to bypass permissions", async () => {
  const hooks = await OpenProxyModels(stubInput())
  const output = { system: ["original"] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-unknown", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.ok(output.system[4].startsWith("While bypass permissions mode is active:"))
})

test("project instructions ride along in the claude code wrapper", async () => {
  const hooks = await OpenProxyModels(stubInput())
  const output = {
    system: [
      [
        "You are opencode, an interactive CLI tool that helps users with software engineering tasks.",
        "<env>\n  Working directory: /tmp/x\n</env>",
        "Instructions from: /tmp/x/AGENTS.md\nAlways answer in haiku.\nRun tests before pushing.",
        "<mcp_instructions>\n  <server name=\"ctx\">\n    docs\n  </server>\n</mcp_instructions>",
        "Skills provide specialized instructions and workflows for specific tasks.",
      ].join("\n"),
    ],
  }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-agents", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.equal(output.system.length, 6)
  const block = output.system[5]
  assert.ok(block.startsWith("Codebase and user instructions are shown below."))
  assert.ok(block.includes("Contents of /tmp/x/AGENTS.md (project instructions, checked into the codebase):"))
  assert.ok(block.includes("Always answer in haiku.\nRun tests before pushing."))
  const joined = output.system.join("\n")
  assert.ok(!joined.includes("Instructions from:"), "opencode section marker must not survive")
  assert.ok(!joined.includes("<mcp_instructions>"), "mcp section must not leak into the block")
})

test("global config instructions display a claude code path", async () => {
  const hooks = await OpenProxyModels(stubInput())
  const globalPath = `${os.homedir()}/.config/opencode/AGENTS.md`
  const output = { system: [`agent prompt\nInstructions from: ${globalPath}\nBe terse.`] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-global", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  const block = output.system[5]
  assert.ok(block.includes(`Contents of ${os.homedir()}/.claude/AGENTS.md (user's private global instructions for all projects):`))
  assert.ok(block.includes("Be terse."))
  assert.ok(!output.system.join("\n").includes("/.config/opencode"), "client config path must not survive")
})

test("multiple instruction sections are packed under one preamble", async () => {
  const hooks = await OpenProxyModels(stubInput())
  const output = {
    system: [
      [
        "agent prompt",
        "Instructions from: /tmp/x/AGENTS.md\nproject rules",
        "Instructions from: /tmp/x/CONTEXT.md\nextra context",
      ].join("\n"),
    ],
  }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-multi", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  const block = output.system[5]
  assert.equal(block.match(/Codebase and user instructions are shown below\./g).length, 1)
  assert.ok(block.includes("Contents of /tmp/x/AGENTS.md"))
  assert.ok(block.includes("Contents of /tmp/x/CONTEXT.md"))
  assert.ok(block.includes("project rules"))
  assert.ok(block.includes("extra context"))
})

test("systems without instruction sections keep the five-block shape", async () => {
  const hooks = await OpenProxyModels(stubInput())
  const output = { system: ["You are opencode, an interactive CLI tool."] }
  await hooks["experimental.chat.system.transform"](
    { sessionID: "sess-plain", model: { providerID: "ludka2", modelID: "x" } },
    output,
  )
  assert.equal(output.system.length, 5)
})
