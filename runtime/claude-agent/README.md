# Claude Agent native runtime

Internal Node >=22 runtime for OpenProxy's **Claude Agent** provider. An installed,
original `claude` executable owns authentication, its native transcript, retries,
and all Anthropic HTTP. OpenCode remains authoritative for history and executes
every offered tool. This runtime neither reads login files nor implements OAuth,
HTTP interception, subscription tokens, or a server-side agent/tool loop.

## Install and run

```sh
pnpm --dir runtime/claude-agent install --frozen-lockfile --no-optional
CLAUDE_CONFIG_DIR=/absolute/parent-owned/persistent/profile \
OPENPROXY_CLAUDE_PATH=/absolute/path/to/claude \
node runtime/claude-agent/bridge.mjs
```

The parent must supply `CLAUDE_CONFIG_DIR`; the runtime deliberately refuses to
fall back to a host user's profile. `OPENPROXY_CLAUDE_PATH` defaults to `claude`
on `PATH`. Every SDK query **explicitly** passes that executable path; the SDK's
optional bundled binary is never selected. The lock pins official SDK **0.3.301**
(its manifest's `claudeCodeVersion` is **2.1.301**) and official MCP server **2.3.1**.
Chats require Claude >=2.1.248, matching the pinned provider's verbatim-input gate.
Native web login and the bridge process supervisor belong to the parent.

The working directory is a private `openproxy-workspace` beneath that profile,
not the OpenCode client's project. No server workspace tool is made available.
API-key, cloud-provider, gateway/header/body and shell effort/tool-timeout
overrides are cleared for this single personal-subscription profile.

## Internal NDJSON

One JSON object per newline on stdin; stdout contains protocol objects **only**.
IDs must be unique among in-flight commands (string or nonnegative safe integer).
Commands are dispatched concurrently; native turns are serialized per session.

```json
{"op":"probe","id":"inventory"}
{"op":"run","id":"step-1","request":{"model":"sonnet","kind":"primary","sessionID":"already-key-scoped-hash","system":{"agent":"","instructions":""},"prompt":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"tools":[]}}
{"op":"idle","id":"terminal-event","sessionID":"already-key-scoped-hash"}
{"op":"cancel","id":"stop","targetId":"step-1"}
{"op":"shutdown","id":"shutdown"}
```

- `probe` initializes the SDK and optionally requests per-model context capacity
  using controls, **without ever submitting a user prompt**. It returns
  `{id,type:"result",result:{version,account,models}}`, then `done`. Account fields
  are whitelisted safe metadata, never token values or unknown native fields.
- `run.request` accepts `model`, `kind` (`primary`, `title`, `generate`,
  `compaction`), optional `sessionID`/`sourceID`, `system:{agent,instructions}`,
  LMV3 `prompt`/`tools`, optional `effort` and explicit `maxOutputTokens`.
  Arbitrary SDK, executable, environment, URL, header and connection overrides
  are rejected. Binary attachments are base64 strings; HTTP(S) URL attachments
  are strings. Keep tool `providerOptions`/`providerMetadata` intact, especially
  `claude-agent.toolError` and `claude-agent.toolAborted`.
- Replies are `{id,type:"part",part:<LMV3 stream part>}` followed by one `done`.
  A native process's first finish part includes safe refreshed inventory at
  `part.providerMetadata["claude-agent"].inventory`. The parent should merge this
  into its catalog, retaining previously probed model context windows when the
  startup inventory omits them. This avoids extra top-level result frames on
  run streams. Native inference errors are LMV3 error parts; bridge/protocol
  errors are `{id,type:"error",error:{message,code?}}`, followed by one `done`.
- A tool-call finish ends an **OpenCode step**, not the native turn. Leave the
  bridge alive and submit the next `run` with the same primary session, including
  the exact tool-call IDs and tool results in the prompt. Do **not** cancel on a
  normal tool-step EOF. Unoffered native tools are ignored; Claude handles its
  own `No such tool available` error. Tools cannot fall back to built-ins.
- `idle` acknowledges `done`; it starts the five-minute timer only when tools
  are parked and OpenCode has explicitly stopped executing the session. Active
  tools otherwise have **no five-minute timeout**. Settled replies automatically
  arm the timer. `cancel` interrupts/drains its target request (including a parked
  last step); `sessionID` is also accepted instead of `targetId`. It acknowledges
  only after cancellation/drain. `shutdown` drains/reaps every owned process.
- The parent authenticates before parsing public bodies, scopes session/source
  IDs to the authenticated key, and forwards normal OpenCode terminal execution
  events as `idle`. No authentication or generic public server lives here.

## Persistence and bounds

Primary calls with a session ID use native persisted sessions. Native transcript
UUIDs plus fingerprints reconcile model switches, missing history, context-hook
changes, edits/undo and native client forks. An interrupted reply remains owned
by the drained native transcript, not a possibly different client partial copy.
Titles are standalone. Generate/compaction use a disposable verified-prefix fork;
compaction first drains/stops the primary process, not its authoritative history.
Side-call processes do not save a transcript and their copied native branch is
deleted afterwards. There is no second generation retry loop.

`openproxy-checkpoints.json` in the profile is only an atomic hash/UUID cache,
not a message database: at most 512 records, 8,192 checkpoints per record, 16 MiB
total. Evicted/oversized records safely rebuild from client history. Native
transcripts are owned/retained by Claude. Runtime limits are eight live native
processes (including probes/side calls), 64 inference requests, 512 session
entries, 256 tools/pending tool IDs, 32 MiB protocol/native frames (plus 4 KiB IPC envelope), and 32 MiB
bounded writes/event queues. Idle settled processes can be reclaimed to free a
slot; executing/parked tools are never capacity-evicted. Startup is bounded at
30 seconds. Interrupts wait for the native result/transcript drain, not merely
the interrupt receipt; failed drains close/reap the native child before parked
MCP handlers are released. Stalled protocol writes fail within 30 seconds.
Native stderr and SDK debug capture are not relayed or logged.

## Tests

```sh
pnpm --dir runtime/claude-agent test
```

Tests use the **actual official SDK and MCP server**, with fake native stdio
transports, plus an actual `node bridge.mjs` child running a fake native CLI.
They use only isolated `/tmp/opencode/claude-runtime-test-*` profiles and never
inspect a host/production login. The simulated clock tests >5-minute tool waits
without waiting in real time. This is not evidence of a real subscription login
or an inference smoke; those require the owner's separate native sign-in.

See [UPSTREAM.md](UPSTREAM.md) for source/behavior attribution and licensing.
