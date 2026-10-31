// Tests the Rust IPC/HTTP boundary only. SDK behavior has its own real-SDK tests.
import readline from "node:readline"
import fs from "node:fs"
import { validateRequest } from "../../runtime/claude-agent/lib/protocol.mjs"
const lines = readline.createInterface({ input: process.stdin })
function emit(event) { process.stdout.write(`${JSON.stringify(event)}\n`) }
for await (const line of lines) {
  const command = JSON.parse(line)
  const { id, op } = command
  if (op === "shutdown") process.exit(0)
  const hold = `${process.env.CLAUDE_CONFIG_DIR}/hold-native-probe`
  if (op === "probe" && fs.existsSync(hold)) {
    fs.writeFileSync(`${process.env.CLAUDE_CONFIG_DIR}/native-probe-started`, "started")
    await new Promise((resolve) => {
      const watcher = fs.watch(process.env.CLAUDE_CONFIG_DIR, () => {
        if (!fs.existsSync(hold)) { watcher.close(); resolve() }
      })
      if (!fs.existsSync(hold)) { watcher.close(); resolve() }
    })
  }
  if (op === "probe") emit({ id, type: "result", result: {
    version: "2.1.301",
    account: { email: "synthetic@example.invalid", subscriptionType: "pro", apiProvider: "firstParty", tokenSource: "native" },
    models: [{ value: "opus", displayName: "Native Opus", contextWindow: 1000000, supportedEffortLevels: ["low", "max"] }],
  } })
  if (op === "run") {
    validateRequest(command.request)
    emit({ id, type: "part", part: { type: "stream-start", warnings: [] } })
    emit({ id, type: "part", part: { type: "text-start", id: "answer" } })
    emit({ id, type: "part", part: { type: "text-delta", id: "answer", delta: command.request.sessionID } })
    emit({ id, type: "part", part: { type: "text-end", id: "answer" } })
    emit({ id, type: "part", part: { type: "finish", finishReason: { unified: "stop", raw: "end_turn" }, usage: { inputTokens: { total: 1 }, outputTokens: { total: 1 } },
      providerMetadata: { "claude-agent": { inventory: { version: "2.1.301", account: { email: "synthetic@example.invalid", subscriptionType: "pro", apiProvider: "firstParty", tokenSource: "native" },
        models: [{ value: "opus", displayName: "Native Opus", supportedEffortLevels: ["low", "max"] }],
      } } },
    } })
  }
  emit({ id, type: "done" })
}
