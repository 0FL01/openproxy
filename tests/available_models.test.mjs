import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "../web/node_modules/typescript/lib/typescript.js";

const customSource = await readFile(new URL("../web/src/shared/utils/providerCustomModels.ts", import.meta.url), "utf8");
const { outputText: customOutputText } = ts.transpileModule(customSource, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
});
const providerCustomModels = await import(`data:text/javascript;base64,${Buffer.from(customOutputText).toString("base64")}`);

const source = await readFile(new URL("../web/src/shared/models/availableModels.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
});
const imports = {
  react: {},
  "@/shared/constants/models": {},
  "@/store/catalogStore": {},
  "@/shared/constants/providers": {},
  "@/shared/utils/providerCustomModels": providerCustomModels,
  "@/shared/utils/providerModelsFetcher": {},
};
const availableModels = {};
new Function("exports", "require", outputText)(availableModels, (id) => {
  assert.ok(Object.hasOwn(imports, id), `Unexpected import: ${id}`);
  return imports[id];
});
const { buildAvailableModels } = availableModels;

test("a custom Fast name survives the model joining the catalog", () => {
  const customModels = [Object.freeze({
    providerAlias: "cx",
    id: "gpt-6.1-sol-fast",
    name: "My Fast",
  })];
  const input = {
    providerAlias: "cx",
    customModels,
    catalogModels: [{ id: "gpt-6.1-sol", name: "GPT-6.1-Sol", type: "llm" }],
  };
  const before = buildAvailableModels(input);
  const after = buildAvailableModels({
    ...input,
    catalogModels: [
      ...input.catalogModels,
      { id: "gpt-6.1-sol-fast", name: "GPT-6.1-Sol-Fast", type: "llm" },
    ],
  });

  const beforeFast = before.allRows.filter((row) => row.id === "gpt-6.1-sol-fast");
  const afterFast = after.allRows.filter((row) => row.id === "gpt-6.1-sol-fast");
  assert.equal(beforeFast.length, 1);
  assert.equal(afterFast.length, 1);
  assert.equal(beforeFast[0].name, "My Fast");
  assert.equal(afterFast[0].name, beforeFast[0].name);
  assert.equal(beforeFast[0].fullModel, "cx/gpt-6.1-sol-fast");
  assert.equal(afterFast[0].fullModel, beforeFast[0].fullModel);
  assert.equal(beforeFast[0].source, "custom");
  assert.equal(afterFast[0].source, "catalog");
  assert.equal(before.allRows.find((row) => row.id === "gpt-6.1-sol").name, "GPT-6.1-Sol");
  assert.deepEqual(
    after.allRows.find((row) => row.id === "gpt-6.1-sol"),
    before.allRows.find((row) => row.id === "gpt-6.1-sol"),
  );
});

test("disabled custom rows stay restorable across catalog arrival and removal", () => {
  const custom = Object.freeze({ providerAlias: "xai", id: "grok-user", name: "My Grok", kind: "llm" });
  const customModels = Object.freeze([custom]);
  const aliases = Object.freeze({ "my-route": "xai/grok-user" });
  const disabledIds = Object.freeze(["grok-user"]);
  const input = { providerAlias: "xai", customModels, modelAliases: aliases, disabledIds };
  const catalog = Object.freeze([Object.freeze({
    id: "grok-user", name: "Official name", kind: "llm", contextWindow: 256000,
  })]);
  const snapshot = JSON.stringify({ input, catalog });

  for (const catalogModels of [[], catalog, []]) {
    const result = buildAvailableModels({ ...input, catalogModels });
    assert.equal(result.enabledRows.length, 0, "disabled custom must never appear active");
    assert.equal(result.customRows.length, 0);
    assert.equal(result.allRows.length, 1, "overlap must be deduplicated");
    assert.equal(result.disabledRows.length, 1, "disabled custom must not disappear");
    assert.equal(result.disabledRows[0].name, "My Grok");
    assert.equal(result.disabledRows[0].fullModel, "xai/grok-user");
    assert.equal(result.disabledRows[0].disabled, true);
    assert.equal(result.disabledRows[0].contextWindow, catalogModels.length ? 256000 : undefined);
  }
  assert.equal(JSON.stringify({ input, catalog }), snapshot, "saved configuration is immutable");

  const restored = buildAvailableModels({ ...input, catalogModels: catalog, disabledIds: [] });
  assert.equal(restored.enabledRows.length, 1);
  assert.equal(restored.enabledRows[0].name, "My Grok");
  assert.equal(restored.disabledRows.length, 0);
});

test("custom and legacy IDs deduplicate against live/catalog rows without changing user data", () => {
  const customModels = Object.freeze([
    Object.freeze({ providerAlias: "xai", id: "grok-user", name: "User name" }),
    Object.freeze({ providerAlias: "xai", id: "grok-user", name: "Duplicate name" }),
    Object.freeze({ providerAlias: "xai", id: "grok-multi-agent-user", name: "Explicit agent" }),
    Object.freeze({ providerAlias: "xai", id: "media-only", kind: "image" }),
  ]);
  const aliases = Object.freeze({ personal: "xai/grok-user", legacy: "xai/grok-legacy" });
  const input = {
    providerAlias: "xai", customModels, modelAliases: aliases,
    catalogModels: [{ id: "grok-user", name: "Official", kind: "llm", contextWindow: null }],
    liveModels: [{ id: "grok-user" }, { id: "grok-legacy" }],
  };
  const snapshot = JSON.stringify(input);
  const result = buildAvailableModels(input);
  assert.equal(result.allRows.length, 3);
  assert.equal(new Set(result.allRows.map(row => row.id)).size, 3);
  assert.equal(result.enabledRows.find(row => row.id === "grok-user").name, "User name");
  assert.ok(result.enabledRows.find(row => row.id === "grok-multi-agent-user"), "explicit saved rows remain");
  assert.ok(result.allRows.every(row => row.contextWindow === undefined), "unknown context gets no default");
  assert.equal(JSON.stringify(input), snapshot);
});
