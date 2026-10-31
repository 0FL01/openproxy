# Pinned source and attribution

The native session/MCP/history/stream logic is a plain Node ESM port of
[anomalyco/opencode PR #58782](https://github.com/anomalyco/opencode/pull/58782),
at immutable commit **`f29e281322b35b525e337d12e84aedc7e129fd6d`**.
The original copyright/license notice is retained in
[`LICENSE.upstream`](LICENSE.upstream) (MIT, Copyright (c) 2025 opencode).
Do not substitute an earlier simplified `persistSession:false` chat design.

Source root:
`packages/core/src/plugin/provider/claude-agent/` at the pinned commit.
Public source and specification were read directly at these immutable URLs:

- [Frozen upstream behavior specification](https://github.com/anomalyco/opencode/blob/f29e281322b35b525e337d12e84aedc7e129fd6d/specs/v2/claude-agent-provider.md)
- [Provider source directory](https://github.com/anomalyco/opencode/tree/f29e281322b35b525e337d12e84aedc7e129fd6d/packages/core/src/plugin/provider/claude-agent)
- [Provider tests and native protocol recordings](https://github.com/anomalyco/opencode/tree/f29e281322b35b525e337d12e84aedc7e129fd6d/packages/core/test/plugin/provider/claude-agent)
- [Original MIT license](https://github.com/anomalyco/opencode/blob/f29e281322b35b525e337d12e84aedc7e129fd6d/LICENSE)

## Port map / behavior evidence

| Runtime file | Pinned upstream files and preserved behavior |
| --- | --- |
| `lib/native.mjs` | `claude/{query-options,probe,io,fork,sdk}.ts`: explicit original executable, settings isolation, no-prompt inventory/context controls, version gate, public SDK transcript existence/fork/delete, copied-prefix verification and UUID remapping |
| `lib/session.mjs` | `claude/session.ts`: one SDK iterator, persistent input stream, split steps vs turns, interrupt receipt vs terminal drain, partial transcript ownership, live model/effort controls |
| `lib/sessions.mjs` | `claude/sessions.ts`: per-session serialization, tool parking/continuations, changed-launch restarts, resume/missing-transcript recovery, edit/undo branches, source snapshots, disposable title/generate/compaction, explicit idle for waiting tools |
| `lib/sync.mjs` | `sync/{fingerprint,decide,catch-up,checkpoints,record}.ts`: canonical visible history fingerprints, grouped user inputs/tool media, safe branch boundary outside pending tools, checkpoint catch-up, interrupted-reply reconciliation, remapped checkpoints |
| `lib/messages.mjs` | `language-model/{convert-to-claude-message,system-prompt}.ts`: Claude's native system preset plus selected role/instructions, independent title prompt, prior-role text tags without reasoning, real image/PDF blocks |
| `lib/tools.mjs` | `tools/{names,pending,hooks,server,result}.ts`: exact PreToolUse IDs, sequential non-read-only MCP tools, bounded names/descriptions/subagent-list preservation, complete tool media via PostToolUse replacement, error/aborted flags, pending handlers released only after drain |
| `lib/stream.mjs` | `language-model/{convert-claude-stream,usage,claude-agent-error}.ts`: LMV3 streaming/reasoning/metadata, hidden unoffered tools, tool-step finish, deduplicated per-response usage/context occupancy, native max-token finish and terminal error diagnostics without outer retries |
| `bridge.mjs`, `lib/{protocol,primitives,records}.mjs` | OpenProxy-specific bounded NDJSON/process/cache lifecycle in place of Effect/OpenCode-private framework/storage; no inference/tool execution added |
| `test/{fake-claude,fake-cli,runtime.test}.mjs` | Minimal native-transport tests derived from upstream `lib/fake-claude.ts`, session/tools/branches/probe/sync tests and recorded protocol shapes. The real SDK is not mocked. |

## Deliberate envelope differences

- Node/Promises/AbortSignal replace upstream Effect/OpenCode-private services;
  there is no separate OpenCode framework dependency or event/history database.
- The parent is an authenticated remote OpenProxy transport, not the local
  provider plugin. It supplies already-key-scoped client session/source IDs,
  role/instructions, tool metadata and explicit generation budgets. It also
  disables outer client retries and automatic primary max-output budgets.
- Per the frozen OpenProxy objective, only one parent-supplied personal native
  profile is supported. The upstream plugin's API-key/cloud/configured HTTP
  overrides are intentionally not exposed; ambient alternatives are cleared.
  No API billing fallback, custom credentials, token handling or direct HTTP is
  introduced. Native account fields, not an assumed zero price, establish login
  provenance; catalog/cost persistence belongs to the Rust parent.
- Resource limits bound bridge/process/checkpoint ownership. Cache eviction
  falls back to the client's full history, never an invented transcript. Startup
  inventory is exposed using valid LMV3 finish provider metadata; explicit
  probes retain the agreed top-level inventory result format.
- The runtime-info note explicitly says the server directory is not the client
  workspace. Native code/settings/tools remain otherwise isolated as in source.
- No OpenCode attach/detach service handover timer is needed inside a single
  supervised bridge process; process shutdown drains the registry immediately.

The official dependencies are distributed under their own published terms.
SDK package `@anthropic-ai/claude-agent-sdk@0.3.301` declares
`claudeCodeVersion: 2.1.301`; its optional bundled binaries are not installed by
the documented `--no-optional` install and are never implicitly executed.
`@modelcontextprotocol/server@2.3.1` matches the pinned source baseline. The pnpm
lock records the complete resolved dependency/integrity graph.

An implementation using the SDK/original binary does **not** establish that
Anthropic separately approved OpenProxy or that any approval transfers from
OpenCode. Owner-assisted real native login/inference remains a separate gate.
