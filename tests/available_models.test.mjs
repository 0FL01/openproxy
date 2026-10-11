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
