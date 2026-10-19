import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "../web/node_modules/typescript/lib/typescript.js";
import { create } from "../web/node_modules/zustand/esm/index.mjs";

async function loadTs(path, imports = {}) {
  const source = await readFile(new URL(`../web/src/${path}`, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  });
  const exports = {};
  new Function("exports", "require", outputText)(exports, (id) => {
    assert.ok(Object.hasOwn(imports, id), `Unexpected import: ${id}`);
    return imports[id];
  });
  return exports;
}

const loadStore = () => loadTs("store/catalogStore.ts", { zustand: { create }, react: { useEffect() {} } });
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};
const response = (id, xaiCatalog = { status: "ready" }) => ({
  ok: true,
  json: async () => ({
    providerIdToAlias: { xai: "xai", codex: "cx" },
    providerModels: [{ alias: "xai", models: [{ id, kind: "llm" }] }],
    xaiCatalog,
  }),
});

test("explicit catalog reload coalesces a follow-up and every waiter includes it", async (t) => {
  const first = deferred();
  const second = deferred();
  const requests = [];
  t.mock.method(globalThis, "fetch", (url) => {
    requests.push(url);
    return requests.length === 1 ? first.promise : second.promise;
  });
  const { useCatalogStore: store } = await loadStore();
  const initial = store.getState().load();
  assert.equal(store.getState().loading, true);
  assert.equal(store.getState().load(), initial, "initial loads join without queuing");
  // Also exercise invalidation before the first fetch's microtask starts.
  const reload = store.getState().reload();
  assert.equal(reload, initial);
  assert.equal(store.getState().reload(), initial);
  await Promise.resolve();
  assert.equal(requests.length, 1);
  assert.equal(store.getState().reload(), initial, "in-flight invalidation joins the same awaited chain too");
  first.resolve(response("old"));
  await new Promise(setImmediate);
  assert.equal(requests.length, 2, "multiple invalidations coalesce to one follow-up");
  assert.equal(store.getState().loading, true);
  let settled = false;
  reload.then(() => { settled = true; });
  await Promise.resolve();
  assert.equal(settled, false, "await must include the follow-up snapshot");
  second.resolve(response("latest", { status: "stale", warning: "temporary failure" }));
  await Promise.all([initial, reload]);
  assert.equal(store.getState().modelsByAlias.xai[0].id, "latest");
  assert.deepEqual(store.getState().xaiCatalog, { status: "stale", warning: "temporary failure" });
  assert.equal(store.getState().loading, false);
  assert.equal(store.getState().error, null);
  await store.getState().load();
  assert.equal(requests.length, 2);
});

test("catalog failures are observable, preserve last-good, and initial loads never reject", async (t) => {
  t.mock.method(globalThis, "fetch", async () => { throw new Error("offline"); });
  const { useCatalogStore: store } = await loadStore();
  await assert.doesNotReject(store.getState().load());
  assert.equal(store.getState().loaded, false);
  assert.equal(store.getState().loading, false);
  assert.equal(store.getState().error, "offline");

  globalThis.fetch = async () => response("good");
  await store.getState().reload();
  const lastGood = store.getState().modelsByAlias;
  globalThis.fetch = async () => ({ ok: false, status: 503 });
  await assert.doesNotReject(store.getState().reload());
  assert.equal(store.getState().modelsByAlias, lastGood);
  assert.match(store.getState().error, /503/);

  globalThis.fetch = async () => ({ ok: true, json: async () => ({
    providerModels: [{ alias: "xai", models: [] }, { alias: "cx", models: [{ id: "other", kind: "llm" }] }],
  }) });
  await store.getState().reload();
  assert.equal(store.getState().error, null);
  assert.equal(store.getState().xaiCatalog, undefined, "no active source may omit diagnostics");
  assert.deepEqual(store.getState().modelsByAlias.xai, [], "empty language inventory is valid");
  assert.equal(store.getState().modelsByAlias.cx[0].id, "other");
});

const xaiMetadata = await loadTs("shared/utils/xaiCatalog.ts");
const thinking = await loadTs("shared/utils/thinkingLevels.ts", { "./xaiCatalog": xaiMetadata });
const models = Object.freeze([
  Object.freeze({ id: "grok-vision", kind: "llm", capabilities: ["vision"], reasoningEfforts: null }),
  Object.freeze({ id: "grok-reasoning", kind: "llm", capabilities: ["reasoning"], reasoningEfforts: ["low", "high"] }),
  Object.freeze({ id: "grok-none", kind: "llm", capabilities: [], reasoningEfforts: ["none"] }),
  Object.freeze({ id: "grok-unknown-efforts", kind: "llm", capabilities: ["reasoning"], reasoningEfforts: null }),
]);

test("xAI thinking uses exact declared efforts, retains none, and never guesses for missing rows", () => {
  assert.deepEqual(thinking.getThinkingLevels("xai", "grok-reasoning", models), ["low", "high"]);
  assert.deepEqual(thinking.getThinkingLevels("xai", "xai/grok-none", models), ["none"]);
  assert.equal(thinking.getThinkingLevels("xai", "grok-vision", models), null);
  assert.equal(thinking.getThinkingLevels("xai", "grok-unknown-efforts", models), null);
  assert.equal(thinking.getThinkingLevels("xai", "grok-4-custom", models), null);
  assert.equal(thinking.getThinkingCapsForModel("xai", "grok-4-custom", models).reasoning, false);
  assert.deepEqual(thinking.unionThinkingLevels("xai", models.map(row => row.id), models), ["auto", "none", "low", "high"]);
  assert.deepEqual(thinking.getThinkingLevels("openrouter", "grok-4-custom", models), ["none", "minimal", "low", "medium", "high", "xhigh"], "other-provider heuristics stay unchanged");
});

test("reactive xAI caps beat both exact API collisions and bare-ID/Grok heuristics", async () => {
  let current = { modelsByAlias: { xai: [] } };
  let stateIndex = 0;
  const { useModelCaps } = await loadTs("shared/hooks/useModelCaps.ts", {
    react: {
      useState: () => [stateIndex++ % 2 ? { "grok-vision": { reasoning: true, vision: true } } : { "xai/grok-vision": { reasoning: true } }, () => {}],
      useEffect() {}, useCallback: callback => callback,
    },
    "@/store/catalogStore": { useCatalogStore: selector => selector(current) },
    "@/shared/utils/xaiCatalog": xaiMetadata,
  });
  assert.deepEqual(useModelCaps().getCaps("xai/grok-vision"), {}, "absent is unknown before catalog arrives");
  current = { modelsByAlias: { xai: models } };
  const { getCaps } = useModelCaps();
  assert.deepEqual(getCaps("xai/grok-vision"), { vision: true });
  assert.deepEqual(getCaps("xai/grok-reasoning"), { reasoning: true });
  assert.deepEqual(getCaps("xai/grok-4-custom"), {});
  assert.deepEqual(getCaps("openrouter/grok-4-custom"), { vision: true, reasoning: true });
  current = { modelsByAlias: { xai: [] } };
  assert.deepEqual(useModelCaps().getCaps("xai/grok-vision"), {}, "removal is reactive too");
});
