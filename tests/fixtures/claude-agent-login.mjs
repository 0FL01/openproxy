#!/usr/bin/env node
// Synthetic native-command boundary; no real login or credential files.
import fs from "node:fs"
import path from "node:path"
import readline from "node:readline"

const profile = process.env.CLAUDE_CONFIG_DIR
const marker = path.join(profile, "synthetic-native-login")
if (process.argv[2] !== "auth" || !profile || process.env.HOME !== path.join(profile, "home")) process.exit(2)
if (["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "GITHUB_TOKEN", "OPENPROXY_API_KEY"].some((name) => process.env[name])) process.exit(2)
if (process.argv[3] === "status") {
  const loggedIn = fs.existsSync(marker)
  console.log(JSON.stringify({ loggedIn, authMethod: loggedIn ? "claude.ai" : "none" }))
  process.exit(loggedIn ? 0 : 1)
}
if (process.argv[3] !== "login" || !process.stdin.isTTY || !process.stdout.isTTY) process.exit(2)
fs.writeFileSync(path.join(profile, "synthetic-native-pid"), String(process.pid))
console.log("Open https://claude.ai/oauth/authorize?state=synthetic-native-state")
console.log("Paste code here if prompted:")
const input = readline.createInterface({ input: process.stdin, terminal: false })
input.once("line", (code) => {
  if (code !== "synthetic-native-code") process.exit(3)
  fs.writeFileSync(marker, "signed in", { mode: 0o600 })
  console.log("Login successful")
  process.exit(0)
})
