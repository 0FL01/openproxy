import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { test } from "node:test";
import ts from "../web/node_modules/typescript/lib/typescript.js";

const sourceURL = new URL("../web/src/shared/utils/claudeNativeLogin.ts", import.meta.url);
const source = await readFile(sourceURL, "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } });
const helpers = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
const { ClaudeNativeLogin, nativeLoginLinks, NATIVE_LOGIN_POLL_MS, NATIVE_LOGIN_OUTPUT_LIMIT } = helpers;
const flush = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
const reply = (data, status = 200) => Response.json(data, { status });

function fixture(t, responses) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const calls = [], states = [], successes = [];
  const login = new ClaudeNativeLogin({
    onState: state => states.push(state), onSuccess: id => { successes.push(id); },
    request: async (path, options = {}) => {
      calls.push({ path, options, body: options.body ? JSON.parse(options.body) : undefined });
      if (options.method === "DELETE") return new Response(null, { status: 204 });
      if (path.endsWith("/input")) return new Response(null, { status: 204 });
      return await responses(path, options);
    },
  });
  t.after(() => login.close());
  return { login, calls, states, successes };
}

test("dashboard relays raw input/newline, polls at 500ms and trusts only native status", async (t) => {
  let polls = 0;
  const state = fixture(t, (path, options) => options.method === "POST" ? reply({ attemptId: "attempt-1" })
    : reply(++polls === 1 ? { status: "pending", output: "Login successful! (only terminal text)\nPaste code:" }
      : { status: "success", output: "Verified native status", connectionId: "external-1" }));
  await state.login.start("  Personal Claude  ");
  assert.deepEqual(state.calls[0].body, { name: "Personal Claude" });
  assert.equal(state.calls[0].path, "/api/claude-agent/login");
  assert.equal(state.states.at(-1).status, "pending");
  assert.deepEqual(state.successes, [], "never infer success from output text");
  assert.equal(await state.login.input("browser-returned-code#native-state\n"), true);
  assert.deepEqual(state.calls.at(-1).body, { input: "browser-returned-code#native-state\n" });
  assert.equal(state.calls.at(-1).path, "/api/claude-agent/login/attempt-1/input");
  assert.equal(await state.login.input("\n"), true);
  assert.deepEqual(state.calls.at(-1).body, { input: "\n" });
  assert.equal(NATIVE_LOGIN_POLL_MS, 500);
  t.mock.timers.tick(499); await flush(); assert.equal(polls, 1);
  t.mock.timers.tick(1); await flush(); assert.equal(polls, 2);
  assert.deepEqual(state.successes, ["external-1"]);
  assert.equal(state.states.at(-1).status, "success");
  state.login.close(); await flush();
  assert.ok(!state.calls.some(call => call.options.method === "DELETE"), "completed login must never be cancelled on close");
  t.mock.timers.tick(5000); await flush(); assert.equal(polls, 2);
  assert.ok(state.calls.every(call => call.options.credentials === "same-origin" && call.options.redirect === "error"));
});

test("close/unmount cancels the initiating attempt, aborts polling and ignores a stale completion", async (t) => {
  const pending = deferred();
  const state = fixture(t, (path, options) => options.method === "POST" ? reply({ attemptId: "first" }) : pending.promise);
  const started = state.login.start("");
  await flush();
  assert.deepEqual(state.calls[0].body, {});
  state.login.close(); state.login.close();
  assert.equal(state.calls[1].options.signal.aborted, true);
  pending.resolve(reply({ status: "success", output: "late", connectionId: "late-connection" }));
  await started;
  assert.deepEqual(state.successes, []);
  assert.equal(state.calls.filter(call => call.options.method === "DELETE").length, 1);
  assert.equal(state.calls.at(-1).path, "/api/claude-agent/login/first");
  assert.equal(state.calls.at(-1).options.keepalive, true);
  const count = state.states.length;
  t.mock.timers.tick(5000); await flush(); assert.equal(state.states.length, count);
});

test("a delayed create is cancelled by its own ID, never a completed later login", async (t) => {
  const pending = deferred(); let creates = 0;
  const state = fixture(t, (path, options) => options.method === "POST"
    ? ++creates === 1 ? pending.promise : reply({ attemptId: "new" })
    : reply({ status: "success", output: "new completed", connectionId: "new-connection" }));
  const old = state.login.start("old");
  await state.login.start("new");
  pending.resolve(reply({ attemptId: "old/encoded" }));
  await old;
  assert.deepEqual(state.successes, ["new-connection"]);
  assert.equal(state.states.at(-1).output, "new completed");
  const cancelled = state.calls.filter(call => call.options.method === "DELETE");
  assert.deepEqual(cancelled.map(call => call.path), ["/api/claude-agent/login/old%2Fencoded"]);
  state.login.close(); await flush();
  assert.equal(state.calls.filter(call => call.options.method === "DELETE").length, 1);
});

