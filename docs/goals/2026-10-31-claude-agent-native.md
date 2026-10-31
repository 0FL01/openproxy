# Goal: Claude Agent native subscription

Status: active
Source: user-approved RECON plan; anomalyco/opencode PR #58782 at f29e281322b35b525e337d12e84aedc7e129fd6d; user amendments: remove the mask completely, offer native login from the dashboard, fully preserve PR/native behavior, implement iteratively, commit/push/build/deploy production, then merge the current branch into main and push.
Last updated: 2026-10-31

## Objective

Use the owner's Claude Pro subscription from OpenCode through OpenProxy. An original installed Claude Code, driven by the official Agent SDK on the OpenProxy server, owns Anthropic authentication and inference. The dashboard relays its native login. OpenCode owns tools, permissions and authoritative conversation history. Replace, do not retain, the old subscription OAuth/masking path.

## Execution Directive

Complete the frozen Required Outcomes using the listed Change Envelope and Primary Evidence. Work on the smallest unresolved outcome. Do not add requirements from reviews, tests, tools, speculative risks, or optional source text. Finish when every required outcome is resolved and affected constraints remain satisfied. Stop substantive work at a proven external blocker, an approved budget boundary, or when no remaining in-scope action has a falsifiable expected result; record the exact evidence and smallest unlock.

## Frozen Contract

### Required Outcomes

- R1: Retire the old Claude subscription integration completely.
  - Source: user: breaking change; remove the deprecated Claude contract at the root; delete openproxy-claude-mask.js completely.
  - Acceptance: no operational claude/cc OAuth, import-token, refresh, C46 identity/system/tool-name masking, mask discovery/inference gate, V1 mask plugin or its obsolete tests/instructions. Legacy stored values remain preserved but inert, never silently converted. Anthropic API-key and shared Messages support remain.
  - Primary evidence: targeted retirement/API tests plus affected library tests.
  - Status: pending
  - Evidence:
- R2: Dashboard account addition relays original native login.
  - Source: user: authorization through the web interface when adding a subscription account; full native forwarding, not our OAuth implementation.
  - Acceptance: Add Claude Agent starts only the fixed original claude auth login process; dashboard displays its output and forwards interactive input, including a browser-returned code when native login requests it. Native Claude Code constructs the URL, exchanges/refreshes credentials and stores them in the persistent native profile. App records only external connection and safe metadata after native status/SDK probe succeed. No passwords or Anthropic tokens in app DB or logs.
  - Primary evidence: fake native process integration and dashboard test; real binary login is verified separately under R7.
  - Status: pending
  - Evidence:
- R3: Server runtime preserves the supported behavior of PR #58782.
  - Source: user: fully repeat PR/Claude Code implementation; approved final amendment withdraws persistSession:false for chats.
  - Acceptance: original installed binary through public SDK; native base prompt plus client role/instructions; only OpenCode tools through MCP, native built-in tools/config/hooks/memory/compaction disabled as in PR; fail closed on MCP failure; correct tool IDs/results/media, sequential tools, interruption/drain, persistent chat sessions, resume/fork/checkpoints, partial reply continuation, separate title/generate/compaction calls, model/effort selection, deduplicated usage and native retries without a second generation retry loop. Five-minute idle never interrupts an executing client tool. Only behavior supported by the PR is required, not every standalone Claude Code feature.
  - Primary evidence: actual SDK with a fake claude transport exercising tool continuation, cancellation, resume/fork and side-call isolation.
  - Status: pending
  - Evidence:
- R4: Existing provider/model controls and OpenCode V2 aggregate discovery work with the native runtime.
  - Source: approved plan and repository core product surfaces.
  - Acceptance: new provider/source claude-agent; native inventory and saved user custom/enabled/disabled models feed Available Models and /v1/models. Existing ludka2 V2 plugin selects a locally defined remote adapter only for canonical claude-agent source. No client-side Claude install/login needed, no arbitrary SDK/URL/header overrides from the catalog, unrelated providers unchanged. OpenCode executes tools/permissions and reports terminal execution events to arm idle.
  - Primary evidence: focused model API/plugin tests and isolated client/transport smoke.
  - Status: pending
  - Evidence:
- R5: Package and deploy the verified implementation to the existing production Compose service.
  - Source: user: build and deploy production.
  - Acceptance: image contains Node, locked SDK/runtime and unmodified published Claude binary, retains existing data/encryption/config and native profile across recreation, passes isolated smoke before replacement; production health and deployed revision checked; previous image retained for rollback. No volume deletion or credential/config reset.
  - Primary evidence: Docker build, isolated container smoke and production image/revision/health observation.
  - Status: pending
  - Evidence:
- R6: Commit and push verified iterations, then incorporate the current branch in main and push.
  - Source: latest user instruction.
  - Acceptance: intended conventional commits on origin/feat/dioxus-dashboard, existing branch work retained, main includes this branch without force-push or discarded commits, origin/main SHA confirmed. Existing unfinished Dioxus work is merged as source, not falsely declared complete or switched into production packaging.
  - Primary evidence: clean status, reviewed staged diffs, remote SHAs and merge ancestry.
  - Status: in_progress
  - Evidence: baseline is clean feat/dioxus-dashboard at 5f7660b1851b5674f49602a6b23c0efa1bf0b298; origin default is main at 82c48877fb2e4d802c86e68f3d5603d80b480f86.
- R7: Verify the owner's real subscription flow end to end.
  - Source: approved finish-line smoke: native personal login, generation, tool call, Stop, parallel sessions and persistence.
  - Acceptance: after the owner completes native browser sign-in, OpenCode can generate and execute a permitted client tool through the deployed proxy; subscription identity comes from native status/account metadata, not an assumed zero price.
  - Primary evidence: explicit owner-assisted native login and one small real request/tool smoke.
  - Status: pending
  - Evidence: production currently has no Claude connection; no personal native credentials inspected or used.

### Constraints

- C1: No token import/export, credential-file parsing, direct subscription HTTP executor, client identity spoofing or fabricated system identity. Claude Code alone owns its credentials and outbound Anthropic requests.
- C2: Keep Anthropic API-key execution, Messages translators, Anthropic-compatible nodes and unrelated providers intact. All retained v1 fields preserve their semantics; declare the Claude-specific hard cut and use v2 for the new runtime transport.
- C3: OpenCode remains source of truth for history and owns compaction, general tools and permissions. Native transcript/checkpoint state is a provider-scoped exception, not a general OpenProxy harness.
- C4: Authenticate before body parsing; bound request/event queues and native processes; isolate sessions by authenticated key plus client session; do not log prompts, tool content, auth URLs/codes, credential data or terminal transcripts. Cancellation and shutdown drain/reap owned children.
- C5: Preserve production volume, SQLite data, stable deployment secrets and existing settings. Do not read .env.prod, dump live DB/credentials or use production data for tests. Production deployment is now explicitly authorized, superseding the earlier Dioxus goal's no-deploy instruction only for this objective.
- C6: Stage only intended files, inspect status/staged diff and secret handling before each commit. No user/client config edits. Pushes are authorized; no force-push or destructive branch reset.
- C7: Native binary/SDK usage is not proof that Anthropic separately approved OpenProxy. Do not claim transferable approval from OpenCode.

### Non-goals

- Arbitrary-client OpenAI compatibility for the new native runtime; first supported client is OpenCode V2 with the checked-in plugin.
- General web shell, server-side execution of client tools, subscription pools/resale, automatic fallback to paid API billing, undocumented quota polling.
- Completing/cutting over the separate unfinished Dioxus dashboard objective or fixing neighboring provider issues.

## Change Envelope / Approved Plan

1. Update contracts/lean-proxy.md/json, docs/ARCHITECTURE.md and docs/CHECKPOINTS.json: retire C46, declare claude-agent native-session exception, keep legacy data inert and new transport v2.
2. Delete old Claude OAuth/import/refresh and identity/tool/system masking from oauth modules, chat/default executor/claude_format, catalogs, CLI examples, dashboard and V1 plugin/tests/docs. Retain shared Anthropic protocol and API-key path.
3. Add runtime/claude-agent with a locked official SDK and source-attributed PR session/MCP/stream/history behavior. Explicitly select the installed original binary. Native profile is $DATA_DIR/claude-agent. Clear ambient API-key/cloud overrides for this personal subscription profile without implementing credentials. Persist chat sessions; side calls are disposable, as upstream.
4. Add a lazy Rust supervisor in src/server/claude_agent.rs and native routes in src/server/api/claude_agent.rs, state/shutdown integration and bounded JSONL stdio. No extra public port or Compose service. Reuse existing admission/body limits. Per-dialog serialization and an eight-live-Claude-process cap; startup 30 seconds, idle five minutes, tool waiting exempt until client execution ends. No second retry/fallback loop.
5. Extend plugins/openproxy-models.js for the fixed remote LanguageModelV3 adapter and native session hooks. Keep canonical-source selection and metadata whitelist; preserve kind/session/fork/role/instructions, tool errors/aborts, attachments, cancellation and idle signals. Client tools run on the client; never represent the server cwd as the client workspace.
6. Dashboard ConnectionsCard + ClaudeNativeLoginModal relay native auth, not OAuthModal/import-token. Persist external connection and safe probe inventory, reuse Available Models, v1_models and existing user overlays. Update directly affected retired-provider consumers in the inactive Dioxus source without its packaging cutover.
7. Dockerfile, relevant CI checks, README and plugins/README package/document the runtime and profile, native web login, client setup, supported limitations and rollback. The existing production Astro build remains the served frontend.