test("transcripts stay bounded, terminal errors stop polling and malformed replies cancel pending login", async (t) => {
  let response = { status: "error", output: "x".repeat(NATIVE_LOGIN_OUTPUT_LIMIT + 99), error: "Native verification failed" };
  const state = fixture(t, (_path, options) => options.method === "POST" ? reply({ attemptId: "bounded" }) : reply(response));
  await state.login.start("");
  assert.equal(state.states.at(-1).output.length, NATIVE_LOGIN_OUTPUT_LIMIT);
  assert.equal(state.states.at(-1).truncated, true);
  assert.equal(state.states.at(-1).error, "Native verification failed");
  state.login.close(); await flush();
  assert.ok(!state.calls.some(call => call.options.method === "DELETE"));
  response = { status: "logged-in", output: "not a supported status" };
  await state.login.start(""); await flush();
  assert.equal(state.states.at(-1).status, "error");
  assert.match(state.states.at(-1).error, /Invalid native login status/);
  assert.equal(state.calls.filter(call => call.options.method === "DELETE").length, 1);
  const count = state.calls.length;
  t.mock.timers.tick(5000); await flush(); assert.equal(state.calls.length, count);
});

test("only exact native HTTPS URLs without credentials become open/copy links", () => {
  const native = "https://claude.ai/oauth/authorize?client_id=native&redirect_uri=http%3A%2F%2Flocalhost%3A6000&state=unchanged#fragment";
  assert.deepEqual(nativeLoginLinks([
    "https://user:password@evil.invalid", "http://insecure.invalid", "javascript:alert(1)",
    native, native, "https://console.anthropic.com/native", "<script>bad()</script>",
  ].join("\n")), [native, "https://console.anthropic.com/native"]);
});

const require = createRequire(new URL("../web/package.json", import.meta.url));
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");

async function moduleFromTS(path, imports) {
  const source = await readFile(new URL(`../web/src/${path}`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, { compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
  } });
  const exports = {};
  new Function("exports", "require", outputText)(exports, id => {
    if (Object.hasOwn(imports, id)) return imports[id];
    if (["react", "react/jsx-runtime"].includes(id)) return require(id);
    throw new Error(`Unexpected import: ${id}`);
  });
  return exports;
}

test("native modal renders escaped terminal text, explicit raw-input help and HTTPS links (SSR fixture)", async () => {
  const Button = props => React.createElement("button", { type: props.type, disabled: props.disabled }, props.children);
  const modal = await moduleFromTS("shared/components/Modal.tsx", {
    "@/shared/utils/cn": { cn: (...parts) => parts.filter(Boolean).join(" ") }, "./Button": { default: Button },
  });
  const malicious = '<img src=x onerror="alert(1)">\nhttps://claude.ai/native?state=keep%26unchanged';
  const component = await moduleFromTS("components/providers/ClaudeNativeLoginModal.tsx", {
    react: { ...React, useState: initial => React.useState(initial === helpers.nativeLoginInitialState
      ? () => ({ status: "pending", output: malicious, truncated: false }) : initial) },
    "@/shared/components/Modal": modal,
    "@/shared/components/Button": { default: Button },
    "@/shared/utils/claudeNativeLogin": helpers,
  });
  const html = renderToStaticMarkup(React.createElement(component.default, { isOpen: true, onClose() {}, onSuccess() {} }));
  assert.match(html, /role="dialog" aria-modal="true" aria-label="Connect Claude Agent"/);
  assert.match(html, /role="log"/);
  assert.match(html, /&lt;img src=x onerror=&quot;alert\(1\)&quot;&gt;/);
  assert.doesNotMatch(html, /<img|type="password"|dangerouslySetInnerHTML/);
  assert.match(html, /href="https:\/\/claude.ai\/native\?state=keep%26unchanged"/);
  assert.match(html, /rel="noopener noreferrer" referrerPolicy="no-referrer"/i);
  assert.match(html, /Only when the native process asks/);
  assert.match(html, /type="submit"/);
  assert.match(html, /No Claude installation is needed/);
});

test("served provider registry retires the old alias and presents native external auth independently of Anthropic", async () => {
  const { AI_PROVIDERS, OAUTH_PROVIDERS, EXTERNAL_PROVIDERS, APIKEY_PROVIDERS, getProviderAlias, USAGE_SUPPORTED_PROVIDERS } = await moduleFromTS("shared/constants/providers.ts", {});
  assert.equal(AI_PROVIDERS.claude, undefined);
  assert.equal(OAUTH_PROVIDERS["claude-agent"], undefined);
  assert.equal(EXTERNAL_PROVIDERS["claude-agent"].authType, "external");
  assert.equal(EXTERNAL_PROVIDERS["claude-agent"].name, "Claude Agent");
  assert.equal(getProviderAlias("claude-agent"), "claude-agent");
  assert.ok(!Object.values(AI_PROVIDERS).some(provider => provider.alias === "cc"));
  assert.ok(APIKEY_PROVIDERS.anthropic);
  assert.ok(!USAGE_SUPPORTED_PROVIDERS.includes("claude"));
  assert.ok(!USAGE_SUPPORTED_PROVIDERS.includes("claude-agent"), "do not invent undocumented subscription quota polling");
  const detail = await readFile(new URL("../web/src/components/providers/ProviderDetailPageClient.tsx", import.meta.url), "utf8");
  assert.match(detail, /setShowNativeLoginModal\(true\)/);
  assert.match(detail, /onSuccess=\{handleNativeSuccess\}/);
  assert.match(detail, /await reloadProviderCatalog\(\);\s+am\.refresh\(\);/);
});