### Transport agreed for independent implementation

- Internal stdin commands: {op:"probe"|"run"|"idle"|"cancel"|"shutdown", id, ...}. Run includes request object below; outputs {id,type:"part",part}, {id,type:"result",result}, {id,type:"error",error:{message,code?}}, {id,type:"done"}. Commands can arrive while a run awaits tools. All output is bounded/framed; stdout is protocol only.
- POST /v2/claude-agent/stream accepts {model,kind:"primary"|"title"|"generate"|"compaction",sessionID?,sourceID?,system:{agent,instructions},prompt,tools,effort?,maxOutputTokens?}. prompt/tools use JSON-serializable LanguageModelV3 shapes, binary attachments base64. Returns application/x-ndjson LanguageModelV3 stream parts. Server validates routing and replaces client session/source IDs with authenticated-key-scoped hashes before the runtime sees them. Client abort cancels; normal tool-step EOF must not cancel parked MCP calls.
- POST /v2/claude-agent/idle accepts {sessionID}; same key-scoped ownership, no generation admission wait for control-plane cleanup.
- Dashboard POST /api/claude-agent/login accepts {name?} and returns {attemptId}. GET /api/claude-agent/login/{attemptId} returns {status:"pending"|"success"|"error"|"cancelled",output,error?,connectionId?}. POST .../{attemptId}/input accepts {input} (raw native input, caller includes newline). DELETE .../{attemptId} cancels. Polling native output is a UI transport only, not an OAuth state machine. Bind attempts to initiating dashboard/management identity; one active login per profile, bounded output/deadline, same-origin checks, no URL credentials. Credentials stay solely in the native profile.

## Current Checkpoint

- Closes: R1/R2/R3/R4 foundation.
- Smallest next action: commit this frozen goal; implement independent native SDK runtime and old-path retirement/client UI while integrating the Rust supervisor and native-login transport.
- Expected evidence: focused SDK/retirement/auth/model/plugin tests before the first runtime commit.
- Stop or replan if: a required native behavior is unavailable in the official SDK/binary, the full PR behavior cannot be preserved by the transport, or a destructive/credential-changing action beyond this approved envelope is required. Record the exact blocker, not a substitute feature.

## Current State

- Resolved: read-only RECON; user authorized implementation, pushes, production deployment and final main merge. Origin main identified; working tree clean.
- Last relevant evidence: production healthy at image sha256:4807bf3a50e8d6d9f11e4cbf71bf933724e91fce74abbe829b1daa4317380b56 with revision 5f7660b1. Installed host Claude Code 2.1.300 exposes native auth login/status, but no host profile was opened. Production connections are codex/glm/opencode-go/xai, not Claude.
- Blocker: none for implementation. R7 will require the owner's interactive browser sign-in; do not substitute unrelated host credentials.
- Next: implementation checkpoint above; isolated DATA_DIR and port 4625/test container only until deployment gates pass.

## Material Decisions

- 2026-10-31: user requires complete old-path removal, not a compatibility shim. Native server login and unchanged Claude Code outbound behavior replace token-based impersonation.
- 2026-10-31: native persistence/resume/fork are required; earlier persistSession:false-for-chats proposal is superseded. Browser dashboard is an I/O wrapper around native login; the upstream PR itself uses a terminal external-login method.
- 2026-10-31: preserve the current feature branch and merge its existing work into main after verified deployment. Do not complete the unrelated Dioxus migration or silently change the production frontend.

## Checkpoint History

- 2026-10-31: goal frozen from the approved plan and amendments; old OAuth/mimicry goal superseded. No implementation or tests yet.

## Completion

- Resolved outcomes: pending.
- Commands and artifacts: pending.
- Constraint and diff-scope check: pending.
- Final status: active.
